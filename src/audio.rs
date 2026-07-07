use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

pub const STT_RATE: usize = 16_000;

/// ~100 ms of 16 kHz mono audio + its level, sent to the app/STT pipeline.
pub struct AudioChunk {
    pub pcm: Vec<f32>,
    pub rms_db: f32,
}

pub struct MicHandle {
    pub paused: Arc<AtomicBool>,
    _stream: cpal::Stream, // kept alive; dropping stops capture
}

/// Default mic capture.
pub fn start_mic(tx: mpsc::Sender<AudioChunk>) -> Result<MicHandle> {
    start_input(None, tx)
}

/// Open an input device (default mic, or one matched by name — e.g. a virtual
/// loopback device like "BlackHole 2ch"), capture at its native config, downmix
/// to mono, resample to 16 kHz, and ship ~100 ms chunks. The callback only pushes
/// into a lock-free ring buffer; a drain thread does the DSP so the audio thread
/// never blocks.
pub fn start_input(device_name: Option<&str>, tx: mpsc::Sender<AudioChunk>) -> Result<MicHandle> {
    let host = cpal::default_host();
    let device = match device_name {
        None => host
            .default_input_device()
            .context("no default input device (mic)")?,
        Some(name) => {
            let needle = name.to_lowercase();
            host.input_devices()
                .context("enumerate input devices")?
                .find(|d| {
                    d.description()
                        .map(|desc| desc.name().to_lowercase().contains(&needle))
                        .unwrap_or(false)
                })
                .with_context(|| format!("no input device matching '{name}'"))?
        }
    };
    let supported = device.default_input_config().context("mic config")?;
    let in_rate = supported.sample_rate() as usize;
    let channels = supported.channels() as usize;

    let (mut prod, mut cons) = rtrb::RingBuffer::<f32>::new(in_rate * channels); // 1 s
    let paused = Arc::new(AtomicBool::new(false));
    let cb_paused = paused.clone();

    let stream = device
        .build_input_stream(
            supported.into(),
            move |data: &[f32], _| {
                if cb_paused.load(Ordering::Relaxed) {
                    return;
                }
                for &s in data {
                    if prod.push(s).is_err() {
                        break; // overrun: drop rather than block the audio thread
                    }
                }
            },
            |e| eprintln!("mic stream error: {e}"),
            None,
        )
        .context("build mic stream")?;
    stream.play()?;

    std::thread::Builder::new()
        .name("mic-drain".into())
        .spawn(move || drain_loop(&mut cons, in_rate, channels, tx))?;

    Ok(MicHandle { paused, _stream: stream })
}

fn drain_loop(
    cons: &mut rtrb::Consumer<f32>,
    in_rate: usize,
    channels: usize,
    tx: mpsc::Sender<AudioChunk>,
) {
    let chunk_in = in_rate / 10; // 100 ms of mono input
    let mut mono: Vec<f32> = Vec::with_capacity(chunk_in);
    let mut frame: Vec<f32> = Vec::with_capacity(channels);

    loop {
        while mono.len() < chunk_in {
            match cons.pop() {
                Ok(s) => {
                    frame.push(s);
                    if frame.len() == channels {
                        mono.push(frame.iter().sum::<f32>() / channels as f32);
                        frame.clear();
                    }
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        let pcm = resample_to_16k(&mono, in_rate);
        mono.clear();
        let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len().max(1) as f32).sqrt();
        let chunk = AudioChunk { pcm, rms_db: 20.0 * rms.max(1e-9).log10() };
        if tx.blocking_send(chunk).is_err() {
            return; // app gone; end thread
        }
    }
}

/// Mono resampler to 16 kHz. Integer ratios (48k, 32k) get box-average decimation
/// (gentle anti-alias); everything else gets linear interpolation.
// ponytail: good enough for speech STT; sherpa's accept_waveform may even resample
// internally — revisit after the M0 spike, upgrade to a windowed-sinc if WER suffers.
pub fn resample_to_16k(input: &[f32], in_rate: usize) -> Vec<f32> {
    if in_rate == STT_RATE {
        return input.to_vec();
    }
    if in_rate.is_multiple_of(STT_RATE) {
        let n = in_rate / STT_RATE;
        return input
            .chunks_exact(n)
            .map(|c| c.iter().sum::<f32>() / n as f32)
            .collect();
    }
    let ratio = in_rate as f64 / STT_RATE as f64;
    let out_len = (input.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos as usize;
            let frac = (pos - j as f64) as f32;
            let a = input[j.min(input.len() - 1)];
            let b = input[(j + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_halves_48k_by_3() {
        let input: Vec<f32> = (0..4800).map(|i| (i % 3) as f32).collect(); // 100ms @48k
        let out = resample_to_16k(&input, 48_000);
        assert_eq!(out.len(), 1600);
        assert!((out[0] - 1.0).abs() < 1e-6); // avg of 0,1,2
    }

    #[test]
    fn resample_44k1_linear_length() {
        let input = vec![0.5f32; 4410];
        let out = resample_to_16k(&input, 44_100);
        assert_eq!(out.len(), 1600);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }
}
