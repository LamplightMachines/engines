# ADR 0005 — Octave-pedal `shift`/`shift_mix` lever + the param-range normalization seam

> **Amended by [ADR 0054](0054-native-units-schema-v0-9.md) (2026-10-27, schema v0.9):** the seam's rule now covers every param, not just `shift`. The doc writes Hz, seconds, cents and dB under unit-suffixed keys, and the seam gains curves: exponential for Hz, dB for gains, linear for times. The doc stays native in memory (an amendment to ADR 0054 reversed its P8), and every engine read goes through the seam (`Instrument::norm`/`native`, `BusEffect::norm`), so the controller and modulation still work in `0..1`. `shift` is now `shift_st` and the bus `predelay` is `predelay_ms`. A route's depth is in its dest's unit, so §2's `depth: 0.30` on `shift` is `depth_st: 14.4`.

- **Status:** accepted
- **Date:** 2026-06-09
- **Related:** `doc/decisions/design-rationale.md` §2.6 (normalized control values; pitch as
  offsets from a root), §7.4–7.5 (engine legibility is the quality bar; the sweep rig picks the
  steerable terms), ADR `0004` (the legibility-gate findings this extends), `PLAN.md`
  (CLAP-distinctness experiments: resonance/drive/attack invisible, cutoff/sustain/master_gain
  legible). Engine: `crates/sme_core/src/{loop_doc,engine,controller,sweep,feature,contract}.rs`.

## Context

The POC's quality bar for the engine is **CLAP legibility** (design-rationale §7.4): the synth
must occupy a region where target perceptual terms have dynamic range as params vary, or the
learner has nothing to steer. A chain of SNR-grounded sweep experiments (recorded in `PLAN.md`)
established a clear generalization:

> CLAP-legible levers change the **steady-state spectrum** (e.g. `cutoff`) or **level**
> (`sustain`/`level`/`master_gain`) inside the scored window. **Transient/fine-detail** params
> (`resonance`, `drive`, `attack`/`decay`/`release`) are CLAP-**invisible** — the settled scoring
> window excludes onsets, and CLAP reads gross spectral shape, not fine timbre.

The implication for "make the engine more CLAP-distinct" is to add levers that make a **large
steady-state spectral move on the dominant instrument**. The highest-leverage candidate is
**register**: transposing an instrument's whole harmonic series by N semitones moves the spectral
centroid by a *factor* (±12 st = ×2 / ÷2), far more than `cutoff`'s ~±15%. It also addresses a
standing problem in the example fixtures — the `poly` pad dominates the mix — by letting us place
instruments in distinct registers.

We chose to add it as a **classic octave pedal** (POG/OC-2 style) rather than a bare transpose: a
`shift` interval plus a `shift_mix` wet/dry blend, so a voice can carry its fundamental *and* the
shifted copy at once (the octave-doubling shimmer that is idiomatic to ambient).

## Decision

### 1. Two new per-instrument params

- **`shift`** — transpose interval in **semitones, native range −24..+24** (±2 octaves). Stored in
  the doc in musical units (like note `offset`s), not normalized.
- **`shift_mix`** — wet/dry **crossfade** in `0..1`: `s = (1 − mix)·dry + mix·wet`. `mix = 0` is
  the untouched dry path; `mix = 0.5` is equal octave-doubling; `mix = 1` is a full register shift.

Synthesis: each *pitched* voice runs a **second oscillator** at `pitch + shift` (with its own
deterministic phase `phase_rad_shift`, so CE-R7.3 bit-reproducibility holds) and blends per the
mix. Both oscillators share glide, filter, drive, and ADSR. Defaults `shift = 0, shift_mix = 0` ⇒
pure dry ⇒ **bit-identical to pre-feature output** (backward compatible). `filtered_noise` (the
unpitched source) is untouched.

### 2. The param-range normalization seam (the architectural commitment)

`shift` is the first param whose native range is **not** `0..1`. The controller (`δ`/`max_change`/
guardrails/`dither_candidates`), the sweep rig, **and** the matrix modulation pipeline
(`base + Σ depth·LFO`, slewed, `clamp01`) all assume params live in `0..1`. Rather than special-case
`shift` in each, we adopt a single rule:

