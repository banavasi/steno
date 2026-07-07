//! stt-bench — risk-burn-down spike for voice-mentor.
//!
//! Proves local streaming STT viability: sherpa-onnx official Rust bindings
//! running NVIDIA Nemotron Speech Streaming EN 0.6B (int8 ONNX) as concurrent
//! streaming recognizer streams, with real-time factor measured.
//!
//! Modes:
//!   --make-fixture            build fixtures/bench-60s.wav (>=60s) from model test_wavs
//!   --bench                   full-speed concurrent-stream benchmark
//!       [--streams N]         number of concurrent streams (default 2)
//!       [--num-threads N]     ONNX intra-op threads per recognizer (default 4)
//!       [--separate]          one recognizer PER stream (default: one shared recognizer)
//!       [--fixture PATH]      wav to feed (default fixtures/bench-60s.wav)
//!   --live [PATH]             real-time-paced single stream, prints evolving partials
//!       [--num-threads N]
//!   --transcribe PATH         full-speed single stream, prints final text (for WER eyeball)

use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, Wave};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

const MODEL_DIR: &str = "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14";
const SAMPLE_RATE: i32 = 16000;
const CHUNK_MS: usize = 100;
const CHUNK_SAMPLES: usize = (SAMPLE_RATE as usize) * CHUNK_MS / 1000; // 1600

fn make_config(num_threads: i32, debug: bool) -> OnlineRecognizerConfig {
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.transducer.encoder = Some(format!("{MODEL_DIR}/encoder.int8.onnx"));
    config.model_config.transducer.decoder = Some(format!("{MODEL_DIR}/decoder.int8.onnx"));
    config.model_config.transducer.joiner = Some(format!("{MODEL_DIR}/joiner.int8.onnx"));
    config.model_config.tokens = Some(format!("{MODEL_DIR}/tokens.txt"));
    config.model_config.num_threads = num_threads;
    config.model_config.provider = Some("cpu".into());
    config.model_config.debug = debug;
    config.decoding_method = Some("greedy_search".into());
    // Endpointing: defaults from sherpa-onnx docs/examples.
    config.enable_endpoint = true;
    config.rule1_min_trailing_silence = 2.4;
    config.rule2_min_trailing_silence = 1.2;
    config.rule3_min_utterance_length = 20.0;
    config
}

/// Read a field like VmRSS/VmHWM (kB) from /proc/self/status.
fn proc_status_kb(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let rest = rest.trim_start_matches(':').trim();
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}

fn rss_report(label: &str) {
    let rss = proc_status_kb("VmRSS").unwrap_or(0);
    let hwm = proc_status_kb("VmHWM").unwrap_or(0);
    println!(
        "[mem] {label}: VmRSS = {:.2} GiB ({rss} kB), VmHWM (peak) = {:.2} GiB ({hwm} kB)",
        rss as f64 / 1024.0 / 1024.0,
        hwm as f64 / 1024.0 / 1024.0,
    );
}

fn load_fixture(path: &str) -> (Vec<f32>, f64) {
    let wave = Wave::read(path).unwrap_or_else(|| panic!("cannot read wav: {path}"));
    assert_eq!(
        wave.sample_rate(),
        SAMPLE_RATE,
        "fixture must be 16 kHz: {path}"
    );
    let samples = wave.samples().to_vec();
    let secs = samples.len() as f64 / SAMPLE_RATE as f64;
    (samples, secs)
}

fn make_fixture() {
    // Concatenate the model repo's 16 kHz test wavs (0.wav ~ "AFTER EARLY NIGHTFALL...",
    // 1.wav ~ "GOD AS A DIRECT CONSEQUENCE...") with 0.5 s of silence between
    // utterances, looped until >= 60 s. 8k.wav is excluded (8 kHz).
    let (w0, s0) = load_fixture(&format!("{MODEL_DIR}/test_wavs/0.wav"));
    let (w1, s1) = load_fixture(&format!("{MODEL_DIR}/test_wavs/1.wav"));
    println!("0.wav = {s0:.2}s, 1.wav = {s1:.2}s");

    let silence = vec![0.0f32; SAMPLE_RATE as usize / 2]; // 0.5 s
    let mut out: Vec<f32> = Vec::new();
    while (out.len() as f64) < 60.0 * SAMPLE_RATE as f64 {
        out.extend_from_slice(&w0);
        out.extend_from_slice(&silence);
        out.extend_from_slice(&w1);
        out.extend_from_slice(&silence);
    }
    std::fs::create_dir_all("fixtures").unwrap();
    let path = "fixtures/bench-60s.wav";
    assert!(sherpa_onnx::write(path, &out, SAMPLE_RATE));
    println!(
        "wrote {path}: {:.2}s ({} samples)",
        out.len() as f64 / SAMPLE_RATE as f64,
        out.len()
    );
}

