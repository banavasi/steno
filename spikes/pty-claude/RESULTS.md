# pty-claude spike — RESULTS

**Question:** Can the interactive `claude` (Claude Code) CLI run embedded inside a PTY we
control, rendered through the `vt100` crate (the emulation core `tui-term` uses)?

**Verdict: GO** (one small caveat about synchronized-output, see below).

- Host: Pop!_OS Linux, rustc 1.96.0
- Child: `claude --model haiku`, Claude Code **v2.1.202**, cwd = trusted project dir
- Stack: `portable-pty` 0.9.0 + `vt100` 0.16.2 (vte 0.15), PTY 120x35 → resized to 90x25
- Runs: `runs/dry` (marker-calibration run), `runs/full` (responder + round-trip + resize),
  `runs/noresp` (`--no-responder`, no round-trip)

## Headline numbers

| Metric | Value |
|---|---|
| UI ready (responder on) | **+1.80 s** after spawn |
| UI ready (responder OFF) | +1.40 s — does **not** hang |
| Round-trip (`PTY_SPIKE_OK` rendered) | **+4.42 s** (~2.6 s after submit, Haiku) |
| Resize 120x35 → 90x25 | repainted in < 3 s, **no nudge needed** |
| Child RSS at ready (process tree) | ~345 MB (node) |
| `/exit` → child exit code 0 | ~2.5 s, clean on every run |
| vt100 panics / garbage rendering | none |

## Terminal queries observed (and replies)

Claude Code v2.1.202 sends exactly this probe set, all within the first ~0.7 s
(hex is the full raw sequence; see `runs/*/queries.log`):

