# ADR 0048 — MIDI mirror tracks for Live: an audio instrument's notes as MIDI, live and as clips

> **Published trimmed.** This record also decides parts of tools that stay closed (a companion's MIDI sending and Live tracks).
> Those sections are left out here; the decisions below are the ones the open code implements, word for word.
> It was written before the rename to Engines: `sme` is `engines` throughout (`Engine::set_midi_mirror` is now the neutral `Engine::set_note_tap`; `sme-song` is `engines-song`).

Status: accepted — 2026-10-22.
Relates to:
- ADR 0026: the `midi` instrument type. An *audio* instrument can also emit its notes (a "mirror").
- ADR 0028: phrase tiles. Pass marks are cut at phrase passes.
- ADR 0043: per-block song renders. The MIDI log sits next to the per-block stems.
- CE-R7.3: determinism. The mirror only reads the voice state the synths already read; audio is
  untouched and no golden hash moves.

## Context

A DAW user wants **an instrument's notes as MIDI**, to double or replace an engine voice with a DAW
instrument, both while composing and in the finished set (as clips aligned with the stems). The engine
already turns a `type:"midi"` instrument's voice set into Note-On/Off, but only `type:"midi"` instruments
emit MIDI, and a parallel `midi` instrument fed by the same modulator is not a mirror: audio voices linger
in their release tail and count toward polyphony, so steals differ. Nothing recorded where a phrase pass
starts either, so a render couldn't be cut into one clip per phrase.

## Decision (the open part)

**D1 — The mirror (engine).** `Engine::set_midi_mirror(set of instrument ids)` makes those instruments
emit their held-voice set as MIDI, alongside their audio. It is a shell setting, never doc content (no
schema change). The diff runs where the `type:"midi"` diff runs — after each note event has landed and
polyphony is enforced, and at loop start — and reads only the voice state the synths read, so audio is
bit-identical whether the mirror is on or off.
- Notes are keyed `source::voice_id` (two sources may both author `p0`).
- A voice that re-articulates on the same pitch re-strikes (Off then On), so the MIDI matches what
  the audio plays; within a frame, re-strike Offs, then Ons, then plain Offs (legato overlap kept).
- Note number = `round(60 + root + offset)`, as ADR 0026. Not mirrored: `cents_offset`, unison detune,
  `shift`, dual `osc_b_ratio`, glide. MPE is a later follow-up.
- Velocity: the authored voicing / phrase-note velocity, else 100.
- Mirror events carry the **instrument id**, not a channel: ports and channels are the shell's
  (ADR 0026), and an offline log needs identity, not channels.
- Mirror state lives in its own snapshot field (`midi_mirror`, skipped when empty: legacy snapshots
  stay byte-identical), with ADR 0026's flush rules: disable, removal, route removal (now reconciled
  at a live edit's switch point, for `midi` instruments too), session exit.
- **Eligible:** any instrument that receives notes — unpitched instruments send their notes as
  triggers; binaural unless its `root_hz` pins the pitch; never `type:"midi"`.
  (`Instrument::can_mirror_midi`, the one rule every tool uses.)
- Mute and solo are audio-only (engine live gains): they never touch MIDI.

**D2 — Phrase-pass marks and the per-loop note log (engine).** Each note source reports where its
passes start: phrase arms one mark per tile (entry id, ordinal, realized-or-not, jitter margin), step
and generator arms one per pattern cycle. With `Engine::set_note_log(true)` a render also reports, per
loop, which sources feed each mirrored instrument (song blocks swap them). A tile ends at the next
mark of the same source — a free-run tile straddling the seam may be revised by an Orrery refit at the
loop top, so consumers dedupe by (source, ordinal).

**D3 — `sme-song` writes a MIDI log with every stems render.** `<stems dir>/midi.json`
(`sme-midi-log/1`): sample rate, bpm, frames, key, the blocks, per loop {start, feeds, Orrery chord},
per source the deduped tile marks, per instrument the notes `[start, end, key, velocity]` in absolute
frames. It covers **every eligible instrument**, whatever the Live set's switches say, so turning MIDI
on later needs no re-render. Inside the stems directory it is protected by `sme-clean` like the stems.

Left out: D4–D8 (the routing map's per-slot MIDI switch, sending the notes from the live player, MIDI twin
tracks and clips in a Live set, their reference sets). Sending notes to MIDI ports from the live player is
a companion's job; the free player taps no notes.
