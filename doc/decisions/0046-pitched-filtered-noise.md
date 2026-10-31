# ADR 0046 — Pitched filtered noise: a band on each note

Status: accepted — 2026-10-21 (asked for by the user for ninety-lights' `noise`); amended 2026-10-30
(the source band reaches down to 20 Hz, see "Amendment")
Relates to:
- ADR 0005: the native-units seam (nothing new here; all params stay 0..1).
- ADR 0009: synth types (this is the subtractive noise source).
- CE-R7.3: determinism. Unpitched noise stays bit-identical, so the goldens don't move.

## Context

A `filtered_noise` instrument plays white noise through a fixed band. The band is
`band_lo`…`band_hi` (0..1 → ≈ 80 Hz…4 kHz), made of a 12 dB/oct low-pass and a 12 dB/oct high-pass,
and it moves only when those params are modulated. Its voices are triggered by notes, but the note's
pitch never reached the sound. `pitched` was documented as "false ignores note offsets", but the
engine never read it, and `resonance` did nothing on noise (the checker flagged it).

The user wants noise that follows the notes:
- with `pitched: true`, the band-limited noise goes on through a band-pass centred on each voice's
  note: A440 → centre 440 Hz;
- the response falls 24 dB/oct either side of the note;
- `resonance` makes the note itself more pronounced.

The source stays exactly as it is: the `band_lo`…`band_hi` noise. Where the note's band reaches past
the source band, its tails are simply missing, because that noise was never there.

## Decision

**D1 — Opt-in with an explicit `pitched: true`.**
- `Instrument::pitched` becomes `Option<bool>`: unset is `None`, and it isn't serialized, so a doc
  round-trips exactly.
- `Instrument::pitched_noise()` is true only for a subtractive `filtered_noise` instrument that says
  `pitched: true`.
- A noise instrument that leaves `pitched` out, or says `false`, takes the old path **bit for bit**.
  That was every noise voice before this ADR, so no golden hash moves.
- `pitched` defaults to true in the schema, but defaulting noise into the new sound would have changed
  docs that never asked for it.
- `pitched` still means nothing for other instruments; nothing else reads it.

**D2 — The note's band (`dsp::PitchBand`), after the source band.** Per voice, after the existing
`band_hi` low-pass and `band_lo` high-pass:
- **Shape:** a 4th-order Butterworth high-pass and a 4th-order Butterworth low-pass, both at the
  voice's note. Each is two TPT SVF stages, Q 0.541 and 1.307.
- **Response:** ×2, so the note sits at 0 dB. It's −18 dB an octave either side, −42 dB two octaves
  out, and 24 dB/oct asymptotically. Its noise bandwidth is ≈ 0.51 × the note.
- **Which note:** the voice's resolved pitch, so glide sweeps the band and `cents_offset` tunes it.
- **Coefficients:** recomputed only when the note or the resonance moves.
- **State:** lives in `Engine::pitch_bands`. It's created on the first sample, reset on ARTICULATE,
  and pruned with the voices.
- **Keys:** every per-voice DSP map (filters, anti-alias, glide and these bands) is keyed by
  instrument, note source and voice id (`voice_key`). Authored ids like `v1` repeat across sources,
  so two note routes into one instrument used to share, and double-advance, one voice's filter
  state. This was a pre-existing bug the Copilot review found. No golden fixture hit it, so the
  goldens don't move.

**D3 — `resonance` = a peak at the note, loudness held.**
- **The peak:** a peaking boost of `(G − 1)·(SVF band-pass / Q)` at the note. G is +24 dB × res, and
  Q is 1 + 29·res² (broad at first, Q 30 at 1). At 1 the noise reads as a breathy whistle on the note
  (the user's "near-whistle").
- **Loudness held:** for white noise the peak adds energy (+4.4 dB at 0.25, +7.3 at 0.5, +13.9 at
  1). The output is scaled by `sqrt(P(0) / P(res))`, where `P` is the power of the noise **actually
  coming in**, through the voice's source band and then the note's band.
- **How it's computed:** since `|1 + (G − 1)·B|² = 1 + (G² − 1)·|B|²`, `P(res) = P(0) + (G² − 1)·P_B`.
  Both are integrated over the analog prototypes: `P(0)` on a 72-point log grid, and `P_B` with
  `y = Q(ω − 1/ω) = tan θ`, uniform in θ (48 points), which stays accurate at Q 30. It's integrated
  again per voice when the note, the resonance or a source-band edge moves by more than 0.1 %
  (≈ 0.01 dB).
- **Why the source band matters:** a note near or outside the source band keeps its resonance-0
  attenuation. Its peak finds little noise, and the match accounts for that. Matching to white noise
  instead (the first version) let resonance drop such notes by 1.8 dB, the Copilot review's finding.
- **Why:** resonance changes how pitched the noise sounds, not how loud it is. An LFO on
  `resonance` doesn't pump the level, and docs keep their gain staging (no limiter in renders).
- `resonance` now applies to a pitched noise instrument, so `sme-check` no longer warns about it.
  On an unpitched one, the warning says `pitched: true` is what would make it count.

**D4 — Even across notes.**
- **The problem:** white noise through a constant-Q band loses 3 dB per octave down.
- **The fix:** each voice is scaled by `sqrt(1000 Hz / note)`, so `level` sounds the same on any note,
  as it would with pink noise. 1 kHz is the reference.
- **Level against unpitched noise:** at a note well inside a wide source band, pitched noise lands
  within a few dB of the unpitched band at the same `level`.
- **Notes outside the source band:** these still come out quiet. That's the user's design: the source
  band decides what noise there is. (Since the amendment below the band can open under any note down
  to B0, so a low note being quiet is the doc's choice, not the range's.)

**D5 — Tooling.**
- `check::param_applies(kind, noise, pitched, leaf)` takes `pitched`.
- `sme-lsp` completion and `sme-part`'s target choice read it from the doc (`Node::as_bool`).
- The descriptions are updated in the schema (they're the LSP's hover text), the annotated template,
  the skill's engine facts and the README.

## Consequences

- ninety-lights' `noise` (`pitched: true`, band 566 Hz–3.2 kHz) changes sound: each stutter now
  carries its note's band. Its `nz_band` LFO still moves the source band. Notes below ≈ 566 Hz come
  out thin, since there's little noise down there; lowering `band_lo` opens them up.
- Each pitched voice costs 5 SVF stages more than plain noise. Coefficients only update on note,
  glide or resonance changes. The loudness match is ~120 function evaluations, redone at most every
  few hundred samples under slow modulation.
- Offline renders stay deterministic: the match is a pure function of the voice's inputs.

Tests:
- `dsp::tests::pitch_band_*`: centred, 24 dB/oct, resonance contrast at the same loudness, even
  across notes; resonance holds loudness through a source band (notes below it, at its edge and
  inside it).
- `engine::tests`: the band follows the note (A4 vs A5); unpitched noise ignores the note and is
  bit-identical whether `pitched` is false or left out; resonance gathers the noise onto the note;
  two sources that both author `v1` keep their own filter state (both = a + b).
- `check::tests::resonance_applies_to_pitched_noise_only`.

## Amendment — 2026-10-30: the source band reaches 20 Hz

**Context.** `d-engine-room`'s heartbeat wanted a pitched noise thump at D2 (73.4 Hz). The source
band's floor was 80 Hz (`band_lo_hz`/`band_hi_hz` 80..4000, ADR 0054), so a note below ≈ E2 had
little noise under it, and the doc couldn't open the band any lower. B0, two octaves further down,
found almost none.

**D1 — Lower the floor (not a band that follows the note).** The band edges' range is now
**20..4000 Hz**. D4's rule stands: the source band decides what noise there is, and the doc decides
the source band. A note's band following under `band_lo_hz` on its own was the alternative; it would
have taken that choice from the doc.

**D2 — 20 Hz.** The bottom of hearing, and the filters' own clamp (`Svf::set_cutoff` and
`PitchBand::clamp_hz` already stop at 20 Hz). It leaves room under a 30 Hz note's lower skirt. The
TPT SVF is stable there, and the loudness match (D3) integrates the 20 Hz high-pass like any other.

**D3 — One log curve, re-anchored.** `band_hz` is now `20 · 200ⁿ` (was `80 · 50ⁿ`), `hz_band` its
inverse. Every Hz a doc could write before keeps its sound:
- **Without drift or a route,** an authored edge plays the same Hz up to float rounding (≤ 1e-6 of
  itself, ≈ 0.002 cents; `params::tests::the_band_curve_keeps_the_old_ranges_hz`). That rounding
  still moves the output bits of any doc with a noise voice. The goldens with one (`engine/template`,
  `session/drift/template`) were re-blessed for it, as the owner chose over keeping the old curve's
  rounding in the hot path.
- **Under drift,** an edge's normalized position moved, and one normalized unit now spans 7.64
  octaves instead of 5.64. So `max_change` and `max_drift` on a band edge allow about 1.35× more
  movement in octaves than before, and drift can take an edge below 80 Hz. A route's `depth_oct`
  is in octaves already and moves the same Hz as before, though it now clamps at 20 Hz, not 80.
  Drift floors for the band are separate.

**D4 — Amend this ADR,** since it lifts this ADR's "notes outside the source band" consequence, and
note the range in ADR 0054's table.

**Measured** (the heartbeat solo: 66 bpm, master −13.15 dB, resonance 0.8, drive 0, −10 dB, a 0.9-beat
gate at 48 kHz): opening `band_lo_hz` from 80 to 20 Hz lifts D2 by about 3 dB, to within about 2 dB
of D3 at the same settings, read at 72.8 Hz. With the piece's 38 ms-step `heart_env` on top, D2 at
resonance 0.8 still trails D3 by about 5 dB. A Q-20 peak at 73 Hz rings up over about 0.1 s, so a very
short hit wants a lower resonance (at 0.3 D2 is level with D3). That's the peak's physics, not the
band, and it's in the skill's engine facts.

Tests:
- `params::tests`: the band curve's ends and formula, and the old range's Hz kept.
- `dsp::tests::pitch_band_is_level_matched_down_to_b0_through_an_open_source_band`: B0, D2 and D3
  through 20 Hz–4 kHz land within 1.5 dB of 1 kHz, and resonance 0.8 holds their loudness.
- `engine::tests::pitched_noise_sits_a_low_note_in_a_band_opened_below_it`: D2 and B0 through
  20 Hz–4 kHz are within 3 dB of D3, with their strongest frequency at the note (± 4 %); under the
  old 80 Hz floor B0 was more than 10 dB quieter.
