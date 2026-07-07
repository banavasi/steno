/// Streaming STT behind one trait so engines swap without touching the pipeline.
/// v1 backend: sherpa-onnx + Nemotron (M0-spike-verified); MockEngine for dev/tests.
pub enum SttEvent {
    /// In-flight hypothesis for the current utterance (replaces the previous partial).
    Partial(String),
    /// Utterance finalized. `t_start` = seconds into the stream where it began.
    Final { text: String, t_start: f64 },
}

pub trait SttEngine: Send {
    /// Feed 16 kHz mono f32 samples; returns any events produced.
    fn feed(&mut self, pcm: &[f32]) -> Vec<SttEvent>;
    /// Flush at end of stream.
    fn finish(&mut self) -> Vec<SttEvent>;
}

pub mod nemotron {
    //! NVIDIA Nemotron Speech Streaming EN 0.6B (int8) on sherpa-onnx.
    //! M0-spike facts baked in: ONE recognizer shared by both channels (a second
    //! instance costs +0.7 GiB for zero gain; concurrent decode on distinct
    //! streams is safe); endpoints come from `is_endpoint()`+`reset()` (the
    //! result's `is_final` never fires); tail flush needs ~0.8 s of silence;
    //! partials are append-only at ~1.1 s cadence (the model's native chunk).
    use super::{SttEngine, SttEvent};
    use anyhow::{Context, Result};
    use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    pub const MODEL_NAME: &str = "sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14";
    pub const MODEL_REPO: &str =
        "https://huggingface.co/csukuangfj/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14";
    const MODEL_REV: &str = "f13b0c6a48186fdd9fdd8d203b9527b0b709b09f"; // pinned
    const MODEL_FILES: [&str; 4] =
        ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"];
    const RATE: i32 = 16_000;

    pub fn model_dir() -> PathBuf {
        crate::session::data_dir().join("models").join(MODEL_NAME)
    }

    pub fn model_present(dir: &Path) -> bool {
        MODEL_FILES.iter().all(|f| dir.join(f).exists())
    }

    /// First-run bootstrap: if the model is missing, offer to download it
    /// (pinned revision, resumable `curl` — present on Linux/macOS/Win10+).
    /// Runs pre-TUI, so plain stdin/stderr prompting is fine.
    pub fn ensure_model() -> Result<()> {
        let dir = model_dir();
        if model_present(&dir) {
            return Ok(());
        }
        eprintln!(
            "Nemotron STT model missing at {} (~650 MB, one-time download).",
            dir.display()
        );
        eprint!("download it now from HuggingFace? [Y/n] ");
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans).ok();
        if ans.trim().eq_ignore_ascii_case("n") {
            anyhow::bail!(
                "model required. Fetch it manually with:\n  hf download csukuangfj/{MODEL_NAME} --local-dir {}",
                dir.display()
            );
        }
        std::fs::create_dir_all(&dir)?;
        for file in MODEL_FILES {
            let dest = dir.join(file);
            if dest.exists() {
                continue;
            }
            let url = format!("{MODEL_REPO}/resolve/{MODEL_REV}/{file}");
            eprintln!("↓ {file}");
            let part = dir.join(format!("{file}.part"));
            let status = std::process::Command::new("curl")
                .args(["-L", "--fail", "--retry", "3", "-C", "-", "--progress-bar", "-o"])
                .arg(&part)
                .arg(&url)
                .status()
                .context("run curl (is it installed?)")?;
            anyhow::ensure!(status.success(), "download failed for {file} — rerun to resume");
            std::fs::rename(&part, &dest)?;
        }
        anyhow::ensure!(model_present(&dir), "download finished but files are missing");
        eprintln!("model ready.");
        Ok(())
    }

    /// Load the shared recognizer (~1.8 s, ~0.9 GiB). Call once per app.
    pub fn load_recognizer() -> Result<Arc<OnlineRecognizer>> {
        let dir = model_dir();
        if !model_present(&dir) {
            anyhow::bail!(
                "Nemotron model missing at {}.\nFetch it with:\n  hf download csukuangfj/{MODEL_NAME} --local-dir {}\n(or see {MODEL_REPO})",
                dir.display(),
                dir.display()
            );
        }
        let mut config = OnlineRecognizerConfig::default();
        let p = |f: &str| Some(dir.join(f).to_string_lossy().into_owned());
        config.model_config.transducer.encoder = p("encoder.int8.onnx");
        config.model_config.transducer.decoder = p("decoder.int8.onnx");
        config.model_config.transducer.joiner = p("joiner.int8.onnx");
        config.model_config.tokens = p("tokens.txt");
        // 2 intra-op threads: the spike's constrained gate still ran 22.7x realtime
        // combined; leaves headroom for Zoom/Meet + browser on the same box.
        config.model_config.num_threads = 2;
        config.model_config.provider = Some("cpu".into());
        config.decoding_method = Some("greedy_search".into());
        config.enable_endpoint = true;
        config.rule1_min_trailing_silence = 2.4;
        config.rule2_min_trailing_silence = 1.2;
        config.rule3_min_utterance_length = 20.0;
        let rec = OnlineRecognizer::create(&config).context("load Nemotron recognizer")?;
        Ok(Arc::new(rec))
    }

    pub struct Engine {
        rec: Arc<OnlineRecognizer>,
        stream: OnlineStream,
        last_partial: String,
        samples_seen: u64,
        utt_start: Option<f64>,
    }

    impl Engine {
        /// Must be called on the thread that will feed it (streams stay put).
        pub fn new(rec: Arc<OnlineRecognizer>) -> Self {
            let stream = rec.create_stream();
            Self { rec, stream, last_partial: String::new(), samples_seen: 0, utt_start: None }
        }

        fn now(&self) -> f64 {
            self.samples_seen as f64 / RATE as f64
        }
    }

    impl SttEngine for Engine {
        fn feed(&mut self, pcm: &[f32]) -> Vec<SttEvent> {
            let mut out = Vec::new();
            self.stream.accept_waveform(RATE, pcm);
            self.samples_seen += pcm.len() as u64;
            while self.rec.is_ready(&self.stream) {
                self.rec.decode(&self.stream);
            }
            if let Some(r) = self.rec.get_result(&self.stream)
                && !r.text.is_empty() && r.text != self.last_partial {
                    if self.utt_start.is_none() {
                        // ponytail: first partial lags speech onset by the model's
                        // ~1.1s chunk; good enough for utterance-level interleave
                        self.utt_start = Some((self.now() - 1.2).max(0.0));
                    }
                    self.last_partial = r.text.clone();
                    out.push(SttEvent::Partial(r.text));
                }
            if self.rec.is_endpoint(&self.stream) {
                if let Some(r) = self.rec.get_result(&self.stream)
                    && !r.text.is_empty() {
                        out.push(SttEvent::Final {
                            text: r.text,
                            t_start: self.utt_start.take().unwrap_or_else(|| self.now()),
                        });
                    }
                self.rec.reset(&self.stream);
                self.last_partial.clear();
                self.utt_start = None;
            }
            out
        }

        fn finish(&mut self) -> Vec<SttEvent> {
            // flush the encoder's right-context with ~0.8s of silence
            let tail = vec![0.0f32; (RATE as usize) * 8 / 10];
            self.stream.accept_waveform(RATE, &tail);
            self.stream.input_finished();
            while self.rec.is_ready(&self.stream) {
                self.rec.decode(&self.stream);
            }
            match self.rec.get_result(&self.stream) {
                Some(r) if !r.text.is_empty() => vec![SttEvent::Final {
                    text: r.text,
                    t_start: self.utt_start.take().unwrap_or_else(|| self.now()),
                }],
                _ => Vec::new(),
            }
        }
    }
}

