# voice-mentor — Design Doc (v1)

**Status: SIGNED OFF 2026-07-06; M0–M4 built & verified same day** · profile: personal

> Implementation deltas from this doc (all ponytail-marked in code):
> - Linux loopback ships as `parec @DEFAULT_MONITOR@` subprocess (server-side 16k mono), not
>   pipewire-rs — the native API arrives with v2 per-app targeting.
> - The claude pane's cwd is a FIXED `~/.local/share/voice-mentor/chat` dir (one trust dialog
>   ever) with the per-meeting workspace granted via `--add-dir`.
> - M0 spike findings baked in: one shared recognizer (not per-channel), `is_endpoint()+reset()`
>   finalization, 0.8s tail flush, ~1.1s append-only partials; responder table = DA1/OSC10-12/
>   XTVERSION/DECRQM + CSI?2026 sync-frame buffering (see spikes/*/RESULTS.md).
> - gcli's cal_list JSON has no event id/description yet → no points-to-discuss seeding from
>   calendar and `event-id` stays empty in filed notes until gcli grows those fields.
> - No daily-note rollup: the vault contract has no fence for meetings; type: meeting notes
>   surface via Dataview instead.
> - The summarizer (§7) runs as `claude -p --model haiku` subprocesses on the user's Claude
>   Code subscription — the gap-check's alternative (a) — instead of the direct Messages API:
>   no API key, no marginal spend, so the dollar spend-cap machinery is void (cadence guard +
>   backoff remain).

A cross-platform terminal TUI (`mentor`) that live-transcribes your meetings, keeps a rolling
AI summary, and embeds a Claude Code pane with project context. Bot-free: it captures audio on
*your* machine, never joins the call.

Research basis: 8-agent workflow (2026-07-06) across cloud STT, local STT, per-OS audio capture,
ratatui/PTY embedding, prior art (Granola/Hyprnote/screenpipe/Meetily/Raven), calendar auth, and
Claude integration, plus an adversarial gap check. Key findings inlined below.

---

## 1. Decisions (locked with user, 2026-07-06)

| Decision | Choice |
|---|---|
| Stack | Rust + ratatui 0.30 + crossterm + tokio. Single binary per OS. |
| STT | **Local on-device**, streaming, behind one `SttEngine` trait. Cloud is a v2 opt-in, not the default. |
| Speaker labels | **Channel separation**: mic = "Me", system/loopback audio = "Them". No diarization model in v1. |
| Calendar | Reuse **`gcli`** (agentic-os `google` tool) via subprocess: `gcli cal list/create --json`. Zero new auth. |
| Notes home | **~/brain** (Obsidian vault), per its CLAUDE.md contract, + daily-note rollup. |
| Summary pane | claude-haiku-4-5 via Anthropic Messages API, incremental delta prompts. |
| Chat pane | Embedded `claude --model haiku` in a PTY pane (tui-term). |

Channel separation is the load-bearing simplification: every serious bot-free product (Granola,
Hyprnote, screenpipe, Project Raven) converged on two never-mixed streams instead of speaker-ID
models. "Me vs Them" is structural, not inferred.

## 2. Non-goals (v1)

- No full multi-speaker identification (splitting voices inside "Them") — documented v2 path is
  pyannote-style ONNX segmentation on the loopback channel.
- No calendar **auto**-start/stop — the user launches `mentor` deliberately. Kills the
  unattended-capture and runaway-spend class entirely.
- No echo cancellation (AEC) — v1 ships an honest headphones gate instead (§6.3).
- No per-app audio capture — whole-system loopback; during a meeting the meeting *is* the
  system audio. Per-app targeting (PipeWire node / macOS per-PID tap / Win11 process loopback) is v2.
- No raw-audio persistence (Granola posture). Audio is transcribed and dropped.
- No bot joining calls, ever.

## 3. UX

### Command

```
mentor standup            # meeting type: standup
mentor 1on1               # meeting type: one-on-one
mentor meet               # scheduled/general meeting
mentor                    # same as `mentor meet`
mentor resume             # reopen the last session after a crash/quit
mentor doctor [--json]    # mic, loopback, STT benchmark, gcli, op, ANTHROPIC key, brain path
```

### Launch flow

1. Meeting type known from the subcommand.
2. Calendar picker: `gcli cal list --profile <p> --days 1 --json` → today's events as a list →
   pick one, **c**reate one (`gcli cal create`), or **s**kip (manual title). Profile selectable
   (personal / asu / oneorigin); `gcli` unavailable → manual title, never block.
3. Project picker: choose the project this meeting is about (dirs from config +
   `~/workspaces/<profile>/*`) — feeds the Claude pane's `--add-dir` and the brain filing target.
4. Audio check: mic level meter + loopback level meter + headphones warning (§6.3). Confirm → recording.

### Layout

```
┌────────────────────────────┬──────────────────────────────┐
│  TRANSCRIPT (live)   50%   │  SUMMARY / POINTS      50%↕  │
│                            │  ▸ Summary                   │
│  Me    10:02 so the deploy │  ▸ Decisions                 │
│        gate needs...       │  ▸ Action items              │
│  Them  10:02 right, and    │  ▸ Points to discuss         │
│        the rollback...     ├──────────────────────────────┤
│  Them  10:03 ▌(partial…)   │  CLAUDE CODE (haiku)         │
│                            │  > what did we decide about  │
│ ● REC  [m]ute [p]ause      │    the rollback gate?        │
└────────────────────────────┴──────────────────────────────┘
```

- Transcript: per-speaker color, partials dimmed, sticky-tail autoscroll with manual scrollback.
- Keybinds: `Tab` cycle pane focus (all other keys forward to the Claude PTY when it's focused);
  `m` pause **mic** transcription (one keystroke — the "I'm muted in Zoom" privacy control);
  `p` pause everything; `Ctrl+Q` end meeting → save flow. A visible `● REC` indicator at all times.

### Meeting end

`Ctrl+Q` → summary finalizes → save prompt (§10) → note filed to brain → session dir kept for `resume`.

## 4. Architecture

```
mic ──cpal──► ring buf ─┐                          ┌─► transcript pane
                        ├─► resample 16k mono ─► VAD ─► SttEngine (×2) ─► merge ─► TranscriptStore ─► summary task ─► summary pane
sys ─loopback─► ring buf ┘        (per channel)   (sherpa-onnx/Nemotron)     │                      (Haiku, delta)
                                                                             └─► transcript.md tail ─► claude PTY pane
```

- **One event loop**: everything (crossterm `EventStream`, STT events, PTY output, summary results,
  tick) funnels into a single `mpsc<AppEvent>` consumed by one `select!` loop that owns `AppState`
  and redraws — coalesced behind a ~30 fps tick, because PTY repaint floods + 5–10 Hz partials
  will peg a redraw-per-event loop.
- **Audio callbacks never touch the runtime**: cpal/loopback callbacks push into lock-free ring
  buffers (`rtrb`); a dedicated thread drains, resamples (rubato, only where the OS can't
  server-side convert), VAD-gates, and feeds the recognizers.
- **Capture is trait-injected** (`AudioSource`): real devices in production, WAV fixtures in tests.

### Crates

ratatui 0.30 / crossterm / tokio / cpal 0.18 (mic) / pipewire-rs + libpulse-binding (Linux loopback)
/ cidre (macOS tap) / wasapi (Windows loopback) / sherpa-onnx + sherpa-onnx-sys (STT+VAD) / rubato
/ rtrb / portable-pty 0.9 + vt100 0.16 + tui-term 0.3.4 (Claude pane) / reqwest+serde (Anthropic API).
Pin ratatui-core-adjacent widget versions together (0.30 modular split strands mismatched widgets).

## 5. Audio capture (per OS)

Mic = cpal everywhere (Linux: pin the ALSA host explicitly if the new PipeWire host misbehaves —
its maturity is unverified). System audio = per-OS backend behind one `SpeakerStream` trait —
**cpal cannot do loopback** (cpal #876), don't plan a single-crate capture layer.

| OS | Backend | Notes |
|---|---|---|
| Linux (v1 primary) | pipewire-rs capture stream, `STREAM_CAPTURE_SINK=true` (default sink monitor) | Zero permission prompts. libpulse fallback for PulseAudio-only distros. Request 16 kHz mono server-side. |
| macOS (M5) | Core Audio process tap via cidre (`CATapDescription`, global mono tap), **14.4+ floor** | The field moved off ScreenCaptureKit (heavyweight Screen Recording permission, breaks after sleep). BlackHole = documented fallback for <14.4. Tap dictates device rate → resample client-side. |
| Windows (M5) | wasapi crate endpoint loopback, default render device, `AUTOCONVERTPCM` | Win11 baseline (Win10 EOL 2025-10; consumer ESU ends 2026-10). Per-process loopback = v2. Device-follow loop for default-device changes. |

Cross-cutting (from research + gap check):

- **Normalization boundary**: everything becomes 16 kHz mono at capture; every chunk tagged
  `{stream_id, monotonic capture timestamp}`.
- **Device churn**: default output changes mid-meeting (AirPods/HDMI) invalidate taps/loopback
  clients and change sample rates — detect and rebuild streams at runtime; also rebuild on
  suspend/resume wake.
- **macOS TCC is the hard part** (M5 scope, decided then, not now): the System Audio grant
  attributes to the *terminal emulator*, unsigned builds silently never prompt, and macOS 26.1 has
  a privacy-pane regression for plain executables. Plan: ad-hoc/Developer-ID codesign spike + a
  first-run permission wizard + `doctor` checks; a thin signed `.app` helper owning capture is the
  fallback architecture if attribution proves unacceptable.
- **Known CoreAudio tap bugs** (2026): level attenuation on multi-output devices, aggregate
  devices decaying to all-zero buffers over long uptimes — ship silence-detection + tap rebuild
  watchdog (screenpipe does).

## 6. STT

### 6.1 Engine

```rust
trait SttEngine {           // one instance per channel
    fn feed(&mut self, pcm16k: &[f32]);
    fn poll(&mut self) -> Vec<SttEvent>;  // Partial{text} | Final{text, words: [(w, t0, t1)]}
}
```

**v1 engine: NVIDIA Nemotron Speech Streaming EN 0.6B (int8) on sherpa-onnx**, official
first-party Rust bindings (`sherpa-onnx` crate — the third-party sherpa-rs was archived June 2026
in its favor). True cache-aware streaming (each frame processed once → **stable partials**, no
window re-decode flicker), ~6.9% avg WER (12.5% on Earnings22 — the honest proxy for compressed
Meet/Zoom far-end audio), punctuation + capitalization, ~0.7 GB int8, **560 ms chunk** setting
(explicit latency budget: ~0.5–1 s end-to-end feels live for captions; the 80/160 ms settings
trade accuracy and are config knobs, not defaults).

- Models download on first run (sidesteps NVIDIA Open Model License redistribution questions);
  pin exact HF revisions/SHAs — the int8 exports are community-maintained (csukuangfj).
- **VAD**: sherpa-onnx's built-in silero-vad gates both feeds — saves CPU and prevents
  silence-hallucination if any Whisper-family engine ever enters the pipeline.
- Whisper-family (whisper.cpp / faster-whisper) is explicitly **not** in the live path — chunked
  pseudo-streaming, rewrite flicker, silence hallucination. whisper.cpp large-v3-turbo is the v2
  *post-meeting* re-transcription pass for a polished canonical transcript.

### 6.2 Two streams on one laptop (the real risk)

All published RTF numbers are single-stream on idle machines; the real environment runs
Zoom/Meet + browser + thermal throttling. Mitigations, all v1:

1. `mentor doctor` benchmarks 2× concurrent Nemotron streams on this machine and reports headroom.
2. Runtime **backlog monitor**: if caption lag grows past a threshold, shed load — drop the mic
   channel to a smaller model, or pause a channel — and *say so* in the status bar.
   Degrade ladder: Nemotron×2 → Nemotron(them)+Kroko/small(me) → single channel.
3. Fallback engines behind the same trait: **Kroko ASR** (streaming Zipformer, same sherpa-onnx
   runtime, drop-in) first; **Moonshine v2** (better but no Rust bindings → sidecar; only if
   Nemotron disappoints in M0); Kyutai STT only ever as a GPU/Apple-Silicon backend (no CPU story).

### 6.3 Echo — v1 stance (gap-check: this is a correctness bug, not polish)

Without headphones, remote voices bleed into the mic → transcribed on **both** channels →
me/them labels and summary attributions silently corrupt. v1 ships the honest gate, not AEC:

- `doctor` + launch audio-check warn hard: **wired headphones = full quality; speakers = degraded
  mode** (banner shown, labels marked unreliable).
- Runtime echo detection: cheap energy-envelope cross-correlation between loopback and mic;
  on detection, show "echo detected — transcript labels degraded" instead of pretending.
- Bluetooth caveat surfaced in the warning: opening the mic on a BT headset collapses it to
  HFP telephone-band audio and can change the loopback rate mid-meeting — wired > BT > speakers.
- **AEC is v2**: webrtc-audio-processing crate / vendored hypr-aec, keyed on the loopback stream
  (Raven's pattern). Never fall back to mixing streams (Meetily's mistake) or a virtual device
  (Krisp's fragility). Note AEC needs ~10 ms mic/loopback alignment — an order tighter than
  transcript merging — so it gets its own alignment mechanism when it comes.

### 6.4 Merging two streams

Each recognizer's word timestamps are relative to its stream; anchor each stream's t=0 to the
shared monotonic clock at first frame, interleave **finalized utterances** by anchored start time.
Local STT keeps latency deterministic (no per-stream network variance), and clock drift
(~100–200 ms/hr) is below conversational-turn granularity; if hour-long sessions show turn-order
flips, re-anchor periodically (v1.x knob, `// ponytail:` marked in code).

## 7. Summary pane (top right)

Direct Anthropic Messages API, `claude-haiku-4-5` ($1/$5 per MTok), structured output.

- **Incremental state-update pattern** (this is also your idempotent-feeder rule): request =
  byte-frozen system prompt (cacheable; Haiku's minimum cacheable prefix is 4096 tokens) +
  previous summary JSON with **stable bullet IDs** + *only the transcript delta* past a
  high-water mark. Never re-feed the whole transcript per tick.
- Cadence: every 30–60 s *of new finalized text* (no new text → no call). Full re-summarize
  every ~10 min to correct drift. On-demand "catch me up" key.
- Sections: Summary · Decisions · Action items · Points to discuss · Open questions. Meeting
  type (standup/1on1/meet) selects the prompt template (standup: per-person yesterday/today/
  blockers; 1on1: topics/feedback/actions; meet: agenda/decisions/actions). "Points to discuss"
  seeds from the calendar event description + an optional pre-typed list.
- Cost ≈ **$0.25–0.50 per meeting-hour**. Hard caps anyway (your 2026-06 incident class):
  per-day spend ceiling in config, max meeting duration (default 3 h → prompt to continue),
  429/outage → stale-summary indicator + backoff, never a tight retry loop.
- Provider is a config knob (OpenAI-compatible transport) so it *can* route to DeepSeek per your
  bulk-LLM pref; default stays Haiku — this is interactive, low-volume, quality-visible.
- Fleet conventions: `ANTHROPIC_API_KEY` via `op read op://Mithra/...`; per-tool spend traced to
  a dedicated Phoenix project (`voice-mentor`) via Rust OTLP/OpenInference spans — v1.x, required
  by fleet convention, small.

## 8. Claude Code pane (bottom right)

**v1 = plan A: embed the real `claude` CLI** in a PTY pane (portable-pty + vt100 + tui-term),
because it delivers the full Claude Code UX (permissions, skills, CLAUDE.md, resume) with zero
protocol work:

```
cwd = <session dir>/workspace
claude --model haiku --session-id <meeting-uuid> \
       --append-system-prompt "Meeting in progress; read ./transcript.md before answering; summary in ./summary.md" \
       --add-dir <selected project dir>
```

- The app tails `transcript.md` / `summary.md` into the workspace (append-only); the workspace
  `CLAUDE.md` carries meeting metadata + the re-read-before-answering instruction. Pull-based
  context: no push protocol needed.
- **Known-fiddly, proven-solvable** (claude-p, eqms/claude-workbench are prior art): the host must
  answer Ink's startup terminal queries (DA1/DA2/DSR/XTVERSION) or Claude Code hangs; pane resize
  must forward TIOCSWINSZ + a repaint nudge (Ink doesn't reflow scrollback — accept ragged
  history); bracketed-paste sanitization; hand-rolled key→byte encoding.
- `--append-system-prompt` is **not preserved on resume** — re-pass all flags on every relaunch.
- Fallback ladder (decided now, so switching is mechanical): vt100 misrenders → swap emulation
  core to `alacritty_terminal` behind our own ~200-line widget (same PTY plumbing). Still janky
  (likely on Windows/ConPTY — least-tested combo) → **stream-json native pane**: drive
  `claude --input-format stream-json --output-format stream-json` and render our own chat widget.
  The gap-check rates this the likely eventual landing spot because Claude Code's renderer is an
  auto-updating moving target; we still start with the PTY embed because it's days-not-weeks and
  M0 tells us fast.
- Auth rides your existing Claude subscription (fine for a personal tool; not shippable to
  third parties per ToS — noted).

## 9. Calendar — reuse `gcli`

Subprocess, matching the tool's own `subprocess-json` transport:

```
gcli cal list   --profile personal --days 1 --json
gcli cal create --profile personal --title ... --start ... --json
```

No OAuth work, no new scopes, all three Google profiles for free, secrets already in Mithra.
`doctor` checks `gcli doctor --json`. The embedded Claude pane can independently reach the same
capability as the gateway MCP tools (`google__cal_list`) if connected — orthogonal to the TUI's
own picker. Not built: direct Calendar API client, ICS fallback, CalDAV — `gcli` already solved this.

## 10. Sessions, persistence, filing to brain

### Session dir (crash-safe by construction)

```
~/.local/share/voice-mentor/sessions/<ISO-ts>--<slug>/
  meeting.json        # type, title, calendar event id, profile, project, attendees
  transcript.jsonl    # append-only finalized utterances {t, speaker, text}; periodic fsync
  summary.json        # latest summary state (stable bullet IDs)
  workspace/          # claude cwd: CLAUDE.md, transcript.md, summary.md
```

Plain files, no SQLite. Crash/power-loss → `mentor resume` replays `transcript.jsonl`, resumes the
Claude session via `--session-id`, rebuilds audio streams. Suspend/resume mid-meeting → streams
rebuilt on wake (same code path as device churn).

### Filing to brain (at meeting end)

Per `~/brain/CLAUDE.md` contract (read at implementation time — its rules win over this sketch):

- Meeting note → the meeting's project under `02-projects/<project>/` with frontmatter
  (`profile`, date, type, attendees, event id): Summary · Decisions · **Action items as
  `- [ ]` + `#next`/`#fu`** (your TickTick-sync convention) · Points discussed.
- One-line rollup into today's daily note (same pattern as `journal`).
- **Transcript retention is a choice, not a default** (gap-check: third-party speech entering
  git-versioned permanent history): save prompt offers *summary-only to brain* (default) /
  *summary + full transcript* / *discard*. Full transcript always stays in the local session dir
  regardless; a `mentor sessions prune` command owns local retention.
- Atomic writes (temp + rename) — the vault is synced; partial reads corrupt.

## 11. Privacy & consent stance (explicit, per gap-check)

- **Local-only audio by default**: with local STT, raw audio never leaves the machine. The only
  egress is transcript *text* to Anthropic for the summary/chat panes — and `--offline` disables
  both panes for meetings where employer policy or jurisdiction demands it.
- **You are the consent owner**: bot-free capture is invisible to participants; two-party-consent
  jurisdictions may treat live transcription as recording. README states this plainly; the tool
  won't pretend otherwise.
- **Muted-mic capture**: the tap hears you while you're muted in Zoom/Meet. v1: `m` pauses mic
  transcription in one keystroke + persistent `● REC`/`◌ MIC PAUSED` indicator. Auto-following
  the meeting app's mute state is v2 (no reliable cross-OS API).
- Kill switch: `p` halts both channels instantly.

## 12. Config & secrets

`~/.config/voice-mentor/config.toml`: default profile, project roots, STT model + chunk-size
preset + degrade ladder, summarizer provider/model/cadence/daily-cap, brain paths, keybinds.
Secrets never in config: `ANTHROPIC_API_KEY` resolved at launch via `op read` (Mithra), env-var
override for machines without `op`.

## 13. Testing

- **Audio path**: `AudioSource` trait-injection → WAV-fixture pairs (mic.wav + loopback.wav from a
  real recorded meeting) drive capture→VAD→STT→merge→summary deterministically; golden-transcript
  assertions are keyword/similarity-based (STT isn't bit-stable). This is the CI story — runners
  have no audio devices; real-device verification is `mentor doctor`'s job on each user machine.
- **PTY pane**: expect-style smoke test — spawn `claude`, send a prompt, resize, assert repaint —
  on Linux + macOS CI; Windows/ConPTY gets a manual checklist until M5.
- One integration test pinning the `claude` CLI version + stream-json protocol shape, so CLI
  upgrades fail loudly, not weirdly.

## 14. Milestones (each independently useful)

| # | Deliverable | Proves |
|---|---|---|
| **M0** | Two spike binaries, ~2–4 days total: (a) `claude` inside tui-term — spawn, prompt, resize, query-responder; (b) sherpa-onnx + Nemotron int8 ×2 concurrent streams from WAV fixtures — RTF on your actual laptop | The two biggest unknowns. Go/no-go → fallbacks (alacritty_terminal or stream-json; Kroko/Moonshine) |
| **M1** | Linux, mic-only: `mentor` → live transcript pane (cpal → VAD → Nemotron), manual title, session dir + `resume` | End-to-end pipeline + TUI skeleton |
| **M2** | Second channel: PipeWire loopback → merged Me/Them transcript; headphones gate + echo detection; `m`/`p`/`● REC` | The core product claim |
| **M3** | Summary pane (Haiku incremental, caps) + Claude pane (PTY embed) + workspace tailing + brain filing with save prompt | The full 3-pane experience |
| **M4** | Calendar picker via `gcli`, meeting types + per-type summary templates, points-to-discuss seeding, `doctor` complete | Your described launch flow |
| **M5** | macOS (cidre tap, codesign/TCC wizard) then Windows (WASAPI loopback, ConPTY validation) | Cross-platform |

**v2 backlog**: AEC (webrtc-audio-processing, loopback-keyed) · per-app capture · cloud STT
opt-in behind the trait (AssemblyAI first — note: gap-check corrected Deepgram streaming to
~$0.46/hr/stream, AssemblyAI $0.15 session-hr stands) · post-meeting whisper.cpp re-transcription ·
within-"Them" diarization (pyannote ONNX) · stream-json native chat pane · mute-state following ·
meeting auto-detection · Phoenix OTLP spend spans (v1.x) · TickTick action-item sync ·
1on1 points-to-discuss seeded from open `- [ ]` items in the brain project note.

## 15. Top risks

| Risk | Mitigation |
|---|---|
| Nested Claude Code pane is a permanent maintenance treadmill (auto-updating Ink renderer, escape-traffic churn, ConPTY unknowns) | M0 spike first; decided fallback ladder ending in stream-json native pane; version-pinned integration test |
| 2× local STT can't hold real-time at minute 45 on a meeting-loaded, thermally-throttled laptop | doctor benchmark gate + runtime backlog monitor + explicit degrade ladder + status-bar honesty |
| Echo without headphones silently corrupts speaker labels | v1 honest gate: warning + runtime detection + "degraded" banner; AEC v2 |
| macOS TCC (terminal-emulator attribution, unsigned-build silence, 26.1 pane regression) | Scoped to M5: codesign spike, permission wizard, doctor checks; signed helper-.app as fallback architecture |
| sherpa-onnx Rust bindings are young; Nemotron int8 exports community-maintained | Own trait wrapper from day one; pinned SHAs + mirrored artifacts; Kroko drop-in fallback |
| "Them"-channel WER on compressed Meet/Zoom audio worse than benchmarks suggest | M0 fixture = real recorded meeting loopback, not LibriSpeech; expectations set by Earnings22 numbers |
| Third-party speech persisted forever in a git-versioned vault | Save prompt with summary-only default; local-only transcript retention + prune command |
| Summarizer spend drift | Delta-only high-water-mark feeding, frozen cacheable prompt, daily cap, backoff — per your bg-llm-idempotent pref |

## 16. Open questions for sign-off

1. **Binary/command name**: `mentor` assumed here (repo stays voice-mentor). Good, or something else?
2. **M0 order**: both spikes in parallel, or Claude-pane spike first?
3. **Consent posture**: is the README-documented "you own consent" stance sufficient, or do you
   want an audible/visible start chime option in v1?
4. macOS codesigning: OK deferring the Developer-ID-vs-ad-hoc decision to M5?
