//! pty-claude — risk-burn-down spike.
//!
//! Proves/disproves: interactive `claude` CLI can run embedded in a PTY we
//! control, rendered through the `vt100` crate (the emulation core used by
//! the `tui-term` ratatui widget).
//!
//! Headless harness, NOT a TUI. See RESULTS.md for findings.
//!
//! Flags:
//!   --out <dir>       output dir (default runs/default)
//!   --no-responder    do not answer terminal queries (still logs them)
//!   --no-roundtrip    skip the model round-trip prompt (saves quota)

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COLS: u16 = 120;
const ROWS: u16 = 35;
const RESIZE_COLS: u16 = 90;
const RESIZE_ROWS: u16 = 25;

// ---------------------------------------------------------------------------
// vt100 callbacks: collect everything vt100 itself does not implement.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Collector {
    unhandled: BTreeMap<String, u64>,
}

impl Collector {
    fn bump(&mut self, key: String) {
        *self.unhandled.entry(key).or_insert(0) += 1;
    }
}

impl vt100::Callbacks for Collector {
    fn unhandled_char(&mut self, _: &mut vt100::Screen, c: char) {
        self.bump(format!("char {c:?}"));
    }
    fn unhandled_control(&mut self, _: &mut vt100::Screen, b: u8) {
        self.bump(format!("ctrl 0x{b:02x}"));
    }
    fn unhandled_escape(
        &mut self,
        _: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        b: u8,
    ) {
        let i1 = i1.map(|c| (c as char).to_string()).unwrap_or_default();
        let i2 = i2.map(|c| (c as char).to_string()).unwrap_or_default();
        self.bump(format!("ESC {i1}{i2}{}", b as char));
    }
    fn unhandled_csi(
        &mut self,
        _: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let i1 = i1.map(|c| (c as char).to_string()).unwrap_or_default();
        let i2 = i2.map(|c| (c as char).to_string()).unwrap_or_default();
        let p: Vec<String> = params
            .iter()
            .map(|sub| {
                sub.iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(":")
            })
            .collect();
        self.bump(format!("CSI {i1}{i2}{} params=[{}]", c, p.join(";")));
    }
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        let head: Vec<String> = params
            .iter()
            .take(2)
            .map(|p| String::from_utf8_lossy(p).chars().take(24).collect())
            .collect();
        self.bump(format!("OSC {}", head.join(";")));
    }
}

// ---------------------------------------------------------------------------
// Terminal-query scanner + responder.
// ---------------------------------------------------------------------------

enum ScanState {
    Ground,
    Esc,
    Csi(Vec<u8>),
    Osc(Vec<u8>),
    OscEsc(Vec<u8>),
    Dcs(Vec<u8>),
    DcsEsc(Vec<u8>),
}