struct StreamStats {
    audio_secs: f64,
    wall_secs: f64,
    final_text: String,
}

/// Feed `samples` full-speed in 100ms chunks into a fresh stream of `rec`,
/// decoding as we go. Returns per-stream stats.
fn run_stream_full_speed(rec: &OnlineRecognizer, samples: &[f32]) -> StreamStats {
    let stream = rec.create_stream();
    let start = Instant::now();
    for chunk in samples.chunks(CHUNK_SAMPLES) {
        stream.accept_waveform(SAMPLE_RATE, chunk);
        while rec.is_ready(&stream) {
            rec.decode(&stream);
        }
    }
    // Streaming encoders need trailing silence to flush their right-context;
    // sherpa-onnx examples append ~0.8 s of tail padding before InputFinished.
    let tail = vec![0.0f32; (SAMPLE_RATE as usize) * 8 / 10];
    stream.accept_waveform(SAMPLE_RATE, &tail);
    stream.input_finished();
    while rec.is_ready(&stream) {
        rec.decode(&stream);
    }
    let wall = start.elapsed();
    let text = rec
        .get_result(&stream)
        .map(|r| r.text)
        .unwrap_or_default();
    StreamStats {
        audio_secs: samples.len() as f64 / SAMPLE_RATE as f64,
        wall_secs: wall.as_secs_f64(),
        final_text: text,
    }
}

