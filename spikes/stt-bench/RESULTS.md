# stt-bench — local streaming STT viability spike

**Date:** 2026-07-06
**Question:** Is local streaming STT v1-viable on this machine — official sherpa-onnx Rust
bindings + NVIDIA Nemotron Speech Streaming EN 0.6B (int8 ONNX), two concurrent streaming
recognizer streams, sustained ≥2× real-time combined?

## VERDICT: GO

Gate was ≥2× real-time combined for 2 concurrent streams. Measured **22.7× combined with
threads constrained to 2 per recognizer** (simulated meeting-loaded machine) and **25.3×
unconstrained**. Even one stream on a single ONNX intra-op thread runs at 9.05×. Peak process
RSS ~1.0 GiB with a shared recognizer. Transcript quality on the reference fixtures is
essentially perfect. Nothing here threatens v1.

---

## Environment

- Host: i7-13700HX (24 hw threads), 31 GiB RAM, Linux 6.18.7-76061807-generic (Pop!_OS/Ubuntu 24.04 userland)
- rustc/cargo 1.96.0
- Benchmarks run with the machine otherwise in normal desktop use (~13 GiB RAM already in use)

## Crates (pinned in Cargo.toml)

| crate | version | notes |
|---|---|---|
| `sherpa-onnx` | =1.13.3 (2026-06-16) | official first-party safe wrapper, repo k2-fsa/sherpa-onnx |
| `sherpa-onnx-sys` | =1.13.3 | raw FFI; `links = "sherpa-onnx"` |

The third-party `sherpa-rs` crate was archived June 2026 in favor of these first-party crates.

### Build gotchas (pleasant surprises, mostly)

- **No cmake needed.** `sherpa-onnx-sys`'s build.rs does **not** compile C++: with the default
  `static` feature it downloads a prebuilt archive
  `sherpa-onnx-v1.13.3-linux-x64-static-lib.tar.bz2` from the k2-fsa GitHub releases and links
  13 static libs (onnxruntime included). cmake 4.3.4 was installed user-locally via
  `uv tool install cmake` as a precaution but was never invoked.
- The prebuilt archive is cached under `target/sherpa-onnx-prebuilt/`; escape hatches exist:
  `SHERPA_ONNX_LIB_DIR` (own libs) and `SHERPA_ONNX_ARCHIVE_DIR` (pre-downloaded archive) —
  useful for offline/CI builds.
- Cold `cargo build --release` including the lib download: **31.5 s**. Binary: 34.7 MB,
  fully static (no runtime .so to ship). A `shared` feature exists if dynamic linking is preferred.
- crates.io API note (tooling only): requests without a User-Agent get empty responses.

## Model

| | |
|---|---|
| HF repo | `csukuangfj/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14` |
| Revision | `f13b0c6a48186fdd9fdd8d203b9527b0b709b09f` (2026-01-14) |
| Upstream | https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b |
| Location | `models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14/` |

Files (exact bytes):

| file | size |
|---|---|
| encoder.int8.onnx | 652,916,830 (623 MiB) |
| decoder.int8.onnx | 7,257,753 |
| joiner.int8.onnx | 1,735,862 |
| tokens.txt | 8,952 (1025 BPE tokens; incl. uppercase + `. , ? ! '`) |
| test_wavs/0.wav | 212,044 (6.62 s, 16 kHz mono) |
| test_wavs/1.wav | 534,924 (16.71 s, 16 kHz mono) |
| test_wavs/8k.wav | 77,244 (8 kHz — excluded from fixtures) |
| test_wavs/trans.txt | 449 (reference transcripts) |

Loaded via the plain **online transducer** config (encoder/decoder/joiner + tokens);
`model_type` left unset (auto-detected), default `feature_dim: 80` worked, greedy_search decoding.

## Fixtures

- `fixtures/bench-60s.wav` — **73.02 s**, built by `--make-fixture`: loop of
  `0.wav + 0.5s silence + 1.wav + 0.5s silence` repeated until ≥60 s (3 loops).
- `fixtures/endpoint-test.wav` — 25.34 s: `0.wav + 2.0 s silence + 1.wav`, used to verify
  endpoint-driven final segments.
- Short quality fixtures: the repo's `test_wavs/0.wav`, `1.wav` with known reference text.

## Measurements

Load time: **1.73–1.81 s** per recognizer create (consistent across all runs).

### RTF table (fixture = 73.02 s audio, fed full-speed in 100 ms chunks, decode-as-you-go)

"speed" = audio_sec / wall_sec (higher is faster; gate = 2.0 combined).

| config | per-stream speed | combined speed | peak RSS |
|---|---|---|---|
| 1 stream, num_threads=4 | 16.5× | 16.5× | 0.94 GiB |
| **2 streams, shared recognizer, num_threads=4** | 13.4× / 12.6× | **25.3×** | 0.96 GiB |
| **2 streams, shared recognizer, num_threads=2 (constrained)** | 11.4× / 11.4× | **22.7×** | 0.95 GiB |
| 2 streams, 2 separate recognizers, num_threads=2 each | 11.3× / 11.6× | 22.6× | 1.66 GiB |
| 4 streams, shared recognizer, num_threads=2 | ~8.6× each | 34.0× | 1.03 GiB |
| 1 stream, num_threads=1 (floor) | 9.05× | 9.05× | — |

