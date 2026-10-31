# ADR 0009 — Self-contained synth types (dual-oscillator + modal)

- **Status:** accepted
- **Date:** 2026-06-24
- **Related:** ADR `0003` (Rust core / iOS portability — all new DSP is hand-rolled, I/O-free),
  ADR `0005` (param-range normalization seam — `osc_b_ratio` is the new native-ranged param),
  ADR `0008` (decoupled scoring thread — `Engine: Clone` must stay cheap/deterministic). Code:
  `crates/sme_core/src/synth.rs` (new), `engine.rs` (dispatch + `ActiveVoice.synth_state`),
  `dsp.rs` (wave palette), `loop_doc.rs` (`type` field + `param_range`), `categorical.rs` +
  `controller.rs` (steering), `feature.rs` (φ roles). Schema: `contracts/loop_contract_v0.7.schema.json`
  + `doc/loop_contract_v0.7.json5`. Fixtures: `lab/tests/fixtures/loop_{dual_basic,modal_marimba}.json`.

## Context

The engine had **no synth-type concept**: every instrument ran one fixed per-sample path
(one oscillator selected by a `wave` string → soft-clip → SVF filter → ADSR → pan → buses),
with only two ad-hoc booleans (`is_noise`, the name-based `event` bell). We want the engine to
host **multiple self-contained synth types**, each exposing shared **base** params plus
**type-specific** params, starting with:

1. **`dual`** — a Hydrasynth-style two-oscillator voice: many wave types, each oscillator at its
   own mixer level, with optional DX-style **phase modulation** (osc B → osc A) + self-feedback.
2. **`modal`** — a struck-object physical model: a parallel bank of decaying two-pole resonators
   (marimba, vibraphone, tuning fork, and a generic tunable "resonators" bank).

Hard constraints: **determinism** (CE-R7.3: same `loop_id` + snapshot ⇒ bit-identical output),
the **param-range seam** (ADR 0005), hand-rolled DSP only in the voice path, and the existing
(no-`type`) path staying **byte-identical**.

## Decision

**1. Source-stage seam, not a full `Synth` trait.** A per-instrument `SynthKind` enum
(`Subtractive` | `Dual(DualConfig)` | `Modal(ModalConfig)`) owns only the **signal source**;
the engine keeps the shared `drive → filter → amp → pan → submix → sends` chain. Per-type policy
methods (`uses_engine_filter`, `amp_policy`) let `modal` opt out of the resonant filter + ADSR
and supply its own intrinsic decay — generalizing the existing `is_noise`/`event` special-cases.
Dispatch is an `enum match` (not `Box<dyn>`) so the hot path stays allocation-free and `Engine`
stays trivially `Clone` + deterministic (ADR 0008). *Rejected:* a full `Synth` trait owning the
whole chain — it would duplicate the (well-tested) shared chain into every type and rewrite the
loop body that ~10 tests assert against, for no gain.

**2. Determinism via a typed snapshot enum.** `ActiveVoice` gains
`synth_state: SynthVoiceState` (`None` | `Dual{phase_b,fb1,fb2}` | `Modal{y1,y2}`), serialized in
the `PhaseSnapshot`. `None` is the `Default` and is `skip_serializing_if`, so subtractive
snapshots are **byte-identical** to before. State is zero-initialized deterministically at
articulation (beside the filter reset). Modal vec length is a pure function of the doc
(`num_modes`); if it changes between loops the carried state is deterministically truncated /
zero-extended at seed time. Resonator coefficients are recomputed per loop (not stored).

**3. Additive within v0.7 (no version bump).** Instruments gain an optional `type` field
(absent ⇒ `subtractive`) and type-specific params live alongside the base ones in `params`.
Following the ADR-0020 send-bus precedent (renumbered from 0007 in Phase 6d), this is a backward-compatible additive change: every
existing doc validates and renders bit-identically. A bump would break every fixture for no
semantic gain.

**4. Phase modulation, not true FM.** The dual synth uses PM (`carrier = osc_a(phase_a +
index·mod)`), which is what DX7-style "FM" actually is and avoids the DC-offset frequency drift
of true FM. Osc B reaches the mixer at `osc_b_level` **independently** of whether it modulates A
(`fm_index = 0` ⇒ a plain dual oscillator). Operator self-feedback uses the classic averaged
last-two-samples damping.

