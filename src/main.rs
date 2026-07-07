mod app;
mod audio;
mod brain;
mod calendar;
mod claude_pane;
mod doctor;
mod loopback;
mod notes;
mod session;
mod stt;
mod summary;
mod transcript;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use session::{Meeting, MeetingType, Session};
use transcript::Speaker;

#[derive(Parser)]
#[command(name = "steno", about = "Live meeting transcription TUI", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Meeting title (skips the calendar picker)
    #[arg(long, global = true)]
    title: Option<String>,
    /// Restrict the calendar picker to one Google profile (default: all)
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Project directory the meeting is about (granted to the claude pane)
    #[arg(long, global = true)]
    project: Option<std::path::PathBuf>,
    /// Capture "Them" from this input device (virtual loopback, e.g.
    /// "BlackHole 2ch" on macOS or "CABLE Output" on Windows)
    #[arg(long, global = true, env = "STENO_LOOPBACK_DEVICE")]
    loopback_device: Option<String>,
    /// STT engine: nemotron (default) or mock
    #[arg(long, global = true, default_value = "nemotron")]
    engine: String,
}

#[derive(Subcommand)]
enum Cmd {
    /// Daily standup
    Standup,
    /// One-on-one
    #[command(name = "1on1")]
    OneOnOne,
    /// Scheduled/general meeting (default)
    Meet,
    /// Reopen the most recent session
    Resume,
    /// List past meetings, or view one: `steno notes 2 [--transcript]`
    Notes {
        /// Meeting number from the list
        n: Option<usize>,
        /// Include the full transcript
        #[arg(long)]
        transcript: bool,
    },
    /// Health checks: audio, models, gcli, keys
    Doctor {
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    // one-time migration from the voice-mentor days: carry sessions + the 633MB
    // model over instead of orphaning them
    if let Some(base) = dirs::data_dir() {
        let old = base.join("voice-mentor");
        let new = base.join("steno");
        if old.is_dir() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
    let cli = Cli::parse();
    let kind = match cli.cmd {
        Some(Cmd::Doctor { json }) => return doctor::run(json),
        Some(Cmd::Notes { n, transcript }) => return notes::run(n, transcript),
        Some(Cmd::Resume) => {
            return run_meeting(Session::latest()?, &cli.engine, cli.loopback_device.as_deref());
        }
        Some(Cmd::Standup) => MeetingType::Standup,
        Some(Cmd::OneOnOne) => MeetingType::OneOnOne,
        Some(Cmd::Meet) | None => MeetingType::Meet,
    };
    let started = chrono::Local::now();
    let default_title = format!("{} {}", kind.label(), started.format("%Y-%m-%d"));
    let title = cli
        .title
        .clone()
        .unwrap_or_else(|| calendar::pick_title(cli.profile.as_deref(), &default_title));
    let session = Session::create(Meeting {
        title,
        kind,
        started,
        calendar_event_id: None,
        project_dir: cli.project.clone().map(|p| p.canonicalize().unwrap_or(p)),
        filed_to: None,
    })?;
    run_meeting(session, &cli.engine, cli.loopback_device.as_deref())
}

/// One factory per channel; nemotron shares a single recognizer between them.
fn engine_factories(name: &str) -> Result<(app::EngineFactory, app::EngineFactory)> {
    match name {
        "nemotron" => {
            stt::nemotron::ensure_model()?;
            eprintln!("loading Nemotron model (~2s)…");
            let rec = stt::nemotron::load_recognizer()?;
            let rec2 = rec.clone();
            Ok((
                Box::new(move || Box::new(stt::nemotron::Engine::new(rec))),
                Box::new(move || Box::new(stt::nemotron::Engine::new(rec2))),
            ))
        }
        "mock" => Ok((
            Box::new(|| Box::new(stt::MockEngine::new())),
            Box::new(|| Box::new(stt::MockEngine::new())),
        )),
        other => anyhow::bail!("unknown engine '{other}' (nemotron | mock)"),
    }
}

fn run_meeting(session: Session, engine: &str, loopback_device: Option<&str>) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(256);
        let (mic_tx, mic_rx) = tokio::sync::mpsc::channel(64);
        let (sys_tx, sys_rx) = tokio::sync::mpsc::channel(64);
        let (summary_ev_tx, summary_ev_rx) = tokio::sync::mpsc::channel(16);
        let (lines_tx, lines_rx) = tokio::sync::mpsc::channel(256);
        let (redraw_tx, redraw_rx) = tokio::sync::mpsc::channel(4);

        let (me_factory, them_factory) = engine_factories(engine)?;
        let mic = audio::start_mic(mic_tx)?;
        app::spawn_stt_pipeline(me_factory, Speaker::Me, mic_rx, events_tx.clone());

        // "Them" channel: whole-system loopback. Meeting audio IS the system audio.
        let loopback = match loopback::start(loopback_device, sys_tx) {
            Ok(h) => {
                app::spawn_stt_pipeline(them_factory, Speaker::Them, sys_rx, events_tx);
                Some(h)
            }
            Err(e) => {
                eprintln!("loopback capture unavailable ({e}); running mic-only");
                None
            }
        };
        let headphones = loopback::default_output_is_headphones();

        let summary_task = summary::spawn(
            session.meeting.kind,
            session.workspace().join("summary.md"),
            lines_rx,
            summary_ev_tx,
        );

        let chat_home = session::data_dir().join("chat");
        std::fs::create_dir_all(&chat_home)?;
        let claude = match claude_pane::start(
            &chat_home,
            &session.workspace(),
            &uuid::Uuid::new_v4().to_string(),
            session.meeting.project_dir.as_deref(),
            redraw_tx,
        ) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("claude pane unavailable: {e}");
                None
            }
        };

        let mut terminal = ratatui::init();
        let result = app::run(
            &mut terminal,
            app::App::new(session, loopback.is_some(), headphones, claude, Some(lines_tx)),
            events_rx,
            summary_ev_rx,
            redraw_rx,
            app::PauseFlags {
                mic: mic.paused.clone(),
                sys: loopback.as_ref().map(|l| l.paused.clone()),
            },
        )
        .await;
        ratatui::restore();
        drop(loopback); // kill parec

        let mut app = result?;
        println!(
            "meeting ended: {} utterances → {}",
            app.transcript.finals.len(),
            app.session.dir.display()
        );

        // close the lines channel so the summarizer runs its final flush, then
        // await it — the filed note must include the last minute of the meeting
        app.summary_lines = None;
        if !app.transcript.finals.is_empty() {
            println!("finalizing summary… (up to ~30s; Ctrl+C skips — the transcript is already saved)");
        }
        if let Ok(Ok(state)) =
            tokio::time::timeout(std::time::Duration::from_secs(45), summary_task).await
        {
            app.summary = state;
        }

        if !app.transcript.finals.is_empty() {
            match brain::ask_choice() {
                brain::SaveChoice::KeepLocal => println!("kept local only."),
                choice => {
                    let with_transcript =
                        matches!(choice, brain::SaveChoice::WithTranscript);
                    match brain::file_meeting(
                        &app.session,
                        &app.summary,
                        &app.transcript.finals,
                        with_transcript,
                    ) {
                        Ok(path) => {
                            println!("filed to brain: {}", path.display());
                            app.session.meeting.filed_to = Some(path);
                            let _ = app.session.save_meeting();
                        }
                        Err(e) => println!("brain filing failed ({e}); session dir keeps everything."),
                    }
                }
            }
        }
        Ok(())
    })
}