RAM: ~0.9 GiB after model load; streams are cheap (~30–65 MB each). A **second recognizer
instance costs +0.72 GiB** (weights are not shared between recognizer instances), and buys
nothing: shared vs separate at nt=2 is 22.7× vs 22.6×. Use ONE recognizer.

### Concurrency findings

- One `OnlineRecognizer` handles many `OnlineStream`s. Both types are `Send + Sync` in the
  Rust wrapper.
- Calling `recognizer.decode(&stream)` **concurrently from two threads on distinct streams
  worked correctly** (identical, correct transcripts on both streams; no crashes across all
  runs). ONNX Runtime `Session::Run` is thread-safe; per-stream state lives in the stream.
- A batched alternative exists if we ever want a single decode thread:
  `decode_multiple_streams(&[&OnlineStream])`.

### Live/partial behavior (`--live`, real-time paced 100 ms chunks, nt=2)

- **Partial cadence ≈ 1.1 s**, bounded by the model's native chunk (cache-aware FastConformer,
  ~1.04 s per encoder step) — not by how often you feed audio. Feeding 100 ms chunks is fine;
  new text just arrives ~1×/second. If sub-second partial latency ever matters, this model
  won't give it; that's a model property, not a perf limit.
- Each encoder step cost **~78–92 ms at num_threads=2** (so live decode occupies ~8% of two
  cores per stream; plenty of headroom for 2 live meetings + the rest of the app).
- **Partials are append-only / stable** — across the whole 16.7 s utterance no previously
  emitted word was rewritten or retracted. Great for incremental UI.
- Endpointing works: with `enable_endpoint` + default rules (rule1=2.4 s trailing silence,
  rule2=1.2 s trailing silence after speech, rule3=20 s max utterance), a 2.0 s pause
  triggered a final segment mid-stream.

## Transcript quality (eyeball WER vs repo reference)

| fixture | ref words | errors | WER | diffs |
|---|---|---|---|---|
| 0.wav | 18 | 0 | 0.0% | — |
| 1.wav | 48 | 1 | 2.1% | `dishonoured` → `dishonored` (US spelling; not a real error) |

Sample output (1.wav):

>  God as a direct consequence of the sin which man thus punished had given her a lovely child
>  whose place was on that same dishonored bosom to connect her parent for ever with the race
>  and descent of mortals, and to be finally a blessed soul in heaven

- **Punctuation: partially.** Commas are emitted naturally; `. , ? !` are all in the vocab but
  no sentence-final period appeared on these (period-less LibriSpeech) clips. Don't count on
  full sentence punctuation without a punctuation post-model.
- **Capitalization: partially.** Proper nouns get capitalized ("God"); segment-initial words
  are lowercase. Plan for a cheap display-side capitalizer or the sherpa-onnx online
  punctuation model if polished text is needed.
- Minor run-to-run variance after endpoint resets (one run produced "bless" for "blessed"
  right at a segment tail) — normal for streaming greedy search.

## API notes for the real app

- Config shape (see `src/main.rs::make_config`): `OnlineRecognizerConfig { feat_config
  (16000/80), model_config.transducer{encoder,decoder,joiner}, model_config.tokens,
  num_threads, provider:"cpu", decoding_method:"greedy_search", enable_endpoint,
  rule1/2/3 }`. `num_threads: 2` is the sweet spot for multi-stream; 4 helps single-stream
  latency slightly.
- Loop: `create_stream()` → per 100 ms chunk `accept_waveform(16000, &chunk)` then
  `while is_ready(&s) { decode(&s) }` → `get_result(&s)`.
- **Segmenting: use `is_endpoint()` + `reset()`.** The `is_final` field in the result JSON is
  NOT set at endpoints (observed `is_final=false` on an endpoint-final segment); it only
  reflects end-of-input. Grab the result, treat it as the final segment text, then `reset()`.
- **Timestamps: yes.** Per-token timestamps in seconds (`timestamps` parallel to `tokens`),
  relative to the current segment; `start_time` and `segment` fields exist for offsetting.
- **Tail flush: required.** Append ~0.8 s of silence before `input_finished()` or the last
  1–2 words are dropped (streaming encoder right-context). For live mic use this only matters
  on shutdown; endpoint-driven finals during silence are unaffected.
- Feed rate is decoupled from decode: `accept_waveform` buffers, `is_ready` gates when a full
  model chunk (~1.04 s) is available.
- 8 kHz input: `accept_waveform` takes a sample-rate argument and sherpa resamples, but keep
  the app pipeline at 16 kHz mono.

## Runnable benchmark

```
cd spikes/stt-bench
cargo build --release
./target/release/stt-bench --make-fixture
./target/release/stt-bench --bench --streams 2 --num-threads 2     # the gate run
./target/release/stt-bench --bench --streams 2 --num-threads 4
./target/release/stt-bench --bench --streams 2 --num-threads 2 --separate
./target/release/stt-bench --live fixtures/endpoint-test.wav --num-threads 2
./target/release/stt-bench --transcribe models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14/test_wavs/1.wav
```

(Model download: `hf download csukuangfj/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14
--revision f13b0c6a48186fdd9fdd8d203b9527b0b709b09f --local-dir models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14`)

## Fallback

Not needed. The Nemotron int8 export loaded and ran first try in sherpa-onnx 1.13.3 via the
plain transducer config; the Kroko ASR fallback was never exercised.