| Query | Raw (hex) | Count | Reply we sent | Required? |
|---|---|---|---|---|
| XTVERSION `ESC[>0q` | `1b5b3e3071` | 1 | `ESC P >\|mentor-spike 0.1 ESC \` | optional |
| DA1 `ESC[c` | `1b5b63` | 4 | `ESC[?1;2c` | optional but **recommended** (see timing) |
| OSC 11 bg query `ESC]11;?BEL` | `1b5d31313b3f07` | 1 direct (+1 tmux-wrapped) | `ESC]11;rgb:1e1e/1e1e/1e1e BEL` | optional — drives light/dark theme detection |
| DECRQM sync-output `ESC[?2026$p` | `1b5b3f323032362470` | 1 | `ESC[?2026;0$y` (not recognized) | optional |

**Not observed** from v2.1.202: DA2 (`ESC[>c`), DSR 5/6 (`ESC[5n`/`ESC[6n`), kitty query
`ESC[?u` (it *pushes* `ESC[>1u` / pops `ESC[<u` without querying), OSC 10 (fg), OSC 4,
XTWINOPS 14/16/18. The harness implements responders for all of these anyway — keep them in
the real app for version-proofing.

### Which replies are load-bearing?

**None are hard-blocking.** With `--no-responder` (all replies suppressed) claude still
reached a fully painted, working UI at +1.40 s, resized correctly, and exited cleanly.

Timing difference worth knowing: with replies suppressed, the *second* probe batch
(DA1/OSC 11/DECRQM) arrives at **t=2.48 s** instead of t=0.72 s — claude waits ~2 s for the
first DA1 answer before giving up and re-probing/proceeding. So answering DA1 removes a ~2 s
internal capability-wait. Answering OSC 11 is what lets claude match its theme to the host
background; without it, it falls back to the configured theme.

### tmux-passthrough nuance

Byte offset ~61 of raw.log: `ESC P tmux; ESC ESC ]11;? BEL ESC \` — claude wraps a second
OSC 11 query in a tmux DCS passthrough (probing the outer terminal in case it's inside
tmux). We are not tmux, so it's safe to ignore the wrapped copy and answer only the direct
one. vt100/vte swallows the DCS harmlessly (no rendering effect).

## Startup escape traffic (decoded from raw.log)

```
ESC 7  ESC[r  ESC 8            save cursor / reset scroll region / restore
ESC[?25h ESC[?25l              cursor visibility
ESC[?2004h                     bracketed paste ON
ESC[?1004h                     focus reporting ON
ESC[?2031h                     color-scheme-change reporting ON
ESC[<u  ESC[>1u                kitty keyboard pop/push (flags=1)
ESC[>4;2m                      modifyOtherKeys=2
ESC P tmux; ESC ESC ]11;? BEL ESC \    tmux-wrapped bg-color probe
ESC[>0q  ESC[c ESC[c  ESC]11;? ESC[c  ESC[?2026$p  ESC[c    the query batch
ESC[?1049h  ESC[2J  ESC[H      alt screen, clear, home
ESC[?1000h ?1002h ?1003h ?1006h    mouse reporting (SGR)
OSC 0 "✳ Claude Code"          window title
```

Steady-state painting is plain full/partial repaints: `CSI G/H/B/C` cursor moves, `CSI J/K`
clears, `CSI m` SGR (24-bit color), `ESC(B` charset, OSC 8 hyperlinks. Wrapped in
`ESC[?2026h … ESC[?2026l` synchronized-output brackets (used blindly even though we reported
mode 2026 unsupported).

## vt100 0.16.2 coverage

Everything vt100 flagged "unhandled" (via its `Callbacks` hooks) is a **non-rendering**
sequence — full list in `runs/full/sequences.log`:

- mode toggles it doesn't model: `?1004` (focus), `?2026` (sync output), `?2031` (color-scheme report)
- kitty keyboard push/pop (`CSI >1u` / `CSI <u`), `CSI >4;2m` modifyOtherKeys
- the queries themselves (`CSI c`, `CSI >q`, `CSI ?2026$p`) — answered by our scanner instead
- `ESC(B` charset select, OSC 8 hyperlinks, OSC 0 title (title IS exposed via callback), tmux DCS

**Zero rendering-relevant misses. No panics. No garbage.** The alacritty_terminal fallback is
not needed.

## Render fidelity — representative screen (120x35, after round-trip)

```
╭─── Claude Code v2.1.202 ─────────────────────────────────────────────────────────────────────────────────────────────╮
│                                                    │ Tips for getting started                                        │
│                 Welcome back Hank!                 │ Run /init to create a CLAUDE.md file with instructions for Cla… │
│                                                    │ ─────────────────────────────────────────────────────────────── │
│                       ▐▛███▜▌                      │ What's new                                                      │
│                      ▝▜█████▛▘                     │ Added a "Dynamic workflow size" setting in `/config` for contr… │
│                        ▘▘ ▝▝                       │ Added `workflow.run_id` and `workflow.name` OpenTelemetry attr… │
│    Haiku 4.5 · Claude Max ·                        │ Fixed a crash in the inline Ctrl+R history search when accepti… │
│    <redacted>@example.com's Organization   │ /release-notes for more                                         │
│         ~/workspaces/personal/voice-mentor         │                                                                 │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯

 ⚠ 2 MCP servers need authentication · run /mcp

❯ Reply with exactly PTY_SPIKE_OK and nothing else.

● PTY_SPIKE_OK

✻ Cogitated for 2s

────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
    Haiku 4.5   Think   18% █▊░░░░░░░░ 36.1k/200k   6.6k/m   +0/-0   ?     0.04  󰔟 40% 2h54m  󰨳 36% 4d…
```

Box-drawing, block-element logo art, wide-glyph icons, the two-column welcome layout, the
spinner, and the status row all render correctly. Idle traffic is tiny (~5.5 KB over 90 s).

## Round-trip

Typed `Reply with exactly PTY_SPIKE_OK and nothing else.` + CR (plain bytes, no bracketed
paste, no kitty encoding — accepted fine). Response `● PTY_SPIKE_OK` appeared on screen at
+4.42 s. Full sequence in `runs/full/snapshots/003–013`.

## Resize

`master.resize(90x25)` + `parser.screen_mut().set_size(25, 90)`. Claude Code picked up
SIGWINCH on its own and repainted within 3 s (3,022 bytes of repaint traffic): text reflowed
to 90 columns, input box and status row redrawn at the new width. **No nudge (Ctrl+L /
space-backspace) was needed** — the nudge path in the harness never triggered. One expected
artifact: shrinking 35 → 25 rows scrolls the top of the welcome box off-screen (normal
terminal semantics, content above the viewport is simply gone since scrollback was 0).

## Caveats for the real (tui-term) embed

1. **Synchronized output (mode 2026):** vt100 doesn't implement it, and claude brackets its
   repaints in `?2026h/l` regardless of the DECRQM answer. Polling `screen.contents()` can
   therefore sample a mid-frame state. At 200 ms polling we never saw visible tearing, but a
   60 fps renderer might occasionally show a partial frame. Cheap fix: we control the byte
   feed — buffer bytes between `ESC[?2026h` and `ESC[?2026l` and feed each frame to the
   parser atomically (or report `?2026;2$y` and honor it the same way).
2. **Mouse:** claude enables SGR mouse reporting (1000/1002/1003/1006). If the embed wants
   mouse support, translate ratatui mouse events into SGR mouse reports on the master.
3. **Keyboard:** claude pushes kitty keyboard flags (`ESC[>1u`). Plain legacy bytes work
   (proven by the round-trip), but chords like Shift+Enter will need kitty-encoded input
   for full fidelity.
4. **Theme:** answer OSC 11 with the app's actual background color so claude's theme
   detection matches the host UI.
5. **RSS:** budget ~350 MB per embedded claude (it's node).
6. Scrollback: run vt100 with scrollback > 0 if you want history above the alt screen
   transitions (the harness used 0).

## Responder list the real app must implement

Answer immediately, on the raw output stream, before/independent of rendering:

| Trigger | Reply |
|---|---|
| DA1 `ESC[c` / `ESC[0c` | `ESC[?1;2c` |
| DA2 `ESC[>c` / `ESC[>0c` | `ESC[>0;10;0c` (future-proofing; not sent by v2.1.202) |
| DSR `ESC[5n` | `ESC[0n` (future-proofing) |
| DSR `ESC[6n` | `ESC[{row};{col}R` from the vt100 cursor, 1-based (future-proofing) |
| XTVERSION `ESC[>q` / `ESC[>0q` | `DCS >\|<app-name> <version> ST` |
| kitty query `ESC[?u` | `ESC[?0u` (future-proofing) |
| DECRQM `ESC[?2026$p` | `ESC[?2026;2$y` if you honor sync-output framing, else `;0$y` |
| OSC 10 `ESC]10;?` | `ESC]10;rgb:xxxx/xxxx/xxxx BEL` (fg; future-proofing) |
| OSC 11 `ESC]11;?` | `ESC]11;rgb:xxxx/xxxx/xxxx BEL` (bg — use the app's real theme color) |

(The harness additionally answers DA3, DECXCPR `?6n`, OSC 4/12, XTWINOPS 14/16/18 — none
were exercised by v2.1.202 but they cost nothing to keep.)

## How to reproduce

```sh
cargo build
./target/debug/pty-claude --out runs/full               # full: responder + round-trip + resize (1 Haiku prompt)
./target/debug/pty-claude --no-roundtrip --out runs/dry # no prompt spend
./target/debug/pty-claude --no-responder --no-roundtrip --out runs/noresp
```

Artifacts per run: `raw.log` (all child bytes), `queries.log` (query→reply, hex),
`sequences.log` (escape-shape inventory + vt100 unhandled report), `snapshots/NNN.txt`
(screen on every change), `run_summary.txt`.
