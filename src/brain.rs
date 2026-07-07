//! File the finished meeting into the second brain (~/brain), following its
//! vault contract: type: meeting → 07-meetings/YYYY-MM-DD-<slug>.md (no project
//! linkage in v1), status: held, action items as `- [ ] … #fu`, atomic write
//! (the vault is synced — partial reads corrupt). Transcript inclusion is a
//! save-time CHOICE (summary-only default): third-party speech in a
//! git-versioned vault is retention the user must opt into.
use crate::session::Session;
use crate::summary::SummaryState;
use crate::transcript::Utterance;
use anyhow::{Context, Result};
use chrono::Duration;
use std::io::Write;
use std::path::PathBuf;

pub enum SaveChoice {
    SummaryOnly,
    WithTranscript,
    KeepLocal,
}

/// Plain-stdin prompt, shown after the TUI has been restored.
pub fn ask_choice() -> SaveChoice {
    print!(
        "\nfile to brain?  [Enter] summary only · [t] summary + full transcript · [k] keep local only: "
    );
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    match line.trim() {
        "t" | "T" => SaveChoice::WithTranscript,
        "k" | "K" => SaveChoice::KeepLocal,
        _ => SaveChoice::SummaryOnly,
    }
}

fn brain_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("brain")
}

fn slugify(s: &str) -> String {
    let slug: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { "meeting".into() } else { slug }
}

pub fn file_meeting(
    session: &Session,
    summary: &SummaryState,
    transcript: &[Utterance],
    include_transcript: bool,
) -> Result<PathBuf> {
    let meetings = brain_dir().join("07-meetings");
    anyhow::ensure!(meetings.is_dir(), "brain not found at {}", meetings.display());

    let m = &session.meeting;
    let date = m.started.format("%Y-%m-%d");
    let mut path = meetings.join(format!("{date}-{}.md", slugify(&m.title)));
    if path.exists() {
        // collision-safe: same title twice in a day gets a time suffix
        path = meetings.join(format!(
            "{date}-{}-{}.md",
            slugify(&m.title),
            m.started.format("%H%M")
        ));
    }

    let sect = |title: &str, items: &[String], task: bool| {
        if items.is_empty() {
            return String::new();
        }
        let bullets = items
            .iter()
            .map(|i| {
                if task {
                    format!("- [ ] {i} #fu")
                } else {
                    format!("- {i}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n## {title}\n{bullets}\n")
    };

    let mut body = format!(
        "---\ntype: meeting\ntitle: {title}\ncreated: {date}\nupdated: {date}\ndate: {date}\ntime: \"{time}\"\nattendees: []\nproject:\narea:\nsource: voice-mentor\nevent-id:{event_id}\nevent-url:\nstatus: held\nfollow-ups: {fu}\ntags: [meeting]\n---\n# {title} — {date}\n\n**Type:** {kind} · **Recorded by:** voice-mentor · **Session:** `{session_dir}`\n",
        title = m.title,
        date = date,
        time = m.started.format("%H:%M"),
        event_id = m
            .calendar_event_id
            .as_deref()
            .map(|id| format!(" {id}"))
            .unwrap_or_default(),
        fu = !summary.actions.is_empty(),
        kind = m.kind.label(),
        session_dir = session.dir.display(),
    );
    body.push_str(&sect("Summary", &summary.summary, false));
    body.push_str(&sect("Decisions", &summary.decisions, false));
    body.push_str(&sect("Action items", &summary.actions, true));
    body.push_str(&sect("Open questions", &summary.open_questions, false));

    if include_transcript {
        body.push_str("\n## Transcript\n");
        for u in transcript {
            let ts = (m.started + Duration::milliseconds((u.t * 1000.0) as i64)).format("%H:%M");
            body.push_str(&format!("- **{}** [{ts}] {}\n", u.speaker.label().trim(), u.text));
        }
    }

    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, body).context("write meeting note")?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}
