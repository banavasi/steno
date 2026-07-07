# mentor

**Live meeting transcription in your terminal — bot-free, local-first.**

[![ci](https://github.com/banavasi/voice-mentor/actions/workflows/ci.yml/badge.svg)](https://github.com/banavasi/voice-mentor/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/banavasi/voice-mentor)](https://github.com/banavasi/voice-mentor/releases/latest)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

`mentor` sits in a terminal next to your Google Meet or Zoom window and transcribes the
meeting **on your machine** — no bot joins the call, no audio leaves your laptop. It captures
your **microphone** ("Me") and the **system audio** ("Them") as two separate channels, so
speaker labels come from physics instead of a diarization model. A rolling AI summary and an
embedded [Claude Code](https://claude.com/claude-code) pane — with live access to the
transcript — round out the three-pane TUI.

```
┌────────────────────────────┬──────────────────────────────┐
│  TRANSCRIPT (live)         │  SUMMARY / POINTS            │
│                            │  ▸ Summary                   │
│  Me    10:02 so the deploy │  ▸ Decisions                 │
│        gate needs the      │  ▸ Action items              │
│        health check first  │  ▸ Open questions            │
│  Them  10:02 right, and    ├──────────────────────────────┤
│        rollback should be  │  CLAUDE CODE (haiku)         │
│        automatic           │  > what did we decide about  │
│  Them  10:03 ▌(partial…)   │    the rollback gate?        │
│ ● REC  mic ▮▮▮▮▯ sys ▮▮▯   │                              │
└────────────────────────────┴──────────────────────────────┘
```

## Highlights

- **Local speech-to-text** — NVIDIA's Nemotron Speech Streaming 0.6B (int8) on
  [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx): true streaming with stable partials,
  ~1 s latency, runs 20×+ real-time on a modern laptop CPU. Raw audio never leaves your machine.
- **"Me vs Them" without ML** — mic and system-loopback audio are captured as two never-mixed
  streams; channel = speaker. The trick every serious bot-free notetaker converged on.
- **Rolling summary** — incremental, delta-fed updates (Summary / Decisions / Action items /
  Open questions) via `claude -p` on your existing Claude Code subscription. No API key.
- **A real Claude Code pane** — not a chat lookalike: the actual `claude` CLI in an embedded
  PTY, told where the live transcript file is. Ask "what did I promise in the last
  10 minutes?" mid-meeting.
- **Crash-safe by construction** — every finalized utterance is fsynced to an append-only
  `transcript.jsonl`; `mentor resume` picks up after a crash or a closed terminal.
- **Meeting notes that live in plain markdown** — on quit, file summary + action items (and
  optionally the transcript) into an Obsidian-style vault (`~/brain/07-meetings/`), or keep
  everything local. `mentor notes` browses it all afterwards.

## Install

Linux / macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/banavasi/voice-mentor/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/banavasi/voice-mentor/main/install.ps1 | iex
```

Or build from source with Rust: `cargo install --git https://github.com/banavasi/voice-mentor`

**Updating** = rerunning the same line — it always fetches the latest release and overwrites in
place (`mentor --version` to check what you have). Source installs update with
`cargo install --git … --force`.

Then:

```sh
mentor            # first run offers the STT model download (~650 MB, one-time, resumable)
mentor doctor     # checks mic, loopback, model, claude — with a fix for anything red
```

The chat + summary panes need the [Claude Code CLI](https://claude.com/claude-code) installed
and logged in (they ride your existing subscription — `claude` on PATH is all mentor asks).
Without it, transcription still works fully.

## Hearing the other side ("Them")

| OS | Setup |
|---|---|
| **Linux** | none — the PipeWire/PulseAudio monitor is captured automatically |
| **macOS** | install [BlackHole 2ch](https://existential.audio/blackhole), create a Multi-Output Device (Audio MIDI Setup → speakers + BlackHole), select it as output, run `mentor --loopback-device 'BlackHole 2ch'` |
| **Windows** | install [VB-Cable](https://vb-audio.com/Cable), route output through it, run `mentor --loopback-device 'CABLE Output'` |

Set `MENTOR_LOOPBACK_DEVICE` once instead of passing the flag. Without loopback, mentor runs
mic-only. Native no-virtual-device capture (Core Audio process taps, WASAPI loopback) is on
the [roadmap](#roadmap).

**Wear wired headphones.** On speakers, remote voices bleed into your mic and the Me/Them
labels degrade — mentor detects this and says so (`⚠ echo detected`) rather than pretending.

## Use

```sh
mentor standup                # or: mentor 1on1 · mentor meet · mentor
mentor --title "arch review"  # skip the calendar picker
mentor --project ~/code/app   # give the Claude pane your project context
mentor resume                 # reopen the last session after a crash
mentor notes                  # list past meetings; `mentor notes 2 --transcript` views one
```

| Key | Action |
|---|---|
| `Ctrl+Q` | end meeting → save prompt (works from any pane) |
| `m` | pause **mic** transcription (the "I'm muted" key) |
| `p` | pause everything |
| `Ctrl+T` | focus/unfocus the Claude pane (other keys pass through to it) |
| `↑↓ PgUp PgDn End` | transcript scrollback / stick to tail |

At meeting end: `Enter` files summary + action items to your vault, `t` includes the full
transcript, `k` keeps everything local. The raw session always stays under
`~/.local/share/voice-mentor/sessions/`.

Launching without `--title` opens a calendar picker when the author's `gcli` calendar CLI is
on PATH (today's events across all Google accounts); everyone else just types a title —
calendar integration is deliberately a thin, optional seam.

## How it works

```
mic ──cpal──► ring buffer ─┐
                           ├─► 16 kHz mono ─► VAD ─► Nemotron (×2 streams,
sys audio ──loopback──► ───┘                          one shared recognizer)
                                                          │
        chronological Me/Them merge ◄── word timestamps ──┘
                    │
                    ├─► transcript pane + append-only transcript.jsonl (fsync)
                    ├─► delta-fed summarizer (claude -p, 45 s cadence)
                    └─► transcript.md tailed for the embedded Claude pane
```

- One shared Nemotron recognizer serves both channels (a second instance costs +0.7 GB for
  zero gain); silero-VAD gates the feeds.
- The Claude pane is `claude --model haiku` inside a PTY (portable-pty + vt100 + tui-term),
  with the terminal-query responder and synchronized-output buffering Claude Code's renderer
  needs.
- The summarizer never re-feeds the whole transcript: previous summary state + only the new
  lines, with a final flush awaited at quit so the filed note includes the last minute.

More in [DESIGN.md](DESIGN.md) — including the research that picked this architecture — and
the M0 risk-spike reports under [`spikes/`](spikes/).

## Privacy & consent

- Transcription is **fully local**. The only network egress is transcript *text* to the
  summary/chat panes via your own Claude login — skip the `claude` CLI (or press `k` at save
  time) and nothing leaves the machine.
- Bot-free capture is **invisible to other participants**, and some jurisdictions treat live
  transcription as recording. Telling people is on you; mentor won't pretend otherwise.
- The mic tap keeps hearing you **while you're muted in the meeting app** — that's what the
  `m` key and the always-visible `● REC` / `◌ MIC PAUSED` indicator are for.
- Raw audio is never persisted; the transcript is yours, in plain files.

## Platform support

| | transcription | loopback ("Them") | Claude panes | notes |
|---|---|---|---|---|
| Linux (x86_64) | ✅ | ✅ automatic | ✅ | primary platform |
| macOS (arm64/x86_64) | ✅ | ✅ via BlackHole | ✅ | native tap on the roadmap |
| Windows (x86_64) | ✅ | ✅ via VB-Cable | ✅ (ConPTY) | least battle-tested |

CI builds and tests all three on every push.

## Roadmap

- Native loopback: Core Audio process taps (macOS 14.4+), WASAPI loopback (Windows)
- Echo cancellation (loopback-referenced AEC) so speakers work as well as headphones
- Per-app capture (only the meeting app, not every notification ding)
- Config file, `--offline` mode, post-meeting re-transcription polish pass
- Splitting speakers *within* "Them" (local diarization on the loopback channel)

## Development

```sh
cargo test                 # unit tests
cargo test -- --ignored    # golden-transcript test (needs the model downloaded)
cargo clippy --release
```

`spikes/` holds the two pre-build risk spikes — embedding Claude Code in a PTY, and the
2-concurrent-streams STT benchmark — kept as reference, each with its RESULTS.md.

## Credits & license

Built on [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) (Apache-2.0),
[NVIDIA Nemotron Speech Streaming](https://huggingface.co/nvidia) (NVIDIA Open Model License,
downloaded at first run), [ratatui](https://ratatui.rs),
[tui-term](https://github.com/a-kenji/tui-term), and [Claude Code](https://claude.com/claude-code).

MIT — see [LICENSE](LICENSE).
