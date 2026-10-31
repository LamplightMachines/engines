# ADR 0036 — The `sme-live` protocol (JSON lines over stdio), mute/solo, song audition

Status: accepted (built 2026-10-10)
Amended by: ADR 0040 (the panel redesign: protocol additions, the panel, stem previews).
Relates to: ADR 0034 (`sme-live` v0, pieces), ADR 0035 (live mode, mid-loop apply, live gains),
ADR 0029 (songs). Consumed by ADR 0038 (the VS Code extension, P7).

## Context

`sme-live` v0 only watched a file and wrote `status.json`. The VS Code extension needs to drive it:
send the editor buffer (saved or not), mute/solo voices, switch hold ⇄ drift for fine-tuning, audition
song blocks, and receive what's happening (applied revisions, refusals, meters, the loop position,
step positions for highlighting) — without polling files.

## Decision

**D1 — JSON lines over stdio.** `sme-live <slug> --stdio` reads one command per line from stdin and
writes one event per line to stdout (logs to stderr). The contract is
`contracts/sme_live_protocol_v1.md` (`protocol: 1`). Commands: `doc`, `play`, `stop`, `restart`,
`mute`, `solo`, `clear_solo`, `control`, `song`, `preview`, `limiter`, `status`, `quit` (EOF = quit).
Events: `ready`, `accepted`, `applied`, `rejected`, `unchanged`, `external_change`, `tick` (≤ 20 Hz:
loop, position, loop-synced step positions, per-instrument meters, master, limiter GR), `loop`,
`control`, `gains`, `limiter` (`on`, after a `limiter` command), `song`, `preview`, `underrun`,
`playing`, `stopped`, `finished`, `error` (with the command's `id`). File mode (no `--stdio`) behaves
as before; both modes write `status.json` and history, and the file watcher stays on in `--stdio`
mode (Claude's edits on disk ⇒ `external_change`). Revisions are content-addressed, so the editor's
buffer and the same text on disk are one revision.

**D2 — The player is a command/event core.** `LivePlayer::handle(cmd)` and `tick()` push events into
an outbox the binary prints; the session can be stopped / (re)started (an epoch counter tells the
device shell to re-attach its output stream). A host error (bad JSON, unknown command or
instrument) is an `error` event, never a crash.

**D3 — Mute/solo** are live gains (ADR 0035 D4): any solo silences every non-soloed instrument;
applied per block with a ~10 ms ramp; meters are post-gain.

**D4 — `control` is the fine-tuning toggle.** `optimize: "hold"` also re-applies the doc, so the
controller drops drift's wandering and plays **exactly what's written** (a plain `set_control(hold)`
would freeze the drifted values). The override wins until the doc's own `optimize` changes. Takes
effect at the next loop top (the doc reset immediately).

**D5 — Song audition.** `song loop_block K` plays block *K*'s materialized doc as a plain loop in the
running session (a mid-loop swap; edits to the doc re-materialize the block, so live editing works on
a block). `song play_from K` restarts the session on the song from block *K* — `SongDriver::start_at`
offsets the song clock, so block *K* starts exactly as it would in the full song (its own seed and
controller reset; later seams included); edits are kept and snapshotted but not applied while the
song plays (ADR 0029). `song stop` returns to the doc's matrix. Terminal equivalents:
`--loop-block K` / `--play-from K`.

**D6 — `preview N`** renders N seconds of the playing doc offline (the whole-loop worker, so it's the
reproducible render) on a background thread into `.live/preview.wav`, with peak and LUFS.

## Changed from the plan

The approved plan's P5 sketched the protocol a little differently; what shipped:
- **No JSON Schema.** The plan said "`contracts/sme_live_protocol_v1.md` plus a schema"; only the
  markdown contract exists, and it is the spec (field by field, with ids and error rules).
- **No `open` command.** It was folded into the CLI argument: the piece is chosen at spawn
  (`sme-live <slug> --stdio`), so one process plays one piece.
- **`tick` is ≤ 20 Hz**, not the planned ≤ 30 Hz.
- Added since: the `editor { unsaved }` command (P9, for Claude's skills) and the `ready.key` /
  `loop.voicing` fields (ADR 0038), all within `protocol: 1`.

## Consequences

- The extension (P7) can be a thin client: spawn `sme-live --stdio`, send the buffer debounced, render
  events. Claude can keep editing the file on disk; the extension learns via `external_change`.
- Verified end to end over pipes (headless): ready → ticks → accepted/applied → rejected/unchanged →
  mute → hold → loop_block / play_from / song end / stop → preview → errors → quit.
- MIDI-out isn't wired in `sme-live` yet (the session's MIDI is drained); `sme-play --midi` remains
  the MIDI path.
