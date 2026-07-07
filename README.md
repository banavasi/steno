# voice-mentor (`mentor`)

Live meeting transcription in your terminal — bot-free, local-first. Captures your **mic**
("Me") and the **system audio** ("Them" — Google Meet in a tab, the Zoom app, anything) as two
separate channels, transcribes both on-device with NVIDIA Nemotron streaming STT, keeps a
rolling Haiku summary, and embeds a real Claude Code (haiku) pane that knows the transcript.
Notes file into the Obsidian brain when the meeting ends.

Design & architecture: [DESIGN.md](DESIGN.md). Linux is the primary platform (PipeWire);
macOS/Windows work today via a virtual loopback device (below); native capture is milestone M5.

```
┌────────────────────────────┬──────────────────────────────┐
│  TRANSCRIPT (live)         │  SUMMARY / POINTS            │
│  Me    10:02 so the deploy │  ▸ Decisions ▸ Action items  │
│  Them  10:02 right, and…   ├──────────────────────────────┤
│  Them  10:03 ▌(partial…)   │  CLAUDE CODE (haiku)         │
│ ● REC  [m] [p] [Ctrl+T]    │  > what did we decide?       │
└────────────────────────────┴──────────────────────────────┘
```

## Install

One command from any machine with Rust (`rustup.rs`) and access to this repo:

```fish
cargo install --git https://github.com/banavasi/voice-mentor   # or: cargo install --path .
mentor                 # first run offers the STT model download (~650 MB, one-time, resumable)
mentor doctor          # checks mic, loopback, model, gcli, claude — with fixes for anything red
```

No API key needed: both the chat pane and the rolling summary run on your existing
Claude Code login (`claude` CLI, haiku model — the summary is `claude -p` under the hood).

**Calendar (optional):** the picker uses the fleet's `gcli`; without it you just type meeting
titles. On a new machine:

```fish
cargo install --git ssh://git@github.com/banavasi/agentic-os gcli
gcli init            # config points at the 1Password items — tokens are already in Mithra,
                     # so no re-auth needed on machines with `op` signed in
```

### System-audio ("Them") capture per OS

| OS | Setup |
|---|---|
| **Linux** | Nothing — PipeWire/PulseAudio monitor is captured automatically (`parec`). |
| **macOS** | Install [BlackHole 2ch](https://existential.audio/blackhole), create a **Multi-Output Device** (Audio MIDI Setup → your speakers + BlackHole) and select it as output, then run `mentor --loopback-device 'BlackHole 2ch'`. |
| **Windows** | Install [VB-Cable](https://vb-audio.com/Cable), set *CABLE Input* as default output (listen through your real device via its monitoring tab), then run `mentor --loopback-device 'CABLE Output'`. |

Set it once via the env var `MENTOR_LOOPBACK_DEVICE` instead of the flag. Without any of
this, mentor still runs mic-only ("Me" channel only). Native capture without a virtual
device (Core Audio process taps / WASAPI loopback) is the M5 milestone in DESIGN.md.

## Use

```fish
mentor standup                 # or: mentor 1on1 · mentor meet · mentor
mentor --title "arch review"   # skip the calendar picker
mentor --project ~/workspaces/personal/agentic-os   # give the claude pane the project
mentor resume                  # reopen the last session after a crash
mentor notes                   # list past meetings
mentor notes 2 --transcript    # view one (brain note if filed, else local summary)
```

Launch flow: pick from **today's events across all your gcli profiles** (fetched
concurrently, merged by time, expired profiles warned inline) — or create one, or just type a
title — then recording starts. `--profile <name>` restricts the picker to one account.

| Key | Action |
|---|---|
| `Ctrl+Q` | end meeting → save prompt (**works from any pane**) |
| `m` | pause **mic** transcription (the "I'm muted in Zoom" key) |
| `p` | pause everything |
| `Ctrl+T` | focus/unfocus the Claude pane (all other keys pass through to it) |
| `↑↓ PgUp PgDn End` | transcript scrollback / stick to tail |
| `q` | also quits, but only while the transcript pane has focus |

On quit: **[Enter]** files summary+actions to `~/brain/07-meetings/` (status: held, action
items as `- [ ] … #fu`), **[t]** includes the full transcript, **[k]** keeps it local. The
raw session (transcript.jsonl, summary, claude workspace) always stays under
`~/.local/share/voice-mentor/sessions/`.

## Honesty & consent

- **You own consent.** This records and transcribes people who cannot see any indicator.
  Two-party-consent jurisdictions may treat live transcription as recording. Tell people.
- **Audio never leaves the machine** — STT is fully local. Transcript *text* goes to the
  Anthropic API for the summary/chat panes; kill both by unsetting the key or using `k` at
  save time. (A `--offline` flag is on the backlog.)
- **Wear wired headphones.** On speakers, remote voices bleed into your mic and Me/Them
  labels degrade — the status bar warns (`⚠ speakers`, `⚠ echo detected`) instead of
  pretending. Bluetooth headsets drop the mic to telephone quality; wired beats BT beats
  speakers. Echo cancellation is v2.
- The mic tap keeps hearing you **while muted in the meeting app** — that's what `m` is for;
  the `◌ MIC PAUSED` / `● REC` indicator is always visible.
- The summarizer is delta-fed (~1 Haiku call per 45 s of new speech) on your subscription —
  no marginal dollar spend; it just counts against your plan's usage.

## Notes

- Whole-system loopback means notification dings and background audio transcribe as "Them" —
  per-app capture is v2.
- The Claude pane runs your normal `claude` login (Haiku model), cwd
  `~/.local/share/voice-mentor/chat` — accept the folder-trust dialog once, ever.
- Spikes under `spikes/` are the M0 risk burn-downs (PTY embedding, STT benchmark) — kept for
  reference, not part of the build.
