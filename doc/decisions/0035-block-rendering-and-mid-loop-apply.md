# ADR 0035 — Block rendering and mid-loop apply

Status: accepted (built 2026-10-10)
Relates to: ADR 0024 (live-edit doc swaps), ADR 0034 (`sme-live` v0), ADR 0029 (songs), ADR 0031
(determinism justified by reproducible renders). Followed by ADR 0036 (the `sme-live` protocol).

## Context

`Engine::render` rendered a whole loop in one call, and the session pushed whole loops into a ~22 s
ring, so an edit was heard a ring + up to a loop later (≈ 20–37 s; `sme-live` v0 cut the ring to 4 s,
still loop-bound). The workflow needs an edit heard ~instantly, with glitches tolerable but no
change to offline renders.

## Decision

**D1 — A loop renders in blocks.** `Engine::begin_loop(doc, snapshot) → LoopRender`, then
`LoopRender::render_block(eng, n)` any number of times, then `finish(eng) → RenderResult`.
`begin_loop` is everything `render` did before its per-sample loop (modulator evaluation, the
control and note plans, carried voices, MIDI reconciliation, bus setup); `render_block` is the
per-sample loop over the next frames; `finish` builds the next snapshot. `render()` is exactly the
three — **the golden hashes are unchanged** — and any block split renders the same samples and
snapshot (tested at 1/7/64/4096). Events and seeds keep using the frame position within the loop.
The per-voice map key no longer allocates per frame.

**D2 — `LoopRender::retarget(fork, new_doc)`** continues the running loop under an edited doc from
the current position. The new loop is re-evaluated from the loop-start phases (modulators are pure)
and seeded with the *current* runtime state:
- carried voices (envelope clocks continuous); voices whose note route is gone are released at the
  edit point; an instrument whose synth type changed re-strikes its held voices;
- every smoothed destination by path (a value moves from where it is), cursors seeked to the edit
  point with the target set from the latest contribution;
- note events: what already played is skipped; where a source's sounding voicing differs from the
  new doc's latest event before the edit point, that voicing is applied **at** the edit point (same
  prob rolls — same `loop_id`, same frames);
- MIDI emitted state, glides in flight;
- a loop-length edit keeps the **beat** position (a tempo edit scales the frame; extra steps at the
  same tempo don't move it; past the end of a shortened loop ⇒ the loop ends now).
It returns `EditKind::{Value, Structural}`. Retargeting to the same doc is bit-identical at any
position (tested), so the mechanism adds nothing but the edit.

**D3 — Retiring buses.** A removed / chain-edited / retyped bus is no longer cut: it moves to
`Engine.retiring`, takes no sends, rings out and is dropped once silent (≤ 15 s). Offline renders
never change a bus mid-session, so goldens are unaffected.

**D4 — Live gains (mute/solo).** Per-instrument gain targets ramp over ~10 ms, applied before the
dry mix and the sends, carried across loops on the engine. Unity skips the multiply — every offline
render is byte-identical. (`Engine::set_live_gains`, `LoopRender::set_gain_targets`,
`Session::set_live_gains`.)

**D5 — Session live mode** (`SessionConfig::live_block = Some(n)`, a separate worker): each loop is
rendered in `n`-frame blocks into a small ring (`sme-live`: 1024 frames, ~170 ms). Before each block
a pending doc edit is taken: the controller swaps (same-id) or is rebuilt (structural), the engine
is **forked**, the loop retargeted on the fork, and old and new renders are crossfaded with an
**equal-gain raised cosine** — 30 ms for a value edit, 250 ms for a structural one (unchanged voices
stay at unity gain; the whole-loop path's equal-power fade boosted them ~3 dB). Reverb tails survive
because the fork clones the buses. Each edit emits an `Applied { doc_rev, loop_id, abs_frame, pos,
kind, xfade_frames }`; each block a `BlockTelemetry { loop_id, abs_frame, pos, total, frames, peak,
meters }`; MIDI goes out block by block. Live gains are taken per block. Without edits the live
worker streams exactly the whole-loop worker's audio (tested).

**D6 — Budget.** Measured (release): the loop-top work (controller pick + `begin_loop`) is ≤ 6 ms
for every fixture, and 1024-frame blocks average 0.5–3 ms (7–40× real time); occasional spikes of
15–40 ms (scheduling noise) sit well inside the 170 ms ring. Pinned by a release-only test.

## Consequences

- `sme-live` now applies an accepted save mid-loop: ≈ 300 ms debounce + ≤ ~200 ms, instead of a loop.
  `status.json` gains where the edit landed (`applied_at_s`, `applied_kind`), the loop position and
  per-instrument meters.
- Live-mode timing depends on when an edit arrives, so live output isn't reproducible run to run —
  by design; offline renders (whole-loop worker) still are, bit for bit.
- ~~Not yet: keeping the Orrery's voyage across a *structural* edit (the controller is rebuilt, as in
  ADR 0024) — the plan's "unless root/mode or the chord/bass modulators change" refinement is open.~~
  Done — see the amendment below. The whole-loop worker's LiveEdit swap (TUI / `sme-play`) still uses
  its ADR 0024 equal-power fade.

## Amendment (2026-10-10) — the Orrery keeps wandering through live edits

The approved plan's rule: the Orrery keeps wandering through an edit, and starts again from home only
when the harmony's root or mode changes or the chord/bass modulators are restructured. Two gaps
stood against it:
- **A structural edit rebuilt the controller**, so the voyage restarted from home with its phrase
  clock at zero.
- **A value edit put the authored chord back.** `swap_doc` replaced the operating point with the
  authored doc, so the chord, bass, arp and accents snapped home until the next phrase boundary
  (up to `phrase_loops`, 4–8 loops in a tamed piece). It also kept the voyage after a root or mode
  edit, still playing chords of the old key.

- **D7 — A voyage's footing** is the key (root, mode) plus the chord and bass carriers: the chord
  modulators (primary and extra) and the bass, by id, type, output and note source (steps,
  generator or phrase).
- **D8 — An edit that keeps the footing keeps the voyage** (`Controller::follow_edit`): the same
  `OrreryState` (voyager, gravity, Arranger, phrase clock, governor), with its config, harmonic
  floor and preference re-read from the new doc. Each modulator the edit didn't touch (its authored
  Orrery-writable content unchanged) keeps what the voyage wrote into it. One the edit did touch plays
  as written, so the edit is heard, until the next phrase rewrites it. Value edits do this in
  `swap_doc`. Structural ones go through `Controller::rebuilt`: a fresh controller (drift travel
  undone, as before) that the old one's voyage moves into. Both the live worker and the whole-loop
  worker use it.
- **D9 — An edit that moves the footing starts a voyage from home** (`start_voyage`, the code that
  turning the Orrery on runs): a new key gets chords in that key, and a restructured chord or bass
  gets a voyage that knows its carriers.
- Renders never edit a doc, so the goldens are unchanged.
- **Verified** (`controller::tests`):
  - **Value and structural edits keep the voyage.** An edit mid-phrase (a timbre value, then a new
    LFO and route) leaves the voices exactly as the unedited run has them. The next six loops match
    it too.
  - **An edited voice plays as written.** Re-voicing the chord plays the new voicing while the bass,
    arp and accents keep the voyage.
  - **A moved footing restarts.** A new root, a new mode, a second chord track and an unbound bass
    each play like a fresh controller on the edited doc.

  The two keep-the-voyage tests fail when every edit restarts the voyage.