struct Scanner {
    state: ScanState,
    /// inventory of escape-sequence "shapes" seen from the child
    shapes: BTreeMap<String, u64>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Scanner {
    fn new() -> Self {
        Self {
            state: ScanState::Ground,
            shapes: BTreeMap::new(),
        }
    }

    fn shape(&mut self, key: String) {
        *self.shapes.entry(key).or_insert(0) += 1;
    }

    fn feed(&mut self, bytes: &[u8], shared: &Shared) {
        for &b in bytes {
            self.step(b, shared);
        }
    }

    fn step(&mut self, b: u8, shared: &Shared) {
        use ScanState::*;
        // Take state to appease the borrow checker.
        let state = std::mem::replace(&mut self.state, Ground);
        self.state = match state {
            Ground => match b {
                0x1b => Esc,
                _ => Ground,
            },
            Esc => match b {
                b'[' => Csi(Vec::new()),
                b']' => Osc(Vec::new()),
                b'P' => Dcs(Vec::new()),
                0x1b => Esc,
                _ => {
                    self.shape(format!("ESC {}", b as char));
                    Ground
                }
            },
            Csi(mut buf) => match b {
                0x20..=0x3f => {
                    if buf.len() < 64 {
                        buf.push(b);
                    }
                    Csi(buf)
                }
                0x40..=0x7e => {
                    self.on_csi(&buf, b, shared);
                    Ground
                }
                0x1b => Esc,
                _ => Ground, // interleaved C0 control; inventory-only scanner
            },
            Osc(mut buf) => match b {
                0x07 => {
                    self.on_osc(&buf, shared);
                    Ground
                }
                0x1b => OscEsc(buf),
                _ => {
                    if buf.len() < 8192 {
                        buf.push(b);
                    }
                    Osc(buf)
                }
            },
            OscEsc(buf) => {
                self.on_osc(&buf, shared);
                match b {
                    b'\\' => Ground,
                    b'[' => Csi(Vec::new()),
                    b']' => Osc(Vec::new()),
                    b'P' => Dcs(Vec::new()),
                    0x1b => Esc,
                    _ => Ground,
                }
            }
            Dcs(mut buf) => match b {
                0x1b => DcsEsc(buf),
                _ => {
                    if buf.len() < 8192 {
                        buf.push(b);
                    }
                    Dcs(buf)
                }
            },
            DcsEsc(buf) => {
                let head: String = String::from_utf8_lossy(&buf).chars().take(16).collect();
                self.shape(format!("DCS {head}"));
                Ground
            }
        };
    }

    fn on_csi(&mut self, params: &[u8], fin: u8, shared: &Shared) {
        // Inventory shape: non-numeric param bytes + final.
        let prefix: String = params
            .iter()
            .filter(|b| !b.is_ascii_digit() && **b != b';')
            .map(|b| *b as char)
            .collect();
        self.shape(format!("CSI {prefix}{}", fin as char));

        let p = std::str::from_utf8(params).unwrap_or("");
        let full = {
            let mut v = vec![0x1b, b'['];
            v.extend_from_slice(params);
            v.push(fin);
            v
        };
        match (p, fin) {
            // DA1
            ("" | "0", b'c') => shared.answer("DA1", &full, b"\x1b[?1;2c"),
            // DA2
            (">" | ">0", b'c') => shared.answer("DA2", &full, b"\x1b[>0;10;0c"),
            // DA3
            ("=" | "=0", b'c') => shared.answer("DA3", &full, b"\x1bP!|00000000\x1b\\"),
            // DSR device status
            ("5", b'n') => shared.answer("DSR-5 (status)", &full, b"\x1b[0n"),
            // DSR cursor position report
            ("6", b'n') => {
                let (r, c) = shared.cursor_position();
                let reply = format!("\x1b[{};{}R", r + 1, c + 1);
                shared.answer("DSR-6 (CPR)", &full, reply.as_bytes());
            }
            ("?6", b'n') => {
                let (r, c) = shared.cursor_position();
                let reply = format!("\x1b[?{};{};1R", r + 1, c + 1);
                shared.answer("DECXCPR", &full, reply.as_bytes());
            }
            // XTVERSION
            (">" | ">0", b'q') => {
                shared.answer("XTVERSION", &full, b"\x1bP>|mentor-spike 0.1\x1b\\")
            }
            // kitty keyboard protocol query
            ("?", b'u') => shared.answer("kitty-kbd query", &full, b"\x1b[?0u"),
            // XTWINOPS queries
            ("14", b't') => shared.answer(
                "XTWINOPS-14 (pixel size)",
                &full,
                format!(
                    "\x1b[4;{};{}t",
                    shared.rows() as u32 * 20,
                    shared.cols() as u32 * 10
                )
                .as_bytes(),
            ),
            ("16", b't') => shared.answer("XTWINOPS-16 (cell size)", &full, b"\x1b[6;20;10t"),
            ("18", b't') => shared.answer(
                "XTWINOPS-18 (text size)",
                &full,
                format!("\x1b[8;{};{}t", shared.rows(), shared.cols()).as_bytes(),
            ),
            // DECRQM mode queries (ESC[?<mode>$p)
            _ if fin == b'p' && p.starts_with('?') && p.ends_with('$') => {
                let mode = &p[1..p.len() - 1];
                let reply = format!("\x1b[?{mode};0$y"); // 0 = not recognized
                shared.answer(&format!("DECRQM ?{mode}"), &full, reply.as_bytes());
            }
            _ => {}
        }
    }

    fn on_osc(&mut self, data: &[u8], shared: &Shared) {
        let s = String::from_utf8_lossy(data);
        let cmd: String = s.chars().take_while(|c| *c != ';').collect();
        self.shape(format!("OSC {cmd}"));
        let full = {
            let mut v = vec![0x1b, b']'];
            v.extend_from_slice(data);
            v.push(0x07);
            v
        };
        match s.as_ref() {
            "10;?" => shared.answer("OSC 10 fg query", &full, b"\x1b]10;rgb:c0c0/c0c0/c0c0\x07"),
            "11;?" => shared.answer("OSC 11 bg query", &full, b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07"),
            "12;?" => shared.answer(
                "OSC 12 cursor-color query",
                &full,
                b"\x1b]12;rgb:c0c0/c0c0/c0c0\x07",
            ),
            _ => {
                // OSC 4;N;? palette query
                if let Some(rest) = s.strip_prefix("4;") {
                    if let Some(n) = rest.strip_suffix(";?") {
                        let reply = format!("\x1b]4;{n};rgb:8080/8080/8080\x07");
                        shared.answer(&format!("OSC 4;{n} palette query"), &full, reply.as_bytes());
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared state between reader thread and main.
// ---------------------------------------------------------------------------

struct Shared {
    parser: Mutex<vt100::Parser<Collector>>,
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    queries_log: Mutex<std::fs::File>,
    bytes_from_child: AtomicU64,
    responder: bool,
    t0: Instant,
    size: Mutex<(u16, u16)>, // rows, cols
}

impl Shared {
    fn cursor_position(&self) -> (u16, u16) {
        self.parser.lock().unwrap().screen().cursor_position()
    }
    fn rows(&self) -> u16 {
        self.size.lock().unwrap().0
    }
    fn cols(&self) -> u16 {
        self.size.lock().unwrap().1
    }
    fn contents(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    fn log_query(&self, line: &str) {
        let t = self.t0.elapsed().as_secs_f64();
        let mut f = self.queries_log.lock().unwrap();
        let _ = writeln!(f, "[{t:9.3}s] {line}");
        let _ = f.flush();
    }

    /// Log a query; reply unless --no-responder.
    fn answer(&self, name: &str, query_raw: &[u8], reply: &[u8]) {
        if self.responder {
            self.log_query(&format!(
                "QUERY {name} raw={} -> REPLY {} ({:?})",
                hex(query_raw),
                hex(reply),
                String::from_utf8_lossy(reply)
            ));
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(reply);
            let _ = w.flush();
        } else {
            self.log_query(&format!(
                "QUERY {name} raw={} -> SUPPRESSED (--no-responder)",
                hex(query_raw)
            ));
        }
    }

    fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Snapshotting
// ---------------------------------------------------------------------------

struct Snapshotter {
    dir: PathBuf,
    n: u32,
    last: String,
    t0: Instant,
}

impl Snapshotter {
    /// Write a snapshot if the screen changed; returns current contents.
    fn tick(&mut self, shared: &Shared, label: &str) -> String {
        let contents = shared.contents();
        if contents != self.last {
            self.n += 1;
            let path = self.dir.join(format!("{:03}.txt", self.n));
            let hdr = format!(
                "# t=+{:.2}s phase={} size={}x{}\n",
                self.t0.elapsed().as_secs_f64(),
                label,
                shared.cols(),
                shared.rows()
            );
            let _ = std::fs::write(&path, format!("{hdr}{contents}\n"));
            self.last = contents.clone();
        }
        contents
    }
}

// ---------------------------------------------------------------------------
// Child RSS via /proc (child + direct descendants)
// ---------------------------------------------------------------------------

fn rss_kb_tree(root: u32) -> u64 {
    let mut pids = vec![root];
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
                let ppid = stat
                    .lines()
                    .find_map(|l| l.strip_prefix("PPid:"))
                    .and_then(|v| v.trim().parse::<u32>().ok());
                if ppid == Some(root) {
                    pids.push(pid);
                }
            }
        }
    }
    let mut total = 0;
    for pid in pids {
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
            if let Some(v) = stat.lines().find_map(|l| l.strip_prefix("VmRSS:")) {
                total += v
                    .trim()
                    .trim_end_matches(" kB")
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(0);
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let responder = !args.iter().any(|a| a == "--no-responder");
    let roundtrip = !args.iter().any(|a| a == "--no-roundtrip");
    let out: PathBuf = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("runs/default"));

    let snap_dir = out.join("snapshots");
    std::fs::create_dir_all(&snap_dir)?;
    let raw_log_path = out.join("raw.log");
    let mut raw_log = std::fs::File::create(&raw_log_path)?;
    let queries_log = std::fs::File::create(out.join("queries.log"))?;
    let mut report = std::fs::File::create(out.join("run_summary.txt"))?;

    macro_rules! note {
        ($($t:tt)*) => {{
            let line = format!($($t)*);
            println!("{line}");
            let _ = writeln!(report, "{line}");
            let _ = report.flush();
        }};
    }

    note!(
        "pty-claude spike run: responder={} roundtrip={} out={}",
        responder,
        roundtrip,
        out.display()
    );

    // --- PTY + child ------------------------------------------------------
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;

    let mut cmd = CommandBuilder::new("claude");
    cmd.arg("--model");
    cmd.arg("haiku");
    cmd.cwd("/home/banavasi/workspaces/personal/voice-mentor");
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // Strip nested-session markers so claude doesn't detect it's inside claude.
    for (k, _) in std::env::vars() {
        if k.starts_with("CLAUDE") || k == "AI_AGENT" {
            cmd.env_remove(&k);
        }
    }
    cmd.env_remove("CLAUDE_CODE_SSE_PORT"); // explicit even if unset

    let t0 = Instant::now();
    let mut child = pair.slave.spawn_command(cmd).context("spawn claude")?;
    drop(pair.slave);
    let child_pid = child.process_id();
    note!("[{:7.2}s] spawned claude pid={:?}", 0.0, child_pid);

    let mut reader = pair.master.try_clone_reader().context("clone reader")?;
    let writer = pair.master.take_writer().context("take writer")?;

    let shared = Arc::new(Shared {
        parser: Mutex::new(vt100::Parser::new_with_callbacks(
            ROWS,
            COLS,
            0,
            Collector::default(),
        )),
        writer: Mutex::new(writer),
        queries_log: Mutex::new(queries_log),
        bytes_from_child: AtomicU64::new(0),
        responder,
        t0,
        size: Mutex::new((ROWS, COLS)),
    });

    // --- reader thread ------------------------------------------------------
    let rshared = Arc::clone(&shared);
    let reader_handle = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut scanner = Scanner::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    let _ = raw_log.write_all(chunk);
                    let _ = raw_log.flush();
                    rshared
                        .bytes_from_child
                        .fetch_add(n as u64, Ordering::Relaxed);
                    // Feed vt100 first so DSR-6 replies use the current cursor.
                    rshared.parser.lock().unwrap().process(chunk);
                    scanner.feed(chunk, &rshared);
                }
            }
        }
        scanner.shapes
    });

    let mut snaps = Snapshotter {
        dir: snap_dir,
        n: 0,
        last: String::new(),
        t0,
    };

    // --- phase 1: wait for ready -------------------------------------------
    let ready_deadline = Instant::now() + Duration::from_secs(90);
    let mut ready_at: Option<f64> = None;
    let mut enters_sent = 0u32;
    let mut verdict = String::from("UNKNOWN");
    let mut last_dialog_nudge = Instant::now() - Duration::from_secs(10);

    while Instant::now() < ready_deadline {
        std::thread::sleep(Duration::from_millis(200));
        let contents = snaps.tick(&shared, "startup");
        let lower = contents.to_lowercase();

        if lower.contains("select login method")
            || lower.contains("browser to log in")
            || lower.contains("please log in")
            || lower.contains("run /login")
        {
            verdict = "NEEDS_LOGIN".into();
            note!(
                "[{:7.2}s] login screen detected -> NEEDS_LOGIN",
                t0.elapsed().as_secs_f64()
            );
            break;
        }

        // Dialogs first: theme picker / trust / update notices (these can
        // contain a "❯" selection pointer, so they must be checked before
        // the ready markers).
        let dialog = lower.contains("choose the text style")
            || lower.contains("do you trust the files")
            || lower.contains("press enter to continue")
            || (lower.contains("dark mode") && lower.contains("light mode"));
        if !dialog {
            // Ready markers (claude v2.1.202): "❯ " prompt + "idle" status
            // row; older builds show a "? for shortcuts" footer.
            if contents.contains("? for shortcuts")
                || (contents.contains('❯') && contents.contains("idle"))
            {
                ready_at = Some(t0.elapsed().as_secs_f64());
                note!("[{:7.2}s] UI ready", ready_at.unwrap());
                break;
            }
        }
        if dialog && enters_sent < 2 && last_dialog_nudge.elapsed() > Duration::from_secs(2) {
            note!(
                "[{:7.2}s] dialog detected, pressing Enter. screen:\n{}",
                t0.elapsed().as_secs_f64(),
                contents
            );
            shared.send(b"\r")?;
            enters_sent += 1;
            last_dialog_nudge = Instant::now();
        }
    }

    let mut roundtrip_result = String::from("SKIPPED");
    let mut resize_notes = String::new();
    let mut rss_note = String::new();

    if verdict == "NEEDS_LOGIN" {
        // fall through to shutdown
    } else if ready_at.is_none() {
        verdict = "NOT_READY_TIMEOUT".into();
        note!(
            "[{:7.2}s] UI never looked ready within 90s. Last screen:\n{}",
            t0.elapsed().as_secs_f64(),
            shared.contents()
        );
    } else {
        if let Some(pid) = child_pid {
            let rss = rss_kb_tree(pid);
            rss_note = format!("{:.1} MB", rss as f64 / 1024.0);
            note!(
                "[{:7.2}s] child RSS (tree): {}",
                t0.elapsed().as_secs_f64(),
                rss_note
            );
        }

        // --- phase 2: round-trip ------------------------------------------
        if roundtrip {
            note!(
                "[{:7.2}s] sending round-trip prompt",
                t0.elapsed().as_secs_f64()
            );
            shared.send(b"Reply with exactly PTY_SPIKE_OK and nothing else.")?;
            std::thread::sleep(Duration::from_millis(400));
            snaps.tick(&shared, "typed");
            shared.send(b"\r")?;

            let rt_deadline = Instant::now() + Duration::from_secs(180);
            roundtrip_result = "TIMEOUT".into();
            while Instant::now() < rt_deadline {
                std::thread::sleep(Duration::from_millis(200));
                let contents = snaps.tick(&shared, "roundtrip");
                let got = contents.lines().any(|l| {
                    l.contains("PTY_SPIKE_OK")
                        && !l.contains("Reply with")
                        && !l.trim_start().starts_with('>')
                        && !l.trim_start().starts_with('❯')
                });
                if got {
                    roundtrip_result = format!("OK at +{:.2}s", t0.elapsed().as_secs_f64());
                    note!("[{:7.2}s] round-trip OK", t0.elapsed().as_secs_f64());
                    break;
                }
            }
            if roundtrip_result == "TIMEOUT" {
                note!(
                    "[{:7.2}s] round-trip TIMED OUT. screen:\n{}",
                    t0.elapsed().as_secs_f64(),
                    shared.contents()
                );
            }
            // let the UI settle
            std::thread::sleep(Duration::from_secs(2));
            snaps.tick(&shared, "post-roundtrip");
        }

        // --- phase 3: resize ------------------------------------------------
        note!(
            "[{:7.2}s] resizing PTY {}x{} -> {}x{}",
            t0.elapsed().as_secs_f64(),
            COLS,
            ROWS,
            RESIZE_COLS,
            RESIZE_ROWS
        );
        let bytes_before = shared.bytes_from_child.load(Ordering::Relaxed);
        pair.master
            .resize(PtySize {
                rows: RESIZE_ROWS,
                cols: RESIZE_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize")?;
        {
            let mut p = shared.parser.lock().unwrap();
            p.screen_mut().set_size(RESIZE_ROWS, RESIZE_COLS);
        }
        *shared.size.lock().unwrap() = (RESIZE_ROWS, RESIZE_COLS);

        std::thread::sleep(Duration::from_secs(3));
        snaps.tick(&shared, "post-resize");
        let bytes_after = shared.bytes_from_child.load(Ordering::Relaxed);
        let repaint_bytes = bytes_after - bytes_before;
        let contents = shared.contents();
        let repainted = repaint_bytes > 200
            && (contents.contains('❯') || contents.contains("? for shortcuts"));
        resize_notes = format!(
            "repaint bytes within 3s: {repaint_bytes}; screen has input box after resize: {repainted}"
        );
        note!(
            "[{:7.2}s] resize: {}",
            t0.elapsed().as_secs_f64(),
            resize_notes
        );

        if !repainted {
            note!(
                "[{:7.2}s] screen looks stale after resize, nudging with Ctrl+L",
                t0.elapsed().as_secs_f64()
            );
            shared.send(b"\x0c")?;
            std::thread::sleep(Duration::from_secs(2));
            snaps.tick(&shared, "post-resize-nudge");
            let contents = shared.contents();
            let fixed = contents.contains('❯') || contents.contains("? for shortcuts");
            resize_notes.push_str(&format!("; after Ctrl+L nudge, input box visible: {fixed}"));
            note!(
                "[{:7.2}s] post-nudge: input box visible: {fixed}",
                t0.elapsed().as_secs_f64()
            );
        }

        verdict = "RAN".into();
    }

    // --- phase 4: shutdown --------------------------------------------------
    note!("[{:7.2}s] sending /exit", t0.elapsed().as_secs_f64());
    let _ = shared.send(b"/exit\r");
    let exit_deadline = Instant::now() + Duration::from_secs(20);
    let mut exited = false;
    while Instant::now() < exit_deadline {
        if let Ok(Some(status)) = child.try_wait() {
            note!(
                "[{:7.2}s] child exited: {:?}",
                t0.elapsed().as_secs_f64(),
                status
            );
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        snaps.tick(&shared, "shutdown");
    }
    if !exited {
        note!(
            "[{:7.2}s] child did not exit after /exit; killing",
            t0.elapsed().as_secs_f64()
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    // Drop master so reader hits EOF.
    drop(pair.master);
    let shapes = reader_handle.join().unwrap_or_default();

    // --- write sequence inventory + vt100 unhandled report ------------------
    {
        let mut f = std::fs::File::create(out.join("sequences.log"))?;
        writeln!(f, "# Escape-sequence shape inventory (from child output)")?;
        writeln!(f, "# shape -> count")?;
        for (k, v) in &shapes {
            writeln!(f, "{k:40} {v}")?;
        }
        writeln!(f)?;
        writeln!(f, "# vt100 0.16.2 UNHANDLED sequences (Callbacks hooks)")?;
        let parser = shared.parser.lock().unwrap();
        let unhandled = &parser.callbacks().unhandled;
        if unhandled.is_empty() {
            writeln!(f, "(none — vt100 handled everything it saw)")?;
        }
        for (k, v) in unhandled {
            writeln!(f, "{k:60} {v}")?;
        }
    }

    // --- final summary --------------------------------------------------------
    note!("---");
    note!("verdict-data:");
    note!("  run mode: responder={responder} roundtrip={roundtrip}");
    note!(
        "  ready: {}",
        ready_at
            .map(|t| format!("+{t:.2}s"))
            .unwrap_or_else(|| "never".into())
    );
    note!("  round-trip: {roundtrip_result}");
    note!("  resize: {resize_notes}");
    note!("  child RSS at ready: {rss_note}");
    note!("  child exited cleanly on /exit: {exited}");
    note!("  status: {verdict}");

    Ok(())
}
