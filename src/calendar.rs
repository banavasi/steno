//! Day's-calendar picker via the fleet's `gcli` tool (subprocess-json) — no
//! OAuth surface of our own. Runs BEFORE the TUI on plain stdin/stdout.
//! Events come from ALL configured gcli profiles (personal/asu/oneorigin),
//! fetched concurrently and merged by start time. Each profile covers its
//! account's PRIMARY calendar — secondary calendars need a gcli extension.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::io::Write;
use std::process::Command;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalEvent {
    #[serde(default)]
    pub summary: String,
    pub start: Option<EventTime>,
    #[serde(default)]
    pub location: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventTime {
    pub date_time: Option<String>,
    pub date: Option<String>,
}

impl CalEvent {
    /// Sort key: parsed start (all-day events sort to start of day).
    fn start_key(&self) -> chrono::DateTime<chrono::Local> {
        let raw = self
            .start
            .as_ref()
            .and_then(|t| t.date_time.clone().or_else(|| t.date.clone()))
            .unwrap_or_default();
        chrono::DateTime::parse_from_rfc3339(&raw)
            .map(|dt| dt.with_timezone(&chrono::Local))
            .or_else(|_| {
                chrono::NaiveDate::parse_from_str(&raw, "%Y-%m-%d").map(|d| {
                    d.and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_local_timezone(chrono::Local)
                        .single()
                        .unwrap_or_else(chrono::Local::now)
                })
            })
            .unwrap_or_else(|_| chrono::Local::now())
    }

    fn time_display(&self) -> String {
        let all_day = self.start.as_ref().is_some_and(|t| t.date_time.is_none());
        if all_day {
            "all-day".into()
        } else {
            self.start_key().format("%H:%M").to_string()
        }
    }
}

fn gcli(args: &[&str]) -> Result<String> {
    let out = Command::new("gcli").args(args).output().context("run gcli")?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    // gcli's error envelope: {"error":{"message":...}} on non-zero exit
    if !out.status.success() {
        let msg = serde_json::from_str::<serde_json::Value>(&stdout)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
        bail!("{msg}");
    }
    Ok(stdout)
}

#[derive(Deserialize)]
struct Profile {
    name: String,
    #[serde(default)]
    default: bool,
}

/// Configured gcli profiles; the default one first.
fn profiles() -> Result<Vec<Profile>> {
    let out = gcli(&["profiles", "--json"])?;
    let mut ps: Vec<Profile> = serde_json::from_str(&out).context("parse gcli profiles")?;
    ps.sort_by_key(|p| !p.default);
    anyhow::ensure!(!ps.is_empty(), "gcli has no profiles configured");
    Ok(ps)
}

pub fn list_today(profile: &str) -> Result<Vec<CalEvent>> {
    let out = gcli(&["cal", "list", "--profile", profile, "--days", "1", "--json"])?;
    serde_json::from_str(&out).context("parse gcli cal list output")
}

/// Today's events across the given profiles, fetched concurrently.
/// Returns (profile, event) pairs sorted by start time + per-profile errors.
fn list_today_all(names: &[String]) -> (Vec<(String, CalEvent)>, Vec<String>) {
    let results: Vec<(String, Result<Vec<CalEvent>>)> = std::thread::scope(|s| {
        names
            .iter()
            .map(|name| {
                let name = name.clone();
                s.spawn(move || {
                    let r = list_today(&name);
                    (name, r)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().expect("calendar fetch thread"))
            .collect()
    });
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for (name, r) in results {
        match r {
            Ok(evs) => events.extend(evs.into_iter().map(|e| (name.clone(), e))),
            Err(e) => {
                // gcli errors embed multi-line JSON; keep the first + last lines
                let e = e.to_string();
                let mut lines = e.lines();
                let first = lines.next().unwrap_or("error");
                let fix = lines.last().filter(|l| l.contains("try `")).unwrap_or("");
                errors.push(format!("{name}: {first} {fix}").trim().to_string());
            }
        }
    }
    events.sort_by_key(|(_, e)| e.start_key());
    (events, errors)
}

pub fn create(profile: &str, title: &str) -> Result<()> {
    let start = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    gcli(&[
        "cal", "create", "--profile", profile, "--title", title, "--start", &start, "--json",
    ])?;
    Ok(())
}

fn prompt(msg: &str) -> String {
    print!("{msg}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    line.trim().to_string()
}

/// Launch-flow picker over ALL profiles' calendars (or one, if `only` is set):
/// pick an event / create / type a title. Degrades to a manual title whenever
/// gcli is unavailable (auth expiry, not installed).
pub fn pick_title(only: Option<&str>, default_title: &str) -> String {
    let (names, create_profile) = match only {
        Some(p) => (vec![p.to_string()], p.to_string()),
        None => match profiles() {
            Ok(ps) => {
                let create = ps.first().map(|p| p.name.clone()).unwrap_or_default();
                (ps.into_iter().map(|p| p.name).collect(), create)
            }
            Err(e) => {
                println!("calendar unavailable: {e}");
                let ans = prompt("meeting title (empty = untitled): ");
                return if ans.is_empty() { default_title.to_string() } else { ans };
            }
        },
    };

    let (events, errors) = list_today_all(&names);
    for err in &errors {
        println!("⚠ calendar '{err}");
    }

    if events.is_empty() {
        println!("no events today across: {}.", names.join(", "));
        let ans = prompt(&format!(
            "[c]reate an event (on '{create_profile}'), or type a title (empty = untitled): "
        ));
        return match ans.as_str() {
            "c" | "C" => create_flow(&create_profile, default_title),
            "" => default_title.to_string(),
            title => title.to_string(),
        };
    }

    println!("today across {}:", names.join(", "));
    for (i, (profile, e)) in events.iter().enumerate() {
        println!(
            "  [{}] {:7} ({profile})  {}  {}",
            i + 1,
            e.time_display(),
            e.summary,
            e.location
        );
    }
    loop {
        let ans = prompt(&format!(
            "pick event number, [c]reate (on '{create_profile}'), or title (empty = untitled): "
        ));
        if let Ok(n) = ans.parse::<usize>() {
            if let Some((_, e)) = events.get(n.wrapping_sub(1)) {
                return e.summary.clone();
            }
            println!("no event [{n}]");
            continue;
        }
        return match ans.as_str() {
            "c" | "C" => create_flow(&create_profile, default_title),
            "" => default_title.to_string(),
            title => title.to_string(),
        };
    }
}

fn create_flow(profile: &str, default_title: &str) -> String {
    let title = {
        let t = prompt("event title: ");
        if t.is_empty() { default_title.to_string() } else { t }
    };
    match create(profile, &title) {
        Ok(()) => println!("event created on '{profile}'."),
        Err(e) => println!("create failed ({e}); continuing without a calendar event."),
    }
    title
}
