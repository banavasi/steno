use crate::transcript::Utterance;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeetingType {
    Standup,
    OneOnOne,
    Meet,
}

impl MeetingType {
    pub fn label(self) -> &'static str {
        match self {
            MeetingType::Standup => "standup",
            MeetingType::OneOnOne => "1on1",
            MeetingType::Meet => "meet",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meeting {
    pub title: String,
    pub kind: MeetingType,
    pub started: chrono::DateTime<chrono::Local>,
    #[serde(default)]
    pub calendar_event_id: Option<String>,
    #[serde(default)]
    pub project_dir: Option<PathBuf>,
    /// Brain note path once filed (set at save time).
    #[serde(default)]
    pub filed_to: Option<PathBuf>,
}

/// On-disk session: crash-safe by construction (append-only jsonl + small json files).
pub struct Session {
    pub dir: PathBuf,
    pub meeting: Meeting,
    transcript: File,
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voice-mentor")
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
    if slug.is_empty() { "untitled".into() } else { slug }
}

impl Session {
    pub fn create(meeting: Meeting) -> Result<Self> {
        let dir = data_dir().join("sessions").join(format!(
            "{}--{}",
            meeting.started.format("%Y-%m-%dT%H-%M-%S"),
            slugify(&meeting.title)
        ));
        fs::create_dir_all(dir.join("workspace"))?;
        // atomic-ish: meeting.json is small; write then rename
        let tmp = dir.join("meeting.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&meeting)?)?;
        fs::rename(&tmp, dir.join("meeting.json"))?;
        fs::write(
            dir.join("workspace/CLAUDE.md"),
            format!(
                "# Meeting workspace — {} ({})\n\nStarted {}. This directory belongs to the \
                 voice-mentor TUI: `transcript.md` is the LIVE meeting transcript (appended \
                 continuously; 'Me' = the user, 'Them' = other participants) and `summary.md` \
                 is the rolling AI summary. Always re-read `transcript.md` before answering \
                 questions about what was said.\n",
                meeting.title,
                meeting.kind.label(),
                meeting.started.format("%Y-%m-%d %H:%M"),
            ),
        )?;
        let transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("transcript.jsonl"))?;
        Ok(Self { dir, meeting, transcript })
    }

    pub fn workspace(&self) -> PathBuf {
        self.dir.join("workspace")
    }

    /// Rewrite meeting.json (e.g. after recording where the note was filed).
    pub fn save_meeting(&self) -> Result<()> {
        let tmp = self.dir.join("meeting.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&self.meeting)?)?;
        fs::rename(&tmp, self.dir.join("meeting.json"))?;
        Ok(())
    }

    /// All sessions on disk, newest first.
    pub fn list_all() -> Vec<(PathBuf, Meeting)> {
        let root = data_dir().join("sessions");
        let Ok(entries) = fs::read_dir(&root) else {
            return Vec::new();
        };
        let mut all: Vec<(PathBuf, Meeting)> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter_map(|p| {
                let m: Meeting = serde_json::from_slice(&fs::read(p.join("meeting.json")).ok()?).ok()?;
                Some((p, m))
            })
            .collect();
        all.sort_by(|a, b| b.0.cmp(&a.0));
        all
    }

    /// Human-readable live transcript for the claude pane to read.
    pub fn append_workspace_transcript(&self, line: &str) {
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.workspace().join("transcript.md"))
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// Most recent session on disk (for `mentor resume`).
    pub fn latest() -> Result<Self> {
        let root = data_dir().join("sessions");
        let mut dirs: Vec<_> = fs::read_dir(&root)
            .with_context(|| format!("no sessions at {}", root.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("meeting.json").exists())
            .collect();
        dirs.sort();
        let dir = dirs.pop().context("no previous session to resume")?;
        let meeting: Meeting = serde_json::from_slice(&fs::read(dir.join("meeting.json"))?)?;
        let transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("transcript.jsonl"))?;
        Ok(Self { dir, meeting, transcript })
    }

    pub fn load_transcript(&self) -> Vec<Utterance> {
        let Ok(f) = File::open(self.dir.join("transcript.jsonl")) else {
            return Vec::new();
        };
        BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect()
    }

    pub fn append_utterance(&mut self, utt: &Utterance) -> Result<()> {
        let mut line = serde_json::to_string(utt)?;
        line.push('\n');
        self.transcript.write_all(line.as_bytes())?;
        self.transcript.sync_data()?; // cheap at utterance cadence; power-loss-safe
        Ok(())
    }
}
