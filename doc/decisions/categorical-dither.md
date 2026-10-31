# Categorical A/B dither

**Status:** implemented (2026-06-21). Extends the controller (CE-R8). Not a numbered ADR;
records a design choice that touches the antithetic-baseline commitment.

## Problem

The controller dithers **numeric** params as symmetric antithetic ±δ pairs (`plus = base+δ`,
`minus = base−δ`) — the basis for ΔCLAP and the online RLS gradient. Several musically
interesting levers are **categorical** and have no ±δ: an instrument's `enabled` flag, its
oscillator `wave`, an LFO `wave`, a euclid generator's `pulses`/`rotation`, a `pitch_walk`'s
`order`. We want the controller to vary these too, to keep the music moving, without an LLM.

## Decisions (chosen by the user)

1. **Mechanism — A/B in the dither pair.** On a categorical loop the pair flips one lever:
   `plus` = a new value (played live), `minus` = the **current** value (the no-change
   counterfactual baseline). ΔCLAP for the loop is `score(plus) − score(minus)` — a same-loop,
   pair-based contrast, so the "never raw loop-to-loop diff" commitment holds (the symmetric ±δ
   is just the continuous-param specialization of "contrast against a matched baseline"). This
   mirrors the Orrery's `harmonic_move` precedent (chosen vs gravity-default).
2. **Scope — instrument + modulator.** `instruments.*.enabled`, pitched `instruments.*.params.wave`,
   LFO `modulators.*.wave`, `modulators.*.generator.{pulses,rotation}`, `modulators.*.pitch_walk.order`.
   Noise `source` has a single option (`filtered_noise`) → no alternative → skipped.
3. **When — all modes** except `hold` (which freezes the operating point). Sprinkled at a low
   per-loop probability.

## How it works

- `categorical` module enumerates `CatCandidate { path, options }` (≥2 options each, current
  included) — pure/deterministic, a function of the doc. Integer levers (`pulses`/`rotation`)
  offer the ±1 neighbours so changes are gradual.
- `Controller::categorical_pick` rolls `DitherConfig.cat_change_prob` (default 0.15) on a hash
  sub-stream **disjoint** from the numeric drift/pursue keys, so enabling categorical dither
  leaves numeric determinism untouched. It picks a candidate + an alternative value, both seeded.
- **Silence guard:** an `enabled=false` flip is suppressed if it would mute the last enabled
  instrument (no dead air).
- A categorical loop carries `delta = 0` (so the numeric RLS update is skipped — `if δ≠0`) and
  records `perturbation.categorical_move { path, from, to }`. `plus` commits on re-base, so the
  change sticks (drift-style variation). `SAMPLE_SCHEMA_VERSION` 0.3 → 0.4 (optional, backward
  compatible).

## Deferred

Online **move-outcome learning** over categorical (and Orrery) moves — score chosen-vs-baseline
over a phrase window and update a per-move value. The corpus already records everything it needs;
this shares the Orrery Shot-3 credit-assignment path.
