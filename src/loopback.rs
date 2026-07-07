//! System-audio ("Them") capture, per platform:
//! - Linux: `parec -d @DEFAULT_MONITOR@` (PipeWire's PulseAudio layer) emits raw
//!   PCM on stdout, already converted server-side to 16 kHz mono f32le.
//! - Any OS: a VIRTUAL loopback device (BlackHole on macOS, VB-Cable on Windows)
//!   exposes system audio as a normal cpal input — `--loopback-device <name>`.
// ponytail: parec + virtual-device cover v1 everywhere; native backends (macOS
// Core Audio process tap via cidre, Windows WASAPI loopback) are the M5 upgrade
// and slot in behind the same start() signature.

use crate::audio::{AudioChunk, MicHandle, STT_RATE};
use anyhow::{Context, Result};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

pub struct LoopbackHandle {
    pub paused: Arc<AtomicBool>,
    keepalive: Keepalive,
}

enum Keepalive {
    Parec(Child),
    Device(#[allow(dead_code)] MicHandle), // stream stops when dropped
}

impl Drop for LoopbackHandle {
    fn drop(&mut self) {
        if let Keepalive::Parec(child) = &mut self.keepalive {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// `device`: capture a named cpal input (virtual loopback device) — works on any
/// OS. Without it: Linux gets the PulseAudio monitor; macOS/Windows get an error
/// explaining the virtual-device setup.
pub fn start(device: Option<&str>, tx: mpsc::Sender<AudioChunk>) -> Result<LoopbackHandle> {
    if let Some(name) = device {
        let handle = crate::audio::start_input(Some(name), tx)
            .with_context(|| format!("open loopback device '{name}'"))?;
        return Ok(LoopbackHandle {
            paused: handle.paused.clone(),
            keepalive: Keepalive::Device(handle),
        });
    }
    if !cfg!(target_os = "linux") {
        let hint = if cfg!(target_os = "macos") {
            "install BlackHole (https://existential.audio/blackhole), add it to a Multi-Output \
             Device, then run with --loopback-device 'BlackHole 2ch'"
        } else {
            "install VB-Cable (https://vb-audio.com/Cable), set CABLE Input as an output, then \
             run with --loopback-device 'CABLE Output'"
        };
        anyhow::bail!("no native loopback on this OS yet — {hint}");
    }
    start_parec(tx)
}

fn start_parec(tx: mpsc::Sender<AudioChunk>) -> Result<LoopbackHandle> {
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

    Ok(LoopbackHandle { paused, keepalive: Keepalive::Parec(child) })
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
