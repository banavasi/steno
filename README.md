# voice-mentor (`mentor`)

Live meeting transcription in your terminal — bot-free, local-first. Captures your **mic**
("Me") and the **system audio** ("Them" — Google Meet in a tab, the Zoom app, anything) as two
separate channels, transcribes both on-device with NVIDIA Nemotron streaming STT, keeps a
rolling Haiku summary, and embeds a real Claude Code (haiku) pane that knows the transcript.
Notes file into the Obsidian brain when the meeting ends.

Design & architecture: [DESIGN.md](DESIGN.md). Linux is the v1 platform (PipeWire);
macOS/Windows are milestone M5.

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

```fish
cargo install --path .
mentor doctor          # checks mic, loopback, model, gcli, op, claude
```

First run needs the STT model (~650 MB, one-time):

```fish
hf download csukuangfj/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14 \
  --local-dir ~/.local/share/voice-mentor/models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-int8-2026-01-14
```

No API key needed: both the chat pane and the rolling summary run on your existing
Claude Code login (`claude` CLI, haiku model — the summary is `claude -p` under the hood).
The calendar picker uses the fleet's `gcli` (`gcli auth login -p <profile>` if expired).

## Use

```fish
mentor standup                 # or: mentor 1on1 · mentor meet · mentor
mentor --title "arch review"   # skip the calendar picker
mentor --project ~/workspaces/personal/agentic-os   # give the claude pane the project
mentor resume                  # reopen the last session after a crash
```

Launch flow: pick today's calendar event (or create/skip) → recording starts.

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
