//! `mentor notes` — browse past meetings from the terminal.
//! Lists sessions newest-first; viewing one prints the filed brain note when it
//! exists, otherwise the local summary.md; `--transcript` appends the transcript.
use crate::session::{Meeting, Session};
use crate::transcript::Utterance;
use anyhow::{Context, Result};
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub fn run(pick: Option<usize>, with_transcript: bool) -> Result<()> {
    let sessions = Session::list_all();
    anyhow::ensure!(!sessions.is_empty(), "no meetings recorded yet");

    let Some(n) = pick else {
        for (i, (dir, m)) in sessions.iter().enumerate() {
            let utts = transcript_lines(dir).len();
            let filed = if brain_note(dir, m).is_some() { "→ brain" } else { "local" };
            println!(
                "[{}] {}  {:7} {:5} utt  {:7}  {}",
                i + 1,
                m.started.format("%Y-%m-%d %H:%M"),
                m.kind.label(),
                utts,
                filed,
                m.title,
            );
        }
        println!("\nmentor notes <n> [--transcript] to view one");
        return Ok(());
    };

    let (dir, meeting) = sessions.get(n - 1).with_context(|| format!("no meeting [{n}]"))?;
    match brain_note(dir, meeting) {
        Some(note) => {
            println!("── {} ──\n", note.display());
            print!("{}", std::fs::read_to_string(&note)?);
        }
        None => {
            println!(
                "── {} ({}) · {} · local only ──\n",
                meeting.title,
                meeting.kind.label(),
                meeting.started.format("%Y-%m-%d %H:%M"),
            );
            match std::fs::read_to_string(dir.join("workspace/summary.md")) {
                Ok(s) if !s.trim().is_empty() => println!("{s}"),
                _ => println!("(no summary was generated)"),
            }
        }
    }
    if with_transcript {
        println!("\n## Transcript");
        for u in transcript_lines(dir) {
            let wall = meeting.started + chrono::Duration::milliseconds((u.t * 1000.0) as i64);
            println!("- **{}** [{}] {}", u.speaker.label().trim(), wall.format("%H:%M"), u.text);
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

/// The filed brain note: the recorded path if we saved one, else a glob-free
/// guess by the naming scheme (covers notes filed before `filed_to` existed).
fn brain_note(_dir: &Path, m: &Meeting) -> Option<PathBuf> {
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
    let meetings = dirs::home_dir()?.join("brain/07-meetings");
    let date = m.started.format("%Y-%m-%d");
    [
        meetings.join(format!("{date}-{slug}.md")),
        meetings.join(format!("{date}-{slug}-{}.md", m.started.format("%H%M"))),
    ]
    .into_iter()
    .find(|c| c.exists())
}