fn bench(streams: usize, num_threads: i32, separate: bool, fixture: &str) {
    println!(
        "== bench: {streams} stream(s), num_threads={num_threads}, {} recognizer(s), fixture={fixture} ==",
        if separate { streams } else { 1 }
    );
    let (samples, audio_secs) = load_fixture(fixture);
    println!("fixture: {audio_secs:.2}s of 16 kHz mono audio");
    rss_report("before model load");

    // Load recognizer(s), timing each load.
    let n_recs = if separate { streams } else { 1 };
    let mut recs: Vec<Arc<OnlineRecognizer>> = Vec::new();
    for i in 0..n_recs {
        let t0 = Instant::now();
        let rec = OnlineRecognizer::create(&make_config(num_threads, false))
            .expect("failed to create OnlineRecognizer");
        println!("[load] recognizer {i}: {:.2}s", t0.elapsed().as_secs_f64());
        recs.push(Arc::new(rec));
        rss_report(&format!("after recognizer {i} load"));
    }

    // Warm-up: one short pass so first-inference lazy init doesn't skew stream 0.
    {
        let warm = &samples[..(SAMPLE_RATE as usize * 2).min(samples.len())];
        for rec in &recs {
            let _ = run_stream_full_speed(rec, warm);
        }
    }

    let barrier = Arc::new(Barrier::new(streams));
    let overall_start = Arc::new(std::sync::OnceLock::<Instant>::new());
    let done_ns = Arc::new(AtomicU64::new(0));

    let stats: Vec<StreamStats> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for i in 0..streams {
            let rec = recs[if separate { i } else { 0 }].clone();
            let samples = &samples;
            let barrier = barrier.clone();
            let overall_start = overall_start.clone();
            let done_ns = done_ns.clone();
            handles.push(scope.spawn(move || {
                barrier.wait();
                let t0 = *overall_start.get_or_init(Instant::now);
                let s = run_stream_full_speed(&rec, samples);
                done_ns.fetch_max(t0.elapsed().as_nanos() as u64, Ordering::SeqCst);
                s
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let overall_wall = done_ns.load(Ordering::SeqCst) as f64 / 1e9;
    rss_report("after bench");

    let mut total_audio = 0.0;
    for (i, s) in stats.iter().enumerate() {
        let rtf = s.audio_secs / s.wall_secs;
        total_audio += s.audio_secs;
        println!(
            "[stream {i}] audio={:.2}s wall={:.2}s speed={rtf:.2}x realtime",
            s.audio_secs, s.wall_secs
        );
        let tail: String = s.final_text.chars().rev().take(80).collect::<Vec<_>>().iter().rev().collect();
        println!("[stream {i}] final text tail: ...{tail}");
    }
    println!(
        "[combined] audio={total_audio:.2}s overall_wall={overall_wall:.2}s combined_speed={:.2}x realtime",
        total_audio / overall_wall
    );
    println!(
        "RESULT bench streams={streams} num_threads={num_threads} separate={separate} combined_x={:.2}",
        total_audio / overall_wall
    );
}

fn live(path: &str, num_threads: i32) {
    println!("== live: real-time paced, {path}, num_threads={num_threads} ==");
    let (samples, audio_secs) = load_fixture(path);
    println!("audio: {audio_secs:.2}s");

    let t0 = Instant::now();
    let rec = OnlineRecognizer::create(&make_config(num_threads, false))
        .expect("failed to create OnlineRecognizer");
    println!("[load] {:.2}s", t0.elapsed().as_secs_f64());

    let stream = rec.create_stream();
    let mut last_text = String::new();
    let mut segment_no = 0;
    let feed_start = Instant::now();

    for (ci, chunk) in samples.chunks(CHUNK_SAMPLES).enumerate() {
        // Real-time pacing: chunk ci should be fed at t = ci * 100ms.
        let target = Duration::from_millis((ci * CHUNK_MS) as u64);
        let now = feed_start.elapsed();
        if target > now {
            std::thread::sleep(target - now);
        }
        stream.accept_waveform(SAMPLE_RATE, chunk);
        let decode_t0 = Instant::now();
        while rec.is_ready(&stream) {
            rec.decode(&stream);
        }
        let decode_ms = decode_t0.elapsed().as_secs_f64() * 1000.0;

        if let Some(r) = rec.get_result(&stream) {
            if r.text != last_text {
                let t = feed_start.elapsed().as_secs_f64();
                println!("[{t:6.2}s] (decode {decode_ms:5.1}ms) partial: {}", r.text);
                last_text = r.text.clone();
            }
        }

        if rec.is_endpoint(&stream) {
            if let Some(r) = rec.get_result(&stream) {
                if !r.text.is_empty() {
                    let ts = r
                        .timestamps
                        .as_ref()
                        .map(|t| {
                            format!(
                                "tokens {:.2}s..{:.2}s",
                                t.first().copied().unwrap_or(0.0),
                                t.last().copied().unwrap_or(0.0)
                            )
                        })
                        .unwrap_or_else(|| "no timestamps".into());
                    println!(
                        ">>> FINAL segment {segment_no} [{ts}] (is_final={}): {}",
                        r.is_final, r.text
                    );
                    segment_no += 1;
                }
            }
            rec.reset(&stream);
            last_text.clear();
        }
        std::io::stdout().flush().ok();
    }

    let tail = vec![0.0f32; (SAMPLE_RATE as usize) * 8 / 10];
    stream.accept_waveform(SAMPLE_RATE, &tail);
    stream.input_finished();
    while rec.is_ready(&stream) {
        rec.decode(&stream);
    }
    if let Some(r) = rec.get_result(&stream) {
        if !r.text.is_empty() {
            println!(">>> FINAL segment {segment_no} (tail flush): {}", r.text);
        }
    }
}

fn transcribe(path: &str, num_threads: i32) {
    let (samples, audio_secs) = load_fixture(path);
    let rec = OnlineRecognizer::create(&make_config(num_threads, false))
        .expect("failed to create OnlineRecognizer");
    let s = run_stream_full_speed(&rec, &samples);
    println!("audio={audio_secs:.2}s wall={:.2}s", s.wall_secs);
    println!("TEXT: {}", s.final_text);
    // Also dump token timestamps availability.
    let stream = rec.create_stream();
    stream.accept_waveform(SAMPLE_RATE, &samples);
    let tail = vec![0.0f32; (SAMPLE_RATE as usize) * 8 / 10];
    stream.accept_waveform(SAMPLE_RATE, &tail);
    stream.input_finished();
    while rec.is_ready(&stream) {
        rec.decode(&stream);
    }
    if let Some(r) = rec.get_result(&stream) {
        let n = r.timestamps.as_ref().map(|t| t.len()).unwrap_or(0);
        println!(
            "tokens={} timestamps={} first_ts={:?} last_ts={:?}",
            r.tokens.len(),
            n,
            r.timestamps.as_ref().and_then(|t| t.first()),
            r.timestamps.as_ref().and_then(|t| t.last()),
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get_flag = |name: &str| args.iter().any(|a| a == name);
    let get_val = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let num_threads: i32 = get_val("--num-threads")
        .map(|v| v.parse().expect("--num-threads N"))
        .unwrap_or(4);

    if get_flag("--make-fixture") {
        make_fixture();
    } else if get_flag("--bench") {
        let streams: usize = get_val("--streams")
            .map(|v| v.parse().expect("--streams N"))
            .unwrap_or(2);
        let fixture = get_val("--fixture").unwrap_or_else(|| "fixtures/bench-60s.wav".into());
        bench(streams, num_threads, get_flag("--separate"), &fixture);
    } else if get_flag("--live") {
        let path = get_val("--live").unwrap_or_else(|| format!("{MODEL_DIR}/test_wavs/0.wav"));
        live(&path, num_threads);
    } else if let Some(path) = get_val("--transcribe") {
        transcribe(&path, num_threads);
    } else {
        eprintln!("usage: stt-bench --make-fixture | --bench [--streams N] [--num-threads N] [--separate] [--fixture PATH] | --live [PATH] | --transcribe PATH");
        std::process::exit(2);
    }
}
