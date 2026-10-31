# ADR 0015 — Orrery dynamics + home (ADR 0010 Phase 5)

- **Status:** accepted
- **Date:** 2026-06-26
- **Implements:** [ADR 0010](0010-runtime-control-plane-and-orrery-roadmap.md) Phase 5 (§6 Orrery v2,
  §9 row 5). Resolves the §11 deferral "governor control law + `tension_max` hysteresis." Builds on
  [ADR 0014](0014-orrery-decoupled-and-enriched.md) (the resolved role binding) and Phase 1
  (`set_control` re-base).
- **Plan:** `doc/plans/phase-5-orrery-dynamics-and-home.md`.
- **Code anchors:** `crates/sme_core/src/controller.rs` (`HomeSnapshot`, `restore_home`,
  `harmonic_pref`, the governor + tension branch in `orrery_step`),
  `crates/sme_core/src/orrery/voyager.rs` (`return_home`, retention, `last_voice_changes`),
  `crates/sme_core/src/theory/voicelead.rs` (`LeadOpts.retain`),
  `crates/sme_core/src/telemetry.rs`.

## Context

After Phase 4 the Orrery was rich but **memoryless and unbounded**: switching it off froze the last
orreried voicing (the authored note sequences were already overwritten in place), nothing held its
sparseness, and nothing capped how far/tense it could wander. Phase 5 gives it **memory and dynamics**.

## Decision

1. **Authored "home" snapshot + Orrery-off revert (the user-requested behavior).** `Controller::new`
   captures a targeted `HomeSnapshot` — each note modulator's authored `steps` + `pitch_walk` pool and
   each instrument's authored `enabled` — **before** any voyage rewrite. On `set_control`'s
   `orrery: true → false` transition, `restore_home` reverts the **full Orrery footprint** (bound
   chord/bass/arp/one-shot sequences **and** the orchestration `enabled` toggles) to that snapshot and
   drops the voyage (`self.orrery = None`); the next loop plays the **original non-orreried** material.
   It is **targeted** — only the Orrery-written leaves are restored, so timbre drift / pursue /
   overrides on the operating point survive — and a static→static edit is a no-op. Because the TUI `m`
   key, the watched-doc edit, and the FFI all flow through `set_control`, this is **core-only**.

2. **Voice-change governor.** `LeadOpts` gains a `retain: usize` knob (generalizing the pedal) that
   holds up to N common-tone voices, never increasing motion. The Voyager records `last_voice_changes`;
   `orrery_step` keeps a 4-phrase moving average and, when `voice_changes_per_loop` is set, nudges
   retention up/down by ±1 with **hysteresis** (act only when the average leaves the band). Inert (and
   byte-identical to before) when no band is set.

3. **Active `tension_target` + hard `tension_max` return.** `harmonic_pref` re-anchors the preference
   tension band on `movement.tension_target` while keeping the objective valence's *delta from neutral*
   (so the two combine). When the current chord's tension exceeds `movement.tension_max`,
   `orrery_step` forces `Voyager::return_home` next phrase instead of advancing; returning to the tonic
   (the calmest node) drops tension far below the threshold, so the trigger **self-arms** — natural
   hysteresis, no oscillation. `home == global` resets the whole voyage; `home == per_modulator`
   resolves the harmony but passes `regen_melody == false` so the melodic surface keeps roaming its
   current pool (**one sequence resolves while others roam**).

4. **Telemetry.** `OrreryTelemetry` gains `voice_change_avg` (tracked every phrase, governed or not);
   the TUI Orrery panel shows `Δvoices`.

## Consequences

- The headline user ask works: flipping Orrery off (TUI `m` or `orrery:false`) reverts to the authored
  arrangement and resets state; re-enabling rebuilds the voyage from home. Verified by tests + the
  on→off→on **determinism** test (CE-R7.3 / AC4) and an `make tui` check.
- Sparseness is **held in a band** (governor) and tension is **bounded** (repeated forced returns) —
  the Phase 5 exit criteria. +13 Rust tests across `theory`, `orrery`, and `controller`.
- **Determinism + I/O-free core preserved:** snapshot/restore and the governor/return are pure
  functions of (seed + control timeline + operating point); no wall-clock, no new file I/O (AC3). The
  default-config voyage (no `voice_changes_per_loop`, default `tension_max`/`tension_target`) is
  byte-identical to Phase 4. No schema bump (v0.8 already carries the fields); telemetry crosses the FFI
  as JSON, so no header change.

## Deferred / not done

The **full per-track tension Voyager** (independent per-modulator tension aggregates) is a documented
sub-deferral: `home == per_modulator` ships as "resolve the harmony, keep the melody roaming" rather
than a fully independent per-surface voyage. A configurable arp-density band and a `returning_home`
telemetry flag are later polish. Phase 6 (cleanup/solidify) is unchanged.
