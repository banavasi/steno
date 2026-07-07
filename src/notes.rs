//! `steno notes` — one combined list of ALL meetings: local recorded sessions
//! plus every `type: meeting` note in the brain (07-meetings/ and
//! 02-projects/*/meetings/), deduped where a session was filed. Viewing prints
//! the brain note when one exists, else the local summary; `--transcript`
//! appends the recorded transcript (sessions only).
use crate::session::{Meeting, Session};
use crate::transcript::Utterance;
use anyhow::{Context, Result};
use std::io::BufRead;
use std::path::{Path, PathBuf};

struct Entry {
    date: String,  // YYYY-MM-DD
    time: String,  // HH:MM ("" when unknown)
    kind: String,  // standup/1on1/meet or "meeting"
    title: String,
    status: String, // held/scheduled/… ("" for local sessions)
    session: Option<(PathBuf, Meeting)>,
    note: Option<PathBuf>,
}

pub fn run(pick: Option<usize>, with_transcript: bool) -> Result<()> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut claimed_notes: Vec<PathBuf> = Vec::new();

    for (dir, m) in Session::list_all() {
        let note = brain_note(&m);
        if let Some(n) = &note {
            claimed_notes.push(n.clone());
        }
        entries.push(Entry {
            date: m.started.format("%Y-%m-%d").to_string(),
            time: m.started.format("%H:%M").to_string(),
            kind: m.kind.label().to_string(),
            title: m.title.clone(),
            status: String::new(),
            session: Some((dir, m)),
            note,
        });
    }
    for path in brain_meeting_files() {
        if claimed_notes.contains(&path) {
            continue;
        }
        if let Some(e) = parse_brain_note(&path) {
            entries.push(e);
        }
    }
    anyhow::ensure!(!entries.is_empty(), "no meetings found (local or in the brain)");
    entries.sort_by(|a, b| (&b.date, &b.time).cmp(&(&a.date, &a.time)));

    let Some(n) = pick else {
        for (i, e) in entries.iter().enumerate() {
            let utts = e
                .session
                .as_ref()
                .map(|(d, _)| format!("{:3} utt", transcript_lines(d).len()))
                .unwrap_or_else(|| "      —".into());
            let source = match (&e.session, &e.note) {
                (Some(_), Some(_)) => "→ brain",
                (Some(_), None) => "local",
                (None, _) => "brain",
            };
            let status = if e.status.is_empty() || e.status == "held" {
                String::new()
            } else {
                format!(" [{}]", e.status)
            };
            println!(
                "[{}] {} {:5}  {:7} {}  {:7}  {}{}",
                i + 1,
                e.date,
                e.time,
                e.kind,
                utts,
                source,
                e.title,
                status,
            );
        }
        println!("\nsteno notes <n> [--transcript] to view one");
        return Ok(());
    };

    let e = entries.get(n - 1).with_context(|| format!("no meeting [{n}]"))?;
    match (&e.note, &e.session) {
        (Some(note), _) => {
            println!("── {} ──\n", note.display());
            print!("{}", std::fs::read_to_string(note)?);
        }
        (None, Some((dir, m))) => {
            println!(
                "── {} ({}) · {} {} · local only ──\n",
                m.title, e.kind, e.date, e.time,
            );
            match std::fs::read_to_string(dir.join("workspace/summary.md")) {
                Ok(s) if !s.trim().is_empty() => println!("{s}"),
                _ => println!("(no summary was generated)"),
            }
        }
        (None, None) => unreachable!("entry has neither note nor session"),
    }
    if with_transcript {
        match &e.session {
            Some((dir, m)) => {
                println!("\n## Transcript");
                for u in transcript_lines(dir) {
                    let wall =
                        m.started + chrono::Duration::milliseconds((u.t * 1000.0) as i64);
                    println!(
                        "- **{}** [{}] {}",
                        u.speaker.label().trim(),
                        wall.format("%H:%M"),
                        u.text
                    );
                }
            }
            None => println!("\n(no recorded transcript — this meeting wasn't captured by steno)"),
        }
    }
    Ok(())
}

fn transcript_lines(dir: &Path) -> Vec<Utterance> {
    let Ok(f) = std::fs::File::open(dir.join("transcript.jsonl")) else {
        return Vec::new();
    };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

fn brain_dir() -> Option<PathBuf> {
    let d = dirs::home_dir()?.join("brain");
    d.is_dir().then_some(d)
}

/// All meeting note files: 07-meetings/*.md + 02-projects/*/meetings/*.md.
fn brain_meeting_files() -> Vec<PathBuf> {
    let Some(brain) = brain_dir() else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = Vec::new();
    let mut push_md = |dir: &Path| {
        if let Ok(entries) = std::fs::read_dir(dir) {
            files.extend(
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "md")),
            );
        }
    };
    push_md(&brain.join("07-meetings"));
    if let Ok(projects) = std::fs::read_dir(brain.join("02-projects")) {
        for p in projects.filter_map(|e| e.ok()) {
            push_md(&p.path().join("meetings"));
        }
    }
    files
}

/// Minimal frontmatter read: only meetings, only the fields the list needs.
fn parse_brain_note(path: &Path) -> Option<Entry> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut fm = std::collections::HashMap::new();
    let mut in_fm = false;
    for line in text.lines() {
        match (line.trim(), in_fm) {
            ("---", false) => in_fm = true,
            ("---", true) => break,
            (l, true) => {
                if let Some((k, v)) = l.split_once(':') {
                    fm.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
                }
            }
            _ => return None, // content before frontmatter
        }
    }
    if fm.get("type").map(String::as_str) != Some("meeting") {
        return None;
    }
    Some(Entry {
        date: fm.get("date").cloned().unwrap_or_default(),
        time: fm.get("time").cloned().unwrap_or_default(),
        kind: "meeting".into(),
        title: fm
            .get("title")
            .cloned()
            .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into_owned()),
        status: fm.get("status").cloned().unwrap_or_default(),
        session: None,
        note: Some(path.to_path_buf()),
    })
}

/// The filed brain note for a session: the recorded path if we saved one, else
/// a guess by the naming scheme (covers notes filed before `filed_to` existed).
fn brain_note(m: &Meeting) -> Option<PathBuf> {
    if let Some(p) = &m.filed_to {
        return p.exists().then(|| p.clone());
    }
    let slug: String = m
        .title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let meetings = brain_dir()?.join("07-meetings");
    let date = m.started.format("%Y-%m-%d");
    [
        meetings.join(format!("{date}-{slug}.md")),
        meetings.join(format!("{date}-{slug}-{}.md", m.started.format("%H%M"))),
    ]
    .into_iter()
    .find(|c| c.exists())
}
