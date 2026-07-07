//! Day's-calendar picker via the fleet's `gcli` tool (subprocess-json) — no
//! OAuth surface of our own. Runs BEFORE the TUI on plain stdin/stdout.
//! gcli Event shape: {summary, start: {dateTime|date}, location}; no event id
//! or description yet (extend gcli when points-to-discuss seeding lands).
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
    fn time_display(&self) -> String {
        let raw = self
            .start
            .as_ref()
            .and_then(|t| t.date_time.clone().or_else(|| t.date.clone()))
            .unwrap_or_default();
        chrono::DateTime::parse_from_rfc3339(&raw)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or(raw)
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

pub fn list_today(profile: &str) -> Result<Vec<CalEvent>> {
    let out = gcli(&["cal", "list", "--profile", profile, "--days", "1", "--json"])?;
    serde_json::from_str(&out).context("parse gcli cal list output")
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

/// Launch-flow picker: today's events → pick / create / manual title.
/// Degrades to a manual title on any gcli failure (auth expiry, no gcli).
pub fn pick_title(profile: &str, default_title: &str) -> String {
    match list_today(profile) {
        Ok(events) if !events.is_empty() => {
            println!("today on '{profile}':");
            for (i, e) in events.iter().enumerate() {
                println!("  [{}] {}  {}  {}", i + 1, e.time_display(), e.summary, e.location);
            }
            loop {
                let ans = prompt("pick event number, [c]reate, or title (empty = untitled): ");
                if let Ok(n) = ans.parse::<usize>() {
                    if let Some(e) = events.get(n.wrapping_sub(1)) {
                        return e.summary.clone();
                    }
                    println!("no event [{n}]");
                    continue;
                }
                return match ans.as_str() {
                    "c" | "C" => create_flow(profile, default_title),
                    "" => default_title.to_string(),
                    title => title.to_string(),
                };
            }
        }
        Ok(_) => {
            println!("no events today on '{profile}'.");
            let ans = prompt("[c]reate an event, or type a title (empty = untitled): ");
            match ans.as_str() {
                "c" | "C" => create_flow(profile, default_title),
                "" => default_title.to_string(),
                title => title.to_string(),
            }
        }
        Err(e) => {
            println!("calendar unavailable: {e}");
            let ans = prompt("meeting title (empty = untitled): ");
            if ans.is_empty() { default_title.to_string() } else { ans }
        }
    }
}

fn create_flow(profile: &str, default_title: &str) -> String {
    let title = {
        let t = prompt("event title: ");
        if t.is_empty() { default_title.to_string() } else { t }
    };
    match create(profile, &title) {
        Ok(()) => println!("event created."),
        Err(e) => println!("create failed ({e}); continuing without a calendar event."),
    }
    title
}
