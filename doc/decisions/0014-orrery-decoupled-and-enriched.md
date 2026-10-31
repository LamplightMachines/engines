# ADR 0014 — Orrery decoupled + enriched (ADR 0010 Phase 4)

- **Status:** accepted
- **Date:** 2026-06-26
- **Implements:** [ADR 0010](0010-runtime-control-plane-and-orrery-roadmap.md) Phase 4 (§6 Orrery v2,
  §9 row 4). Resolves the §11 deferral "`chord_fit` weighting + how strongly fixed-pitch context
  vetoes vs. biases."
- **Plan:** `doc/plans/phase-4-orrery-decoupled-and-enriched.md`.
- **Code anchors:** `crates/sme_core/src/orrery/{mod,voyager,gravity}.rs`,
  `crates/sme_core/src/theory/chord_fit.rs`, `crates/sme_core/src/controller.rs`,
  `lab/tests/fixtures/loop_orrery_roles.json`.

## Context

After Phases 0–3 the Orrery was still shallow: it bound to **hardcoded demo modulator ids**, ignored
read-only voices, only rewrote pad/bass/arp, and could happily voyage onto chords that clash with an
uncontrollable held drone. Phase 4 makes the harmonic layer **general and rich** without a schema bump
(v0.8 already carries `orrery_role`, `bind.oneshot`).

## Decision

**Bind by role, build a harmonic floor, and derive all four surfaces.**

1. **Role-based binding.** `Controller::orrery_config` resolves each role from
   `modulators.<id>.orrery_role` first, then `movement.bind.<role>` ids, then the demo defaults
   (`role_mod` / `resolve_oneshots`). `OrreryConfig` gains `oneshot_mods` + `context_mods`. A modulator
   that is `orrery_disable` or `orrery_role: context` is read as context (never rewritten), per D1.

2. **Fixed-pitch context (the harmonic floor).** New pure `theory::chord_fit(chord, scale, fixed)`
   scores a chord's consonance with sustained pitch-classes (shared tone +1 … semitone −1, averaged).
   `Controller::fixed_pitch_context` harvests the sounding pitch-classes of the context modulators;
   `voyager::advance` adds `w_fit · chord_fit` into the per-node **`base`** score. Folding it into
   `base` (not the preference/noise terms) means the **gravity-default counterfactual respects the
   drone too**, so the antithetic phrase-window baseline stays musical (D1). `w_fit` is a `Gravity`
   field; **the policy is a soft bias, not a hard veto** — a clashing chord is heavily penalized but
   never removed from the field (keeps the voyage generative, no dead-ends).

3. **Derived surfaces + variable pools.** After the chord is chosen, `write_oneshots` places seeded
   **chord-tone accents** (1..=3 hits, register-placed) into each one-shot modulator's steps —
   replacing the old `enabled`-toggle stub for that role — and `vary_arp_pool` varies the arp
   `pitch_walk` pool's **length (density) and octave** per phrase. Both are seeded via `hash_site`
   sub-streams disjoint from the numeric ±δ stream, so determinism holds. The harmonic core (pad/bass)
   and the `apply_orchestration` `enabled` thinning of non-core instruments are unchanged.

## Consequences

- Orrery works on **non-demo docs**: `loop_orrery_roles.json` binds purely by `orrery_role` (ids
  `pad_voicing`/`sub_bass`/`glints`/`chimes`/`held_fifth`) and drives chord+bass+arp+one-shot while a
  held-fifth drone biases the voyage. Covered by tests + an audible `make tui` check.
- **Determinism preserved (CE-R7.3):** the enriched four-surface trajectory is bit-reproducible across
  runs (test `enriched_orrery_is_deterministic_across_runs`); the new randomness rides disjoint
  `hash_site` sub-keys, so the numeric ±δ pair is byte-identical to before on docs without the new
  surfaces. The antithetic baseline stays valid (AC2) since `chord_fit` lives in `base`.
- **`optimize=pursue` + `movement=orrery`** still learn together (existing session test) — the two axes
  remain orthogonal (ADR 0011).
- +7 Rust tests (role resolution ×2, `chord_fit` ×3, drone bias, context harvest, one-shot accents ×2,
  variable pool, enriched determinism, non-demo surfaces). No FFI/schema change.

## Deferred / not done

The voice-change governor, `tension_max` global return, authored-"home" snapshot, and the Orrery-off
revert are **Phase 5** (ADR 0015). A configurable arp-density band (vs. the internal default range) and
generated melody `step_seq`s (beyond `pitch_walk` pools) are later refinements. `chord_fit` is keyed by
the *currently authored* context content; a context modulator changed live mid-voyage is re-harvested
only on the next voyage rebuild.