/// Dev engine: energy-gated "VAD" that reports speech bursts as utterances.
/// Proves the audio→engine→UI pipe end-to-end before the real model lands.
pub struct MockEngine {
    samples_seen: u64,
    in_speech: bool,
    speech_start: f64,
    silence_run: f64, // seconds of consecutive silence while in speech
    peak_db: f32,
}

const MOCK_GATE_DB: f32 = -45.0;
const MOCK_ENDPOINT_SECS: f64 = 0.8;

impl MockEngine {
    pub fn new() -> Self {
        Self {
            samples_seen: 0,
            in_speech: false,
            speech_start: 0.0,
            silence_run: 0.0,
            peak_db: f32::NEG_INFINITY,
        }
    }

    fn now(&self) -> f64 {
        self.samples_seen as f64 / 16000.0
    }
}

impl SttEngine for MockEngine {
    fn feed(&mut self, pcm: &[f32]) -> Vec<SttEvent> {
        let mut out = Vec::new();
        if pcm.is_empty() {
            return out;
        }
        let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt();
        let db = 20.0 * rms.max(1e-9).log10();
        let dur = pcm.len() as f64 / 16000.0;
        self.samples_seen += pcm.len() as u64;

        if db > MOCK_GATE_DB {
            if !self.in_speech {
                self.in_speech = true;
                self.speech_start = self.now() - dur;
                self.peak_db = db;
            }
            self.peak_db = self.peak_db.max(db);
            self.silence_run = 0.0;
            out.push(SttEvent::Partial(format!(
                "[speech… peak {:.0} dB]",
                self.peak_db
            )));
        } else if self.in_speech {
            self.silence_run += dur;
            if self.silence_run >= MOCK_ENDPOINT_SECS {
                self.in_speech = false;
                out.push(SttEvent::Final {
                    text: format!(
                        "[speech {:.1}s, peak {:.0} dB — mock engine]",
                        self.now() - self.speech_start - self.silence_run,
                        self.peak_db
                    ),
                    t_start: self.speech_start,
                });
                self.peak_db = f32::NEG_INFINITY;
            }
        }
        out
    }

    fn finish(&mut self) -> Vec<SttEvent> {
        if self.in_speech {
            self.in_speech = false;
            vec![SttEvent::Final {
                text: format!("[speech, peak {:.0} dB — mock engine]", self.peak_db),
                t_start: self.speech_start,
            }]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden-transcript check through the app's own Engine wrapper
    /// (partial/endpoint/finish logic), not just the raw bindings.
    #[test]
    #[ignore = "needs the 633MB Nemotron model in the data dir"]
    fn nemotron_transcribes_fixture() {
        let rec = nemotron::load_recognizer().expect("model present");
        let mut eng = nemotron::Engine::new(rec);
        let wav = nemotron::model_dir().join("test_wavs/0.wav");
        let wave = sherpa_onnx::Wave::read(wav.to_str().unwrap()).expect("fixture wav");
        assert_eq!(wave.sample_rate(), 16_000);

        let mut partials = 0;
        let mut finals: Vec<String> = Vec::new();
        for chunk in wave.samples().chunks(1600) {
            for ev in eng.feed(chunk) {
                match ev {
                    SttEvent::Partial(_) => partials += 1,
                    SttEvent::Final { text, .. } => finals.push(text),
                }
            }
        }
        for ev in eng.finish() {
            if let SttEvent::Final { text, .. } = ev {
                finals.push(text);
            }
        }
        let text = finals.join(" ").to_lowercase();
        assert!(partials > 0, "no streaming partials emitted");
        assert!(
            text.contains("after early nightfall") && text.contains("yellow lamps"),
            "unexpected transcript: {text:?}"
        );
    }
}
