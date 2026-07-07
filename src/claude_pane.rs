//! Embedded Claude Code pane: `claude --model haiku` inside a portable-pty PTY,
//! rendered through vt100 → tui-term. M0-spike facts baked in:
//! - answer DA1 / OSC 10/11/12 / XTVERSION / DECRQM (and friends) or startup
//!   stalls ~2 s per unanswered probe; nothing is hard-blocking, all cheap.
//! - claude reflows itself on SIGWINCH; resize = master.resize + parser.set_size.
//! - repaints are bracketed in CSI ?2026 h/l (synchronized output) that vt100
//!   doesn't model: buffer between markers and feed the parser atomically.
use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub struct ClaudePane {
    pub parser: Arc<Mutex<vt100::Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16), // rows, cols
}

impl Drop for ClaudePane {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// `chat_home` is a FIXED directory (trusted in claude once, ever); the
/// per-meeting `workspace` is granted via --add-dir and pointed to by the
/// system prompt, so no per-meeting folder-trust dialog fires.
pub fn start(
    chat_home: &Path,
    workspace: &Path,
    session_id: &str,
    project_dir: Option<&Path>,
    redraw: mpsc::Sender<()>,
) -> Result<ClaudePane> {
    let pty = native_pty_system();
    let (rows, cols) = (20u16, 60u16); // real size arrives on first draw
    let pair = pty
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .context("openpty")?;

    let ws = workspace.to_string_lossy();
    let mut cmd = CommandBuilder::new("claude");
    cmd.args(["--model", "haiku", "--session-id", session_id]);
    cmd.args([
        "--append-system-prompt",
        &format!(
            "You are the in-meeting assistant pane of the steno TUI. The live meeting \
             transcript is appended continuously to {ws}/transcript.md ('Me' = the user, \
             'Them' = other participants) and a rolling summary to {ws}/summary.md. Always \
             re-read the transcript before answering questions about the meeting."
        ),
    ]);
    cmd.args(["--add-dir", &ws]);
    if let Some(dir) = project_dir {
        cmd.args(["--add-dir", &dir.to_string_lossy()]);
    }
    cmd.cwd(chat_home);
    cmd.env("TERM", "xterm-256color");
    // don't let the child think it's nested inside another claude session
    for var in ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT"] {
        cmd.env_remove(var);
    }
    let child = pair.slave.spawn_command(cmd).context("spawn claude")?;
    drop(pair.slave);

    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 2000)));
    let writer = Arc::new(Mutex::new(pair.master.take_writer().context("pty writer")?));
    let mut reader = pair.master.try_clone_reader().context("pty reader")?;

    let t_parser = parser.clone();
    let t_writer = writer.clone();
    std::thread::Builder::new().name("claude-pty".into()).spawn(move || {
        let mut buf = [0u8; 8192];
        let mut pending: Vec<u8> = Vec::new();
        let mut syncing = false;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break, // child exited
                Ok(n) => n,
            };
            respond_to_queries(&buf[..n], &t_parser, &t_writer);
            pending.extend_from_slice(&buf[..n]);
            feed_with_sync_buffering(&mut pending, &mut syncing, &t_parser);
            let _ = redraw.try_send(()); // coalesced by the app tick
        }
        let _ = redraw.try_send(());
    })?;

    Ok(ClaudePane { parser, writer, master: pair.master, child, size: (rows, cols) })
}

const SYNC_ON: &[u8] = b"\x1b[?2026h";
const SYNC_OFF: &[u8] = b"\x1b[?2026l";
const SYNC_CAP: usize = 2 * 1024 * 1024;

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Feed `pending` to the parser, holding back synchronized-output frames until
/// their closing marker so the UI never samples a half-drawn repaint.
fn feed_with_sync_buffering(
    pending: &mut Vec<u8>,
    syncing: &mut bool,
    parser: &Arc<Mutex<vt100::Parser>>,
) {
    loop {
        if *syncing {
            match find(pending, SYNC_OFF) {
                Some(i) => {
                    let end = i + SYNC_OFF.len();
                    parser.lock().unwrap().process(&pending[..end]);
                    pending.drain(..end);
                    *syncing = false;
                }
                None => {
                    if pending.len() > SYNC_CAP {
                        // runaway frame: bail out rather than stall the pane
                        parser.lock().unwrap().process(pending);
                        pending.clear();
                        *syncing = false;
                    }
                    return;
                }
            }
        } else {
            match find(pending, SYNC_ON) {
                Some(i) => {
                    if i > 0 {
                        parser.lock().unwrap().process(&pending[..i]);
                    }
                    pending.drain(..i);
                    *syncing = true;
                }
                None => {
                    // keep a marker-length tail in case ESC[?2026h spans reads
                    let keep = pending.len().min(SYNC_ON.len() - 1);
                    let feed_len = pending.len() - keep;
                    if feed_len > 0 {
                        parser.lock().unwrap().process(&pending[..feed_len]);
                        pending.drain(..feed_len);
                    }
                    return;
                }
            }
        }
    }
}

