//! System-audio ("Them") capture on Linux via the PulseAudio layer of PipeWire:
//! `parec -d @DEFAULT_MONITOR@` emits raw PCM on stdout, already converted
//! server-side to 16 kHz mono f32le — no client DSP needed.
// ponytail: subprocess parec covers whole-system loopback for v1; move to
// pipewire-rs node targeting when v2 per-app capture lands. macOS/Windows
// backends (cidre tap / WASAPI) slot in behind the same start() signature.

use crate::audio::{AudioChunk, STT_RATE};
use anyhow::{Context, Result};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

pub struct LoopbackHandle {
    pub paused: Arc<AtomicBool>,
    child: Child,
}

impl Drop for LoopbackHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn start(tx: mpsc::Sender<AudioChunk>) -> Result<LoopbackHandle> {
    let mut child = Command::new("parec")
        .args([
            "-d",
            "@DEFAULT_MONITOR@",
            "--rate",
            &STT_RATE.to_string(),
            "--channels=1",
            "--format=float32le",
            "--latency-msec=50",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn parec (PulseAudio/PipeWire monitor capture)")?;
    let mut out = child.stdout.take().context("parec stdout")?;

    let paused = Arc::new(AtomicBool::new(false));
    let thread_paused = paused.clone();

    std::thread::Builder::new()
        .name("loopback-drain".into())
        .spawn(move || {
            let mut buf = vec![0u8; STT_RATE / 10 * 4]; // 100 ms of f32le
            loop {
                let mut filled = 0;
                while filled < buf.len() {
                    match out.read(&mut buf[filled..]) {
                        Ok(0) => return, // parec died / sink gone
                        Ok(n) => filled += n,
                        Err(_) => return,
                    }
                }
                if thread_paused.load(Ordering::Relaxed) {
                    continue; // keep draining so the pipe doesn't back up
                }
                let pcm: Vec<f32> = buf
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                let rms =
                    (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len().max(1) as f32).sqrt();
                let chunk = AudioChunk { pcm, rms_db: 20.0 * rms.max(1e-9).log10() };
                if tx.blocking_send(chunk).is_err() {
                    return;
                }
            }
        })?;

    Ok(LoopbackHandle { paused, child })
}

/// Best-effort headphones check for the echo gate: inspect the default sink's
/// active port. Speakers → warn that Me/Them labels will degrade without AEC.
pub fn default_output_is_headphones() -> Option<bool> {
    let out = Command::new("pactl")
        .args(["list", "sinks"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let default = Command::new("pactl").args(["get-default-sink"]).output().ok()?;
    let default = String::from_utf8_lossy(&default.stdout).trim().to_string();
    let mut in_default = false;
    for line in text.lines() {
        let l = line.trim();
        if let Some(name) = l.strip_prefix("Name: ") {
            in_default = name == default;
        }
        if in_default && l.starts_with("Active Port:") {
            let port = l.to_lowercase();
            return Some(port.contains("headphone") || port.contains("headset"));
        }
    }
    None
}
