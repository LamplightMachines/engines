# ADR 0031 — Re-scope to a composing instrument; remove the CLAP learner

Status: accepted (P1a built 2026-10-09)
Supersedes: the learner parts of `doc/SRD_v1.1.md` and `doc/SRD_core_engine_v0.1.md` (CLAP-steered
adaptation, the ±δ corpus, the online RLS, `pursue`, the sweep/legibility gate, the offline harness),
and ADRs 0004 (CLAP scorer), 0006 (CLAP providers), 0008 (decoupled scoring thread), 0012 / 0022 (the
pursue gate). Amends 0010 / 0011 (the control plane loses `objective`/`pursue`/`clap_avoid`), 0013 / 0021
(the TUI's term/objective views), 0026 phase D (the DAW return is now monitor-only), 0021's "no master
limiter" rationale. Plan: [`doc/plans/composition-workflow-p0-p9.md`](../plans/composition-workflow-p0-p9.md) (phases P0–P9).

## Context

The project began as a POC for a drone generator that "listens" to its own output through an audio-text
embedding model (LAION CLAP, later MusicCoCa) and adapts toward perceptual goals ("warmer", "less harsh")
via antithetic ±δ pairs, a sample corpus and an online RLS learner. In practice the output the user cares
about is **long-form generative tracks published to YouTube** (`idle-night-v2`: a 5¼-hour sleep track,
finished with extra tracks and mastering in Ableton). The learner hasn't been useful: the measured
legibility was weak (single-loop ΔCLAP ~1e-3, offset-dominated cosines; only `cutoff`/level-like levers
were reliably heard), and the levers the user actually reaches for are **density, brightness, structure
(messy ↔ predictable — including polymeter variance across euclid sequences and free LFOs) and mood
(mostly the scale)** — all controllable directly by the loop doc and the Orrery/Arranger.

## Decision

**D1 — The project is a composing instrument.** Its job: take a piece from a short idea, through a
plain loop doc, live editing (VS Code + a live player, mid-loop apply), to a song rendered as a master
and stems. The SRDs stay as the record of the original design; a banner on each points here.

**D2 — The learner is removed, not shelved.** Deleted: the `Scorer` seam and every scorer (`fake`,
`null`, the ONNX CLAP provider, the Python MusicCoCa backend), `learner.rs` (RLS + per-(path, term)
stats), `feature.rs` (φ), `observe.rs` (captures + DSP features), `sample.rs` / the `Sample` corpus,
`runtime.rs`, `sweep.rs`, the `sme-run` / `sme-sweep` bins, `pursue` and its gate, `objective` weights,
`clap_avoid`, the DAW-return *scoring* path, `lab/clap`, the offline harness, `contracts/providers/`,
the Python lab's sweep. The `onnx` cargo feature and the `ort` dependency are gone. `models/` (1.2 GB of
weights) is no longer used.

**D3 — The played audio is unchanged (proven).** Removing the minus side changes nothing that was
played: the worker always rendered and pushed only the +δ doc. `crates/sme_core/tests/golden.rs` pins
FNV hashes of 7 renders (engine-direct; Session hold, drift, drift + Orrery, stems; a one-loop-per-block
song) — blessed before the removal, **bit-identical after it**. (The removal also drops the per-loop
engine clone, FFT feature pass and doc clones: renders got noticeably faster.)

**D4 — What replaced the learner's observation channels.**
- The **controller** returns a `LoopPick { doc, path, delta, harmonic_move, categorical_move }` from
  `next(loop_id)` — no `minus`, no learner/captures arguments, no corpus `pair_id`.
- The **session** collapses to one worker loop (no scoring thread, `ScoreJob`, `Backpressure`,
  `return_grace`) and emits one **`LoopRecord`** per rendered loop (`telemetry.rs`): loop id, control
  revision, mode, start frame + length, the drift/constraint move, the Orrery transfer, the categorical
  move, the song block/seam, the master peak. `Session::poll_records()`; `sme-song --loop-log <jsonl>`;
  the FFI's `sme_session_poll_records_json`. The skill's `orrery_voyage.py` reads it (and `sme-song`
  accepts `--objectives`, so tamed movement is now measurable headlessly — `sme-run` never could).
- **Telemetry** loses `gate_state` / `objective_score` / `picks`.
- The Orrery's "solar wind" (`preference_from_objective`, keyword valence on objective term names) is
  replaced by the default preference re-anchored by `movement.tension_target` — exactly what it computed
  with an empty objective, so Orrery voyages are unchanged.

**D5 — Kept, with new rationale.**
- **Determinism (CE-R7.3)** stays mandatory, now justified by reproducible renders (a FLAC or a stem set
  must be re-creatable from a doc snapshot + seed) and by the golden tests that guard refactors.
- **Drift** stays as the "organic variation" lever (a bounded random walk with momentum); categorical
  moves keep only their played side. (Its band-edge pinning — a ±δ-era symmetric clamp — is fixed
  separately, P1b / ADR 0032.)
- **No master limiter in renders** stays: renders feed stems and a DAW, where dynamics are the mastering
  engineer's call. A live-only protection limiter in the player shell is allowed (it never touches a
  render). The old rationale (a limiter would colour the scored audio) no longer applies.
- MIDI drift (`DitherConfig::include_midi`) stays off by default — now simply because an external synth's
  patch is the author's, not because ΔCLAP is structurally zero there.
- The core stays portable and I/O-free; the C ABI and the Swift shell are kept compiling with the learner
  stripped (`sme_session_new` lost its scorer argument — a null scorer used to spawn MusicCoCa), without
  new investment.

**D6 — Objectives docs.** `SessionControl` keeps `revision`, `optimize` (`hold | drift | live_edit`),
`movement`, `constraints`, `overrides`, `guardrails` (`max_change_per_loop`, ceilings, floors). Unknown
fields are ignored, so a pre-0031 doc carrying `objective` / `clap_avoid` still loads; `optimize:
"pursue"` is an error that names this ADR. (P2 moves movement settings into the loop doc itself.)

## Consequences

- Test counts drop by the deleted learner tests (scorers, RLS, φ, sweep, harness, pursue, return-scoring);
  the kept ones were ported to `LoopRecord`s / audio comparisons. The TUI keeps compiling and passing,
  minus its Terms tab and objective editing.
- The SRD's "feature schema and steerable-term list are outputs of the POC" commitment and the
  "params are learned over a feature vector" commitment no longer apply.
- Nothing measures perceptual targets any more. If an audio-embedding model is wanted again, it would come
  back as an offline analysis tool over renders, not a controller in the loop.
