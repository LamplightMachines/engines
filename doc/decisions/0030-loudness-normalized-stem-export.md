# ADR 0030 — Loudness-normalized stem export (`sme-song --stems`)

Status: accepted (built 2026-10-09)
Relates to: ADR 0021 (no master limiter), 0027 (`out_channel` stems — this delivers its "multichannel
stem export" future consumer), 0029 (songs, `sme-song`, the streaming writers + its FLAC amendment
D9a); CE-R7.3 determinism.

## Context

A finished song is mixed in a DAW, which wants **stems**: one file per voice, each at a level that
sits comfortably on a channel strip. `sme-song` wrote only the stereo master. ADR 0027's stem tap
splits by *device pair* (`out_channel` routes; all bus wet lumped into "the rest") — the right shape
for live routing into a DAW, the wrong one for an offline export, which wants every instrument and
every send-return separately and needs no doc annotations.

## Decision

**D1 — Per-source split in the engine, observation-only.** `Engine::set_source_capture` fills
`RenderResult.sources: SourceStems { inst, bus }`: each audio instrument's dry (post level/pan,
pre-bus) and each internal bus's wet return (post `return_gain`), every buffer through the master's
per-sample width × `master_gain` stage (shared `width_gain_in_place`). The master is exactly
Σ dry + Σ wet through that linear stage — no inserts, no bus→bus sends, no master nonlinearity — so
the stems **sum back to the master** (float association only; measured ≤ 2e-7). The master `audio`
is byte-identical with the tap on (pinned); the −δ counterfactual clone turns it off. Kept separate
from ADR 0027's `StemMix` rather than generalizing it, so that working live path doesn't change
rounding. An instrument's reverb lives in its bus's stem — the standard "dry stems + send returns"
split; per-instrument wet would need a reverb instance per (instrument, bus) pair.

**D2 — The layout is fixed at session start.** `engine::source_layout(doc)`: every non-midi
instrument (including authored-disabled ones — an objectives `overrides` entry can enable one) in id
order, then every non-`external_return` bus. `SessionConfig::source_stems` makes the ring carry
`2 + 2N`-wide frames — the **exact master** on 1/2 (for the tail check, the master's peak/clip report,
and a live |master − Σ pairs| self-check), then one pair per source. A source absent from a loop is
silence on its pair; one absent from the layout (only a structural LiveEdit can add one; songs and
`sme-song` never do) is dropped from the stems, while the master stays whole.

**D3 — Normalization: BS.1770-4 integrated loudness, one static gain per stem.** Default target
**−18 LUFS** (`--stem-lufs`), sample-peak ceiling **−1 dBFS**: `gain = min(target − I, −1 − peak)`.
`loudness.rs` is a pure meter (K-weighting from libebur128's prototypes ⇒ any sample rate; 400 ms
blocks / 100 ms hop; −70 LUFS absolute + −10 LU relative gates). It keeps every block's power, so the
gain is solved against the **re-gated** loudness (raising a quiet stem can lift blocks over −70).
Edge cases keep the stems summable: digital silence is skipped (listed in the manifest); audio
entirely under the −70 gate is written at unity, `"normalized": false`. High-crest stems
(percussion) land under the target, limited by the peak ceiling (`limited_by: "peak"`). This is a
static, export-side gain on files: the scored master stays the unlimited, unnormalized mix of
ADR 0021.

**D4 — One render, then a rescale (no second render, no ffmpeg).** A stem's gain needs its
whole-render loudness and peak. Rather than render twice (2× time) or shell out to ffmpeg (an
external runtime dependency), the render writes each source to a **float temp WAV** (lossless, overs
kept, `.sme-stems-tmp/` or `--stem-temp-dir`) while metering it; afterwards a parallel post-pass
reads each temp (`wav::WavReader`), applies the gain in f64, writes the final file through the same
`WavWriter`/`FlacWriter` + quantizer as a stereo render, re-measures it, and deletes the temp. The
cost is temp disk (≈ 1.4 GB per stem-hour at 48 kHz): `--dry-run` and the render's start print the
estimate and free space, and a render whose temps can't fit is refused before it starts. Temp files
open at a stem's first non-zero sample (the leading silence is written then), so a stem that never
sounds costs nothing. Cleanup never deletes a file it can't prove is its own: the temps live in a
fresh, run-owned dir under the temp root (`sme-stems-<pid>`, created exclusively — never an
existing dir — with an owner marker holding the pid), so a `--stem-temp-dir` shared with other
files, even same-named `*.f32.wav` ones, is never touched. A drop guard removes the run's files and
dirs on any exit short of a kill; a later run reclaims a killed run's dir only once its pid is no
longer running, and only our files in it.

**D5 — CLI.** `--stems DIR` (conflicts with `--out`); `--container flac|wav` (stems default to FLAC;
with `--out` it may confirm the extension but never contradict it); `--format` as before (FLAC
s24/s16; WAV f32/s24/s16); `--stem-lufs`, `--stem-temp-dir`. `make song STEMS=<dir>` passes `--stems`
instead of the Makefile's `--out`. Files are `<instrument>.<ext>` and `bus_<id>.<ext>` (sanitized,
de-duplicated case-insensitively for APFS). `DIR/stems.json` records, per stem, raw and output
LUFS/peak, the exact `gain_db`/`gain_linear`, `limited_by`, and `mix_fader_db = −gain_db`: set
every DAW fader to it and the engine's mix is rebuilt. Re-exporting into the same DIR removes the
stems the previous `stems.json` recorded that this export didn't rewrite (a source now silent or
gone, or the other container), so no stale audio sits beside the new manifest; files the previous
manifest didn't record (and any entry that isn't a bare `.wav`/`.flac` name) are left alone.

## Consequences

- Verified on a 10-minute "Idle" render: every stem reads −18.0 LUFS on ffmpeg's `ebur128` (the
  percussive `piston` −28.0, peak-limited at −1.0 dBFS), matching the manifest; Σ stem ×
  10^(mix_fader_db/20) nulls against a separate stereo render to **−136 dBFS** (f32 WAV stems) /
  **−130 dBFS** (s24 FLAC) — which also re-proves cross-run determinism. Render speed is unchanged;
  the post-pass takes seconds per 10 minutes of audio.
- Through `make`, pass `--stem-lufs -14` with a space: make reads any `a=b` word as a variable.
- Rounding `mix_fader_db` to a DAW's 0.01 dB leaves a ≈ −75 dBFS residual — inaudible, and why the
  manifest keeps exact values.
- The demo is `lab/tests/fixtures/song_glass_garden.json5` (1 h, D lydian, 6 instruments + 2 reverb
  buses ⇒ 8 stems).
- Not built: per-instrument wet stems, true-peak (oversampled) ceilings, a loudness target per stem,
  and stems from `sme-play`/the TUI (they keep ADR 0027's live device routing).