**5. Modal = excited resonator bank with intrinsic decay.** A unit impulse at the strike sample
(reproducible off the note-relative clock) excites N two-pole resonators
(`y[n]=x[n]+2R·cos w·y[n-1]−R²·y[n-2]`, `R=exp(−6.9078/(T60·fs))`). Per-mode amplitude uses
strike-position weighting + a brightness tilt, normalized by `sin w`. Note-off shortens the ring
(extra damping); the engine keeps a released modal voice for its T60-derived tail length rather
than the ADSR release, so the ring isn't truncated at the seam.

*Loudness + note-duration coupling (amendment).* Two fixes after field use found modal ~3× quieter
than `dual`/`subtractive` and its ring deaf to note length: **(a)** a per-material output
normalization (`ModalConfig::strike_norm = MODAL_TARGET_PEAK / Σ amp·|sin(π(k+1)·strike_pos)|·bright^k`)
replaces the old flat `0.5` gain, so every preset strikes at ~the same peak (≈0.9 FS at `level` 1.0,
in the sustained-synth ballpark) instead of 0.33–0.56. **(b)** an opt-in `t60_mode: "note"` couples
the fundamental T60 to the driving note's own duration (`gate_s = source step · ring_ratio(t60_scale)`,
with `t60_scale` re-purposed as a trim around unity) so `step_duration` audibly controls ring length;
default `"fixed"` keeps the absolute `t60_scale` map (existing docs unchanged). Note-mode released
voices are pruned on a per-voice tail (`release_tail_for_note`, the step drives it) rather than the
instrument default. The step is a fraction of the master loop, so a stepped source must set
`step_duration` ≈ `1/length` to fill the loop (a `1/4` with `length:16` fires only ~4 of 16 steps).

**6. Steering wired now.** New numeric params are dither candidates automatically (range-gated;
`osc_b_ratio` gets a native `(0.5, 12)` `param_range` entry). New categorical levers
(`osc_a_wave`/`osc_b_wave`, `material`, `num_modes`) are added to `categorical_candidates`.
`type` is **excluded** from all dither (numeric and categorical) — changing it mid-session would
change per-voice state shape, a topology change, not a perturbation. The online learner's φ gains
roles for the new continuous params (`feature::ROLES`, bumping `FEATURE_SCHEMA_VERSION` to 0.2).
The Python **offline** harness keeps its own (smaller) feature space and degrades gracefully
(unknown leaves describe into context/bias only), as it already diverges from the Rust φ.

**7. Anti-aliasing: 2× oversampling for `dual`.** PM + soft-clip `drive` generate content above
Nyquist that folds. The dual source (osc A + PM + osc B mix + drive) runs at **2×** and is
decimated through a 4th-order Butterworth (`Aa2x`, cutoff ≈ 0.45·base-rate). The decimator is
engine-resident per-voice (like `self.filters`) — reset on articulation, carried across the seam —
so it stays deterministic without bloating the serialized snapshot. Measured: a harmonic-FM patch
at 3.5 kHz with index ≈ 1 shows off-grid (alias) energy ≈ 0 relative to its harmonics. `modal`
stays base-rate: its resonator modes are clamped below Nyquist, so it is inherently band-limited.
(`drive` for a dual voice is applied inside the oversampled block, so the engine skips the shared
soft-clip for it.)

## Consequences

- Adding a synth type now means: a `SynthKind` arm + (optional) `SynthVoiceState` variant + a
  source function in `synth.rs`, its params in `param_range`/schema/json5, `ROLES`, and the
  `engine.rs` modulatable-param list. The shared chain, polyphony/voice-stealing, sends, and
  determinism plumbing are reused unchanged.
- The corpus dimensionality grows (new steerable params) — intended; the feature schema and
  steerable-term list remain POC outputs.
- Modal cost is `voices × modes × frames`; `num_modes` is capped at 8 and offline rendering is
  acceptable for the POC. A stolen modal/dual voice drops its state abruptly (a click) — the same
  pre-existing limitation as filters; acceptable for the POC.
- An instrument literally named `event` keeps its bell envelope only when `subtractive`; a typed
  synth ignores the name-based special case.