/// Minimal terminal-query responder (the M0-verified table).
fn respond_to_queries(
    bytes: &[u8],
    parser: &Arc<Mutex<vt100::Parser>>,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
) {
    let reply = |r: &[u8]| {
        let mut w = writer.lock().unwrap();
        let _ = w.write_all(r);
        let _ = w.flush();
    };
    let mut i = 0;
    while let Some(esc) = bytes[i..].iter().position(|&b| b == 0x1b) {
        let s = &bytes[i + esc..];
        i += esc + 1;
        if s.len() < 2 {
            return;
        }
        match s[1] {
            b'[' => {
                let Some(fin) = s[2..].iter().position(|b| (0x40..=0x7e).contains(b)) else {
                    return;
                };
                let params = &s[2..2 + fin];
                let fin = s[2 + fin];
                let p = std::str::from_utf8(params).unwrap_or("");
                match (p, fin) {
                    ("" | "0", b'c') => reply(b"\x1b[?1;2c"),
                    (">" | ">0", b'c') => reply(b"\x1b[>0;10;0c"),
                    ("=" | "=0", b'c') => reply(b"\x1bP!|00000000\x1b\\"),
                    ("5", b'n') => reply(b"\x1b[0n"),
                    ("6", b'n') => {
                        let (r, c) = parser.lock().unwrap().screen().cursor_position();
                        reply(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
                    }
                    ("?6", b'n') => {
                        let (r, c) = parser.lock().unwrap().screen().cursor_position();
                        reply(format!("\x1b[?{};{};1R", r + 1, c + 1).as_bytes());
                    }
                    (">" | ">0", b'q') => reply(b"\x1bP>|steno 0.1\x1b\\"),
                    ("?", b'u') => reply(b"\x1b[?0u"),
                    ("18", b't') => {
                        let sz = parser.lock().unwrap().screen().size();
                        reply(format!("\x1b[8;{};{}t", sz.0, sz.1).as_bytes());
                    }
                    _ if fin == b'p' && p.starts_with('?') && p.ends_with('$') => {
                        let mode = &p[1..p.len() - 1];
                        reply(format!("\x1b[?{mode};0$y").as_bytes());
                    }
                    _ => {}
                }
            }
            b']' => {
                let end = s[2..]
                    .iter()
                    .position(|&b| b == 0x07 || b == 0x1b)
                    .map(|e| 2 + e)
                    .unwrap_or(s.len());
                match std::str::from_utf8(&s[2..end]).unwrap_or("") {
                    "10;?" => reply(b"\x1b]10;rgb:c0c0/c0c0/c0c0\x07"),
                    "11;?" => reply(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07"),
                    "12;?" => reply(b"\x1b]12;rgb:c0c0/c0c0/c0c0\x07"),
                    osc => {
                        if let Some(n) =
                            osc.strip_prefix("4;").and_then(|r| r.strip_suffix(";?"))
                        {
                            reply(format!("\x1b]4;{n};rgb:8080/8080/8080\x07").as_bytes());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl ClaudePane {
    /// Called every frame with the pane's inner size; resizes only on change.
    pub fn ensure_size(&mut self, rows: u16, cols: u16) {
        if (rows, cols) == self.size || rows < 2 || cols < 10 {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
    }

    pub fn send_key(&self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let mut bytes: Vec<u8> = Vec::new();
        if alt {
            bytes.push(0x1b);
        }
        match key.code {
            KeyCode::Char(c) if ctrl => {
                let b = (c.to_ascii_uppercase() as u8).wrapping_sub(b'@');
                bytes.push(b & 0x7f);
            }
            KeyCode::Char(c) => bytes.extend_from_slice(c.to_string().as_bytes()),
            KeyCode::Enter => bytes.push(b'\r'),
            KeyCode::Backspace => bytes.push(0x7f),
            KeyCode::Tab => bytes.push(b'\t'),
            KeyCode::BackTab => bytes.extend_from_slice(b"\x1b[Z"),
            KeyCode::Esc => bytes.push(0x1b),
            KeyCode::Up => bytes.extend_from_slice(b"\x1b[A"),
            KeyCode::Down => bytes.extend_from_slice(b"\x1b[B"),
            KeyCode::Right => bytes.extend_from_slice(b"\x1b[C"),
            KeyCode::Left => bytes.extend_from_slice(b"\x1b[D"),
            KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
            KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
            KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
            KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
            KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
            _ => return,
        }
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(&bytes);
        let _ = w.flush();
    }

    /// Polite shutdown; the Drop impl hard-kills whatever is left.
    pub fn shutdown(&mut self) {
        {
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(b"/exit\r");
            let _ = w.flush();
        }
        for _ in 0..20 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
