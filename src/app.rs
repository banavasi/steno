use crate::audio::AudioChunk;
use crate::claude_pane::ClaudePane;
use crate::session::Session;
use crate::stt::{SttEngine, SttEvent};
use crate::summary::{SummaryEvent, SummaryState};
use crate::transcript::{Speaker, TranscriptStore, Utterance};
use crate::ui;
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

pub enum AppEvent {
    Stt(Speaker, SttEvent),
    Level(Speaker, f32),
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    Transcript,
    Claude,
}

/// Pause flags shared with the capture threads. `sys` is None when loopback
/// capture is unavailable (mic-only mode).
pub struct PauseFlags {
    pub mic: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub sys: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

pub struct App {
    pub session: Session,
    pub transcript: TranscriptStore,
    pub claude: Option<ClaudePane>,
    pub focus: Focus,
    pub summary: SummaryState,
    pub summary_status: String,
    pub summary_lines: Option<mpsc::Sender<String>>,
    pub mic_db: f32,
    pub them_db: f32,
    pub has_loopback: bool,
    pub headphones: Option<bool>,
    /// Sliding window of (mic_db, sys_db) pairs for the echo heuristic.
    level_pairs: std::collections::VecDeque<(f32, f32)>,
    pub echo_suspect: bool,
    pub mic_paused: bool,
    pub all_paused: bool,
    /// None = stick to tail; Some(n) = scrolled up by n lines.
    pub scroll_up: Option<u16>,
    pub started: std::time::Instant,
    quit: bool,
}

impl App {
    pub fn new(
        session: Session,
        has_loopback: bool,
        headphones: Option<bool>,
        claude: Option<ClaudePane>,
        summary_lines: Option<mpsc::Sender<String>>,
    ) -> Self {
        let transcript = TranscriptStore {
            finals: session.load_transcript(), // non-empty on resume
            ..Default::default()
        };
        Self {
            session,
            transcript,
            claude,
            focus: Focus::Transcript,
            summary: SummaryState::default(),
            summary_status: "waiting for speech…".into(),
            summary_lines,
            mic_db: f32::NEG_INFINITY,
            them_db: f32::NEG_INFINITY,
            has_loopback,
            headphones,
            level_pairs: std::collections::VecDeque::with_capacity(64),
            echo_suspect: false,
            mic_paused: false,
            all_paused: false,
            scroll_up: None,
            started: std::time::Instant::now(),
            quit: false,
        }
    }

    /// Echo heuristic: when system audio is active and the mic level tracks it
    /// (Pearson r over ~5 s), remote voices are bleeding into the mic — the
    /// channel-separation labels are degraded. Honest warning, not AEC (v2).
    fn update_echo(&mut self, sys_db: f32) {
        self.level_pairs.push_back((self.mic_db, sys_db));
        if self.level_pairs.len() > 50 {
            self.level_pairs.pop_front();
        }
        if self.level_pairs.len() < 30 {
            return;
        }
        let n = self.level_pairs.len() as f32;
        let (mut sx, mut sy) = (0.0f32, 0.0f32);
        for &(x, y) in &self.level_pairs {
            sx += x.max(-80.0);
            sy += y.max(-80.0);
        }
        let (mx, my) = (sx / n, sy / n);
        let (mut cov, mut vx, mut vy) = (0.0f32, 0.0f32, 0.0f32);
        for &(x, y) in &self.level_pairs {
            let (dx, dy) = (x.max(-80.0) - mx, y.max(-80.0) - my);
            cov += dx * dy;
            vx += dx * dx;
            vy += dy * dy;
        }
        let sys_active = my > -50.0;
        let r = cov / (vx.sqrt() * vy.sqrt()).max(1e-6);
        self.echo_suspect = sys_active && r > 0.6;
    }

    fn on_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            (KeyCode::Char('m'), _) => self.mic_paused = !self.mic_paused,
            (KeyCode::Char('p'), _) => self.all_paused = !self.all_paused,
            (KeyCode::Up, _) => {
                self.scroll_up = Some(self.scroll_up.unwrap_or(0).saturating_add(1));
            }
            (KeyCode::Down, _) => {
                self.scroll_up = match self.scroll_up.unwrap_or(0).saturating_sub(1) {
                    0 => None,
                    n => Some(n),
                };
            }
            (KeyCode::PageUp, _) => {
                self.scroll_up = Some(self.scroll_up.unwrap_or(0).saturating_add(10));
            }
            (KeyCode::PageDown, _) => {
                self.scroll_up = match self.scroll_up.unwrap_or(0).saturating_sub(10) {
                    0 => None,
                    n => Some(n),
                };
            }
            (KeyCode::End, _) => self.scroll_up = None,
            _ => {}
        }
    }

    fn on_stt(&mut self, speaker: Speaker, ev: SttEvent) -> Result<()> {
        match ev {
            SttEvent::Partial(text) => self.transcript.set_partial(speaker, text),
            SttEvent::Final { text, t_start } => {
                let utt = Utterance { t: t_start, speaker, text };
                self.session.append_utterance(&utt)?;
                // one human-readable line fans out to the claude workspace + summarizer
                let wall = self.session.meeting.started
                    + chrono::Duration::milliseconds((utt.t * 1000.0) as i64);
                let line = format!(
                    "[{}] {}: {}",
                    wall.format("%H:%M"),
                    utt.speaker.label().trim(),
                    utt.text
                );
                self.session.append_workspace_transcript(&line);
                if let Some(tx) = &self.summary_lines {
                    let _ = tx.try_send(line);
                }
                self.transcript.push_final(utt);
            }
        }
        Ok(())
    }
}

