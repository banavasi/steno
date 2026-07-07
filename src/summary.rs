//! Rolling meeting summary via `claude -p --model haiku` (headless print mode):
//! rides the user's existing Claude Code login — no API key, no marginal spend.
//! Delta-fed state updates (never re-feeds the whole transcript — high-water
//! mark per the fleet's bg-llm-idempotent rule), backoff on errors.
use crate::session::MeetingType;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SummaryState {
    #[serde(default)]
    pub summary: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub points_to_discuss: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
}

pub enum SummaryEvent {
    Updated(SummaryState),
    Status(String),
}

const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(45);
const MIN_DELTA_CHARS: usize = 80; // ~one short utterance
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn instructions(kind: MeetingType) -> String {
    let focus = match kind {
        MeetingType::Standup => {
            "This is a daily standup: capture per-person yesterday/today/blockers in `summary`, \
             and every blocker or dependency in `open_questions`."
        }
        MeetingType::OneOnOne => {
            "This is a 1-on-1: capture topics and feedback themes in `summary`, agreements in \
             `decisions`, and each person's follow-ups in `actions`."
        }
        MeetingType::Meet => {
            "This is a working meeting: keep `summary` to the narrative arc, record concrete \
             `decisions`, and extract every commitment into `actions` with an owner."
        }
    };
    format!(
        "You maintain the live summary pane of a meeting-transcription TUI. You receive the \
         CURRENT summary state as JSON plus ONLY the new transcript lines since the last \
         update. Speakers: 'Me' is the TUI's user; 'Them' is everyone else on the call. {focus}\n\
         Return ONLY a JSON object (no prose, no code fences) with keys: summary, decisions, \
         actions, points_to_discuss, open_questions — each an array of short strings. Update \
         incrementally: keep existing bullets stable unless new lines contradict or refine \
         them; merge rather than duplicate; move resolved open questions into decisions. \
         Never return an all-empty object when the new lines contain speech: even small talk \
         gets one `summary` bullet describing what was discussed."
    )
}

/// One summary refresh = one `claude -p` subprocess on the user's own login.
async fn update(
    instructions: &str,
    prev: &SummaryState,
    delta: &str,
) -> Result<SummaryState> {
    let prompt = format!(
        "{instructions}\n\nCurrent summary state:\n{}\n\nNew transcript lines:\n{}\n\nReturn the updated JSON.",
        serde_json::to_string(prev)?,
        delta
    );
    let mut cmd = tokio::process::Command::new("claude");
    cmd.args(["-p", "--model", "haiku"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // don't let the child think it's nested inside another claude session
    for var in ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT"] {
        cmd.env_remove(var);
    }
    let mut child = cmd.spawn().context("spawn `claude -p` (is claude on PATH?)")?;
    child
        .stdin
        .take()
        .context("claude stdin")?
        .write_all(prompt.as_bytes())
        .await?;
    let out = tokio::time::timeout(CALL_TIMEOUT, child.wait_with_output())
        .await
        .context("claude -p timed out")??;
    if !out.status.success() {
        anyhow::bail!("claude -p exited {}", out.status);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str(text).context("parse summary JSON")
}

/// Task: consumes transcript lines, periodically refreshes the summary, writes
/// workspace/summary.md, and reports state/status to the app. Returns its final
/// state so the meeting-end flush can be awaited before filing (drop the lines
/// sender to trigger it — the task ends when the channel closes).
pub fn spawn(
    kind: MeetingType,
    summary_md: PathBuf,
    mut lines: mpsc::Receiver<String>,
    events: mpsc::Sender<SummaryEvent>,
) -> tokio::task::JoinHandle<SummaryState> {
    tokio::spawn(async move {
        let instructions = instructions(kind);
        let mut state = SummaryState::default();
        let mut calls = 0u32;
        let mut pending = String::new();
        let mut last_call = std::time::Instant::now() - MIN_INTERVAL;
        let mut backoff_until = std::time::Instant::now();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));

        loop {
            tokio::select! {
                line = lines.recv() => match line {
                    Some(l) => { pending.push_str(&l); pending.push('\n'); }
                    None => break, // meeting over
                },
                _ = tick.tick() => {
                    if pending.len() < MIN_DELTA_CHARS
                        || last_call.elapsed() < MIN_INTERVAL
                        || std::time::Instant::now() < backoff_until
                    {
                        continue;
                    }
                    let delta = std::mem::take(&mut pending);
                    last_call = std::time::Instant::now();
                    match update(&instructions, &state, &delta).await {
                        Ok(next) => {
                            calls += 1;
                            state = next;
                            let _ = std::fs::write(&summary_md, render_md(&state));
                            let _ = events.send(SummaryEvent::Updated(state.clone())).await;
                            let _ = events.send(SummaryEvent::Status(
                                format!("updated {} · {calls} calls (subscription)",
                                    chrono::Local::now().format("%H:%M:%S")))).await;
                        }
                        Err(e) => {
                            pending = delta + &pending; // retry this delta next round
                            backoff_until = std::time::Instant::now()
                                + std::time::Duration::from_secs(60);
                            let _ = events.send(SummaryEvent::Status(
                                format!("stale — {e} (retrying)"))).await;
                        }
                    }
                }
            }
        }
        // final flush so the filed note gets everything said since the last refresh
        if !pending.trim().is_empty()
            && let Ok(next) = update(&instructions, &state, &pending).await
        {
            state = next;
            let _ = std::fs::write(&summary_md, render_md(&state));
        }
        state
    })
}

pub fn render_md(s: &SummaryState) -> String {
    let sect = |title: &str, items: &[String]| {
        if items.is_empty() {
            String::new()
        } else {
            format!(
                "## {title}\n{}\n\n",
                items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n")
            )
        }
    };
    format!(
        "{}{}{}{}{}",
        sect("Summary", &s.summary),
        sect("Decisions", &s.decisions),
        sect("Action items", &s.actions),
        sect("Points to discuss", &s.points_to_discuss),
        sect("Open questions", &s.open_questions),
    )
}
