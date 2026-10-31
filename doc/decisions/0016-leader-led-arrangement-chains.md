# ADR 0016 — Leader-led arrangement chains + per-track organic returns (Phase 5b)

- **Status:** accepted
- **Date:** 2026-06-26
- **Implements:** the Phase 5 sub-deferral in [ADR 0015](0015-orrery-dynamics-and-home.md) (true
  per-track tension/home) and ADR 0010 §6/§9-row-5. **Grounded in a music-theory expert review**
  (summarized below).
- **Plan:** `doc/plans/phase-5b-per-track-tension-and-home.md`.
- **Code anchors:** `crates/sme_core/src/theory/melody_fit.rs` (new), `crates/sme_core/src/orrery/voyager.rs`
  (`LeaderFit`/`AdvanceOpts`, leader fit + cadence-aware return), `crates/sme_core/src/controller.rs`
  (per-track clocks, `return_hazard`, `draw_track_returns`/`pick_leader`/`leader_fit`/`restore_tracks`,
  `orrery_step`), `crates/sme_core/src/telemetry.rs` + `crates/sme_tui/src/ui.rs`.

## Context

Phase 5 shipped a thin `home == per_modulator` (the melody kept roaming on a global snap). The user
asked for the real model: per-track authored "home" with a **growing return-home probability**, where a
returned track plays its authored content and — including the chord — can return home **before**
`tension_max`; and when a **non-chord** track returns it **takes precedence in the arrangement chain**,
so every other track (incl. the chord) must fit its notes.

## Decision

**Under `home == per_modulator`, each phrase every track draws a seeded return-home Bernoulli whose
hazard grows with `loops_since_home`. A fired track rests to its authored home; the top-precedence
fired non-chord track becomes the leader the chord must fit.**

- **`melody_fit` (new theory primitive)** is the harmonization objective for a melody (arp) leader:
  chord-tone +1 / available-tension (whole step above a chord tone) +0.5 / avoid-note (semitone above)
  −1 / chromatic −0.5 — quality-aware (the natural 11 is an avoid note over a maj7, a tension over a
  m7). `chord_fit` stays the **bass/drone consonance floor** it was built for.
- **Leader precedence bass > arp; one-hits never lead** (followers — a sparse accent shouldn't re-pick
  the chord); **one leader per phrase** (avoid over-constraint).
- **Bass pedal gate:** a returned bass on scale-degree 1 or 5 is a free pedal (the chord voyages above
  it); otherwise the chord must contain the bass as a chord tone (`Scale::degree_of`).
- **Chord return is cadence-aware:** the chord's return-hazard boosts the tonic in `advance` **only when
  poised** (`function_of == Dominant` or tonic ∈ `resolution_targets`) — a prepared resolution, not a
  teleport. `tension_max` stays the hard ceiling.
- **Pacing = per-phrase hazard** `clamp(p0 + k·(n−dwell), 0, 0.6)`: bass rare/long-dwell, arp
  frequent/short, one-hit rare, chord moderate. Seeded via `hash_site` ⇒ deterministic + staggered.
- **Mechanics:** `advance` gains `AdvanceOpts { leader, want_return }`; the leader fit folds into `base`;
  the controller writes surfaces chord-derived then `restore_tracks` each fired track to authored home;
  `restore_home` (Orrery-off, WS5.2) now delegates to `restore_tracks`.

## Theory review (the audit that shaped this — "if you implement only three things")

1. **Don't use `chord_fit` as the harmonization oracle** — it's a consonance floor; maximizing it picks
   bland chords. ⇒ added the quality-aware `melody_fit`. *(Highest priority.)*
2. **Cap to one leader; precedence bass > arp; gate non-chord-tone bass to degrees 1/5** — separates the
   tonic/dominant pedal (musical) from a dissonant pedal (clash) and prevents the no-good-chord
   over-constraint.
3. **Make the chord's organic return bias gravity / be cadence-gated**, not an independent teleport that
   bypasses the cadence and adds large voice-leading jumps.
   Plus: keep the leader melody in the arp surface (the `pitch_walk` pool isn't requantized); a pedal
   bass is its own modulator so the pad voice-leads freely above it; per-phrase **hazard** not the CDF.

## Consequences

- The user's model works: a bass returns to a tonic/dominant **pedal** while the chords voyage above; an
  arp returns to a **recurring motif** that the chord harmonizes (quality-aware, no avoid-notes); the
  chord resolves home organically before `tension_max`, through a cadence. +10 Rust tests (213 total).
- **Determinism (CE-R7.3) + I/O-free core (AC3) preserved:** seeded hazards, pure snapshot restore, no
  wall-clock, controller/voyager-side only. `home == global` and the no-`per_modulator` path are
  **byte-identical** to Phase 5. No schema bump (uses internal hazard defaults); telemetry crosses the
  FFI as JSON (no header change).
- Telemetry: `OrreryTelemetry.leader`; the TUI Orrery panel shows `◀leader bass|arp`.

## Deferred / not done

The **quartal/sus over-constraint escape hatch** (prefer a third-less voicing when the best leader fit
is poor) — the one-leader cap + pedal gate already avoid the double-constraint cases it guards against,
so it is a documented follow-up rather than part of this phase. A fully independent **per-surface
harmonic voyage** (each surface choosing its own chord) remains out of scope — surfaces derive from the
one chord spine; a non-chord track's "home" is its authored content. Per-track hazard/threshold config
in the objectives doc (uses internal defaults now).