/// Single event loop owning all state: key events, STT events, summary updates,
/// claude-pane output, and a coalesced ~30fps redraw tick.
pub async fn run(
    terminal: &mut DefaultTerminal,
    mut app: App,
    mut events: mpsc::Receiver<AppEvent>,
    mut summary_events: mpsc::Receiver<SummaryEvent>,
    mut claude_redraw: mpsc::Receiver<()>,
    pause: PauseFlags,
) -> Result<App> {
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;

    while !app.quit {
        tokio::select! {
            ev = keys.next() => {
                match ev {
                    Some(Ok(Event::Key(k))) if k.is_press() => {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        // Ctrl+Q quits from ANY focus — never forwarded to the pane
                        if ctrl && k.code == KeyCode::Char('q') {
                            app.quit = true;
                        } else if ctrl && k.code == KeyCode::Char('t') && app.claude.is_some() {
                            app.focus = match app.focus {
                                Focus::Transcript => Focus::Claude,
                                Focus::Claude => Focus::Transcript,
                            };
                        } else if app.focus == Focus::Claude {
                            if let Some(c) = &app.claude { c.send_key(k); }
                        } else {
                            app.on_key(k);
                        }
                        dirty = true;
                    }
                    Some(Ok(Event::Resize(_, _))) => dirty = true,
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                    _ => {}
                }
                pause.mic.store(
                    app.mic_paused || app.all_paused,
                    std::sync::atomic::Ordering::Relaxed,
                );
                if let Some(sys) = &pause.sys {
                    sys.store(app.all_paused, std::sync::atomic::Ordering::Relaxed);
                }
            }
            ev = events.recv() => {
                match ev {
                    Some(AppEvent::Stt(sp, e)) => { app.on_stt(sp, e)?; dirty = true; }
                    Some(AppEvent::Level(Speaker::Me, db)) => { app.mic_db = db; }
                    Some(AppEvent::Level(Speaker::Them, db)) => {
                        app.them_db = db;
                        app.update_echo(db);
                    }
                    None => break,
                }
            }
            ev = summary_events.recv() => {
                match ev {
                    Some(SummaryEvent::Updated(s)) => { app.summary = s; dirty = true; }
                    Some(SummaryEvent::Status(s)) => { app.summary_status = s; dirty = true; }
                    None => {}
                }
            }
            _ = claude_redraw.recv() => { dirty = true; }
            _ = tick.tick() => {
                if dirty {
                    terminal.draw(|f| ui::draw(f, &mut app))?;
                    dirty = false;
                } else {
                    // level meters only; cheap enough to always refresh
                    terminal.draw(|f| ui::draw(f, &mut app))?;
                }
            }
        }
    }
    if let Some(mut c) = app.claude.take() {
        c.shutdown();
    }
    Ok(app)
}

/// Engine construction runs inside the pipeline thread (sherpa streams are not Send).
pub type EngineFactory = Box<dyn FnOnce() -> Box<dyn SttEngine> + Send>;

/// Bridge: audio chunks → engine → AppEvents. Runs on a blocking thread; the engine
/// (real model or mock) is CPU work that must not sit on the async runtime.
pub fn spawn_stt_pipeline(
    factory: EngineFactory,
    speaker: Speaker,
    mut audio_rx: mpsc::Receiver<AudioChunk>,
    events_tx: mpsc::Sender<AppEvent>,
) {
    std::thread::Builder::new()
        .name(format!("stt-{}", speaker.label().trim()))
        .spawn(move || {
            let mut engine = factory();
            while let Some(chunk) = audio_rx.blocking_recv() {
                let _ = events_tx.blocking_send(AppEvent::Level(speaker, chunk.rms_db));
                for ev in engine.feed(&chunk.pcm) {
                    if events_tx.blocking_send(AppEvent::Stt(speaker, ev)).is_err() {
                        return;
                    }
                }
            }
            for ev in engine.finish() {
                let _ = events_tx.blocking_send(AppEvent::Stt(speaker, ev));
            }
        })
        .expect("spawn stt thread");
}