> **Everything internal is normalized `0..1`. Native units exist only at the JSON/doc and CLI
> boundaries.** Conversion happens at exactly those boundaries via a small table keyed by the
> path's leaf name (`param_range`/`param_norm`/`param_denorm` in `loop_doc.rs`; `SHIFT_RANGE`
> is the shared source of truth).

Boundary conversions (each a **no-op for every existing `0..1` param**, since their range is
identity):

- **Controller** — reads base via `get_param_norm`, clamps `δ` in normalized space, writes via
  `with_param_norm`; reports `Perturbed.delta` back in **native** units so `plus = base + δ` holds
  for any param. `dither_candidates` gates each param against *its own* range.
- **Sweep** — steps across the param's native range and emits native values (a `shift` sweep
  reports semitones); monotonicity/range metrics in the lab analyzer are x-scale-invariant.
- **Modulation** — `base_of(dest)` normalizes the modulation base; the capture-mirror denormalizes
  so the effective-state JSON reports native units. A `depth: 0.30` bipolar LFO on `shift` then
  sweeps ≈ ±14 semitones — the same depth convention as every other mod destination.
- **Guardrails** — CLI `--ceiling`/`--floor` are authored in native units and normalized on parse.

This generalizes design-rationale §2.6 ("normalized control values"): control values stay
normalized for the *learning/steering* machinery, but params may be **authored** in whatever unit
is natural (semitones here), with the seam bridging the two.

### 3. Feature roles

`shift` and `shift_mix` are added to the working feature vector's `ROLES` (CE-R8.6a), so the
learner can attribute ΔCLAP to them like any other lever (`DIM` 19 → 21).

## Alternatives considered

- **Store `shift` normalized `0..1` in the doc** (0.5 = no shift), like `cutoff`/`attack`.
  *Rejected:* the user wanted musical authoring (semitones, like note `offset`s). The seam makes
  native-unit storage cheap and keeps the doc readable. (Trade-off: `shift` is the one `params`
  entry not in `0..1` — acceptable, and the seam contains the blast radius.)
- **Additive octave-pedal mix** (`dry + mix·wet`, true POG/OC-2: dry stays at unity, octave stacked
  on top). *Rejected as default:* level rises with `mix`, which **confounds** with the `level`
  lever CLAP already reads — it would muddy the very experiment the lever is for. The crossfade
  keeps energy ~constant and isolates the spectral move. (Additive, level-compensated, is a noted
  follow-up.)
- **Special-case `shift` at each call site** instead of a seam. *Rejected:* fragile and
  non-extensible; the next non-`0..1` param would repeat the work. The seam is ~3 helpers + one
  rule.

## Consequences / findings

- **`shift` is a real but *conditional* lever.** On `fake_clap` (the engine-structure check) it is
  **perfectly monotonic** for brightness (mono +1.0, dynamic range ~0.21 at an open filter). But
  across 4 randomized operating points its SNR is ~0.9 (vs `cutoff` ~15.2): the register effect is
  **gated** — when context randomizes `cutoff` low (the filter removes the shifted harmonics) or
  `shift_mix` low (dry), the move disappears. **Treat `shift` + `shift_mix` + `cutoff` as a coupled
  set.** This is consistent with the generalization (register *is* steady-state spectrum), not a
  contradiction of it. A real-CLAP (ONNX) SNR sweep is still TODO.
- **Backward compatible / deterministic:** defaults give the pre-feature dry path bit-for-bit; the
  normalized-base change is a verified no-op for existing modulation fixtures.
- **Extensible:** any future param with a non-`0..1` native range now has a home (`param_range`)
  and works across the controller/sweep/modulation pipeline for free.

## Out of scope / follow-ups

- An additive, level-compensated pedal mode alongside the crossfade.
- Steering `shift` in `pursue` end-to-end on **real** CLAP (mechanically works once roles land).
- Feeding a measured register-sweep shortlist into `contracts/`.
