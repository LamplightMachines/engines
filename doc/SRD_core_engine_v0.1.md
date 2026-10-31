# Core Engine + Controller — SRD v0.1

> **Scope note (2026-10-09, [ADR 0031](decisions/0031-composition-rescope-and-learner-removal.md)):** the project was re-scoped to a composing instrument for long-form generative tracks, and the CLAP learner (perceptual scoring, the ±δ corpus, the online RLS, `pursue`, the sweep/legibility gate, the offline harness) was **removed**. This SRD remains the record of the original design; the engine, songs, stems, determinism and the drift/Orrery controller still follow it. Where it describes the learner, ADR 0031 wins.

> **Units note (2026-10-27, [ADR 0054](decisions/0054-native-units-schema-v0-9.md)):** the loop doc is schema **v0.9**: a param is written in its unit under a suffixed key (`cutoff_hz`, `attack_s`, `level_db`, with `-Infinity` for silence), and a route's depth in its dest's unit (`depth_oct`, `depth_db`, …). The engine still drifts in a normalized `0..1` through the seam's curves (ADR 0005). Where this SRD writes a param as `0..1` or uses a v0.7/v0.8 key, that is the original design; ADR 0054 and `doc/loop_contract_v0.9.json5` win.

> **Status:** accepted (v0.1). Companion/subordinate to `doc/SRD_v1.1.md`
> (the whole-system SRD) and `doc/loop_contract_v0.9.json5` (the loop schema).
> Where this conflicts with the system SRD, flag it; this document **narrows and
> sharpens** the system SRD for the buildable core. **Stack: a Rust I/O-free core +
> a Swift shell, macOS-first (port to iOS later)** — replaces the C++/JUCE approach of
> ADR `0001` (see ADR `0003`). The core requirements remain stack-neutral; the shell
> sections (§3.2, §12) are Swift/AVAudioEngine/CoreML specific.
>
> Folds in the M2 (C++) code-review findings as hard requirements so the rewrite does
> not repeat M2's gaps (priority stealing, portable hashing, zipper-free automation,
> unimplemented transport humanization).
>
> **v0.1 scope:** the core builds the **full online loop** on macOS first — live
> streaming + antithetic dither + **runtime CLAP scoring** + **online RLS learning** +
> complete sample export. Samples are complete on emission (no offline scoring pass
> required).
>
> **v0.7 loop schema + JSONPath canonical names (#2B):** `modulators`, `matrix`, and
> per-step `voicing` are **id-keyed objects** (like `instruments`); `steps` and
> `pitch_walk.pool` stay arrays. Canonical param names are **full JSONPath** matching the
> doc — `instruments.poly.params.voices_detune_cents`, `matrix.m_breath_c.depth_oct`,
> `modulators.chords.steps[0].voicing.v3.cents_offset`. Object members iterate in
> **sorted-id order** (deterministic). The effective-state capture is the **whole nested
> v0.7 doc with effective values** (all properties), JSONL now, Parquet later — the
> learner selects/transforms at ingest. Fixture: `lab/tests/fixtures/loop_contract_v0.9.json`.

---

## 0. What this document covers
The **core engine** (synthesis + loop sequencer + modulators + matrix) plus the
**controller / online learner** (per-loop param evolution, runtime observation incl.
CLAP, online RLS, and sample export), as a single portable, deterministic, **I/O-free**
unit driven by a thin platform shell. It does **not** cover the *offline* exploration
lab (grouping discovery, coverage audit, counterfactual-replay tooling, active
sampling), the sweep rig, or the CLAP/fake-CLAP model implementations — those are shell
/ Python-lab concerns (§13). The CLAP model is **injected behind an interface**.

Deliverable: a core that, **given a starter loop document, streams a continuous
evolving drone live, perturbs base params as antithetic ±δ pairs, scores each loop with
CLAP in-process, updates an online RLS model, and emits a self-contained, complete
per-loop sample** — usable to learn online and to train/re-derive offline.

---

## 1. Purpose & scope
- **CE-R1.1** Renders a looped **control** timeline; synthesis runs continuously (no
  audio-path seam) — system SRD §3 / rationale §1.1.
- **CE-R1.2** Headless and **I/O-free**: no audio devices, no file I/O, no wall-clock,
  **no linked ML model**. Device I/O, persistence, scheduling, and the CLAP/fake-CLAP
  scorer implementation live in the **shell** and are passed in behind interfaces (§3).
- **CE-R1.3** **Real-time streaming required** (live monitoring is a must);
  faster-than-real-time not required (POC v1).
- **CE-R1.4** One core serves three front-ends over time: macOS CLI shell (now), the
  Python lab (offline, file/CLI boundary), iOS app shell (later). No front-end
  assumptions leak into the core.

## 2. Definitions (aligned)
| Term | Meaning in this SRD |
|---|---|
| **Base param** | An authored loop-doc value (instrument/global/matrix). The **learnable unit** and the thing a dither **δ** is applied to. |
| **Operating point** | The full state perturbed *from*: all **base** param values + harmony/transport + the **phase snapshot** (modulator phases, active voices). |
| **Effective (feature) state** | The **realized, post-modulation sounding values** at a captured moment: instrument params after modulation/glide, modulator outputs/phases, globals. *Not* base values. |
| **DSP features** | Acoustic measures of rendered audio (RMS, centroid, flatness, flux, crest, stereo-corr, roughness). |
| **Capture** | One sampling of effective state + DSP features + CLAP at a capture instant (~every 4 steps; 4 per 16-step loop). |
| **Sample** | One per-loop training record aggregating the loop's captures: operating point + δ + effective-state context + DSP features + **complete CLAP outcome + baseline-corrected ΔCLAP** + provenance. |
| **Antithetic pair** | Two renders from the *same* operating point, +δ and −δ; their settled-CLAP difference is the drift-cancelled ΔCLAP (non-deferrable, system SRD §5.3.1). |
| **Control Objective** | Steering intent (system SRD §6.2): `mode` (drift/hold/pursue), signed weights on CLAP terms, guardrails (ceilings/floors, `max_change_per_loop`, `clap_avoid`). Optional core input. |
| **Working feature vector** | The **versioned, provisional** feature encoding the online RLS generalizes over (role, parent constraints, dest/source kind, effective context). Revisable; raw storage (CE-R10.3) keeps the corpus re-derivable when it changes. |
| **Scorer** | Injected interface returning CLAP embedding + per-term scores for an audio window. Implementations: real CLAP (CoreML/ONNX), `fake_clap` (synthetic), null. |

## 3. Architecture & boundaries
- **CE-R3.1 Plain-data core↔shell contract** (rationale §1.4). Inputs: loop doc
  (v0.6 JSON) + phase snapshot + optional Control Objective + injected Scorer. Outputs:
  audio blocks, next phase snapshot, per-capture observation rows, complete per-loop
  samples, and (if pursue) the next loop doc.
- **CE-R3.2 Block-rate render API.** Core fills caller buffers a block at a time; the
  shell's audio render callback (AVAudioEngine, same on macOS/iOS) owns the device and
  pulls. Core never blocks on I/O.
- **CE-R3.3 Threading.** Audio generation is real-time-safe on the audio thread (no
  alloc/locks/file/CLAP/RLS work). Controller, **CLAP scoring, RLS update, sample
  assembly, persistence** run on a **worker thread**; audio handoff is **lock-free**;
  next-loop control state swaps in **atomically** at the boundary; if not ready, the
  current loop's control **repeats once** (safe non-event) — system SRD R3.9.3/R3.10.4.
- **CE-R3.4 Runtime CLAP via injected Scorer.** CLAP scoring happens **in-process
  during the run** so each emitted sample is **complete (no offline pass required)** —
  but the model is **never linked into the core**; it is provided by the shell behind
  the Scorer interface (CoreML/ONNX on device; the lab may also re-score for
  validation). The same interface admits `fake_clap` for independent learner validation
  (system SRD §8.3) and a null scorer for engine-only runs.
- **CE-R3.5 Per-loop worker budget.** Per loop the worker performs **two CLAP
  inferences** (live +δ and re-rendered −δ) + feature aggregation + one RLS update +
  next-loop param selection, all within the loop period (~15 s @ 65 bpm). This budget
  is a hard real-time constraint on the integrated loop.
- **CE-R3.6 Binding (decided stack).** Core ships as a Rust `staticlib`/`cdylib` with a
  C ABI generated by `cbindgen`; the Swift shell links it and calls across that ABI.
  Real-time-safe types cross the boundary as plain buffers/POD; no Rust panics escape
  (errors are returned codes). The same Rust core recompiles unchanged for
  `aarch64-apple-ios`; only the shell changes for the iOS app.

## 4. Synthesis engine
- **CE-R4.1** Four instrument slots: `mono`, `poly`, `rhythmic` (`pitched:false`,
  filtered-noise), `event`. Base params + note engine only; **runtime-extensible**
  (new slots → new canonical paths) without contract recompilation.
- **CE-R4.2** Honor `enabled` (disabled ⇒ silent). *(M2 gap.)*
- **CE-R4.3 Voice object model:** a voice owns its oscillator, per-voice filter,
  envelope. Waveforms ≥ `sine`, `triangle`, `saw_soft`, `sine_bell`, `filtered_noise`.
- **CE-R4.4 Primary CLAP levers, high fidelity:** low-pass filter incl. resonance,
  saturation/drive, reverb/space, detune/beating. Per-voice filter + shared reverb
  state **persist across loop boundaries** (tails cross the seam).
- **CE-R4.5 Zipper-free automation (hard).** Every modulated destination is
  **smoothed per-sample**; no discrete jumps at control-event boundaries. Param→audio
  response **monotonic and smooth**, no dead zones — the learner must not confuse an
  artifact for an acoustic effect. *(M2 gap: stair-stepped control updates.)*
- **CE-R4.6 Global section:** `master_gain`, `stereo_width` (mid-side), `reverb_mix`
  (+ reverb size/character), automatable and learnable. *(Schema v0.9: `master_gain_db`; `reverb_mix`
  is gone — the reverb is a send bus, ADR 0054.)*

## 5. Loop sequencer, modulators & matrix
- **CE-R5.1 Loop doc v0.6** parsed/rendered exactly (§3.5–3.9 semantics). Transport:
  `bpm`, `master_steps`, `step_unit`(`beat`), `swing`, `timing_jitter_ms`.
- **CE-R5.2 Transport humanization applied** — `swing`, `timing_jitter_ms`,
  modulator `humanize_ms` affect event placement, deterministically seeded (§7).
  *(M2 gap: parsed, never applied.)*
- **CE-R5.3 Modulators** `lfo` / `step_seq` (explicit `steps` XOR `generator`
  euclid). Domains `control` / `note`. `sync: loop|free` (free phase engine-owned).
  `step_duration` fraction of master; `length×step_duration ≠ 1` ⇒ polymetric drift.
  `entry_value` C0 continuity. `pitch_walk` order `up|down|random` all implemented
  *(M2 gap: only `up`)*.
- **CE-R5.4 Matrix `mod`:** `value = clamp01(base + Σ depth·source·polarity)`,
  `mode:"add"`, `depth`, `depth_slew_ms` (ramp; omit ⇒ instant).
- **CE-R5.5 Matrix `note` fan-in & stealing (hard).** Allocate by `priority`
  (lower=higher), each up to its `voices` budget; fan-out up to `polyphony` in declared
  id order. **Steal lowest-priority-source-first, then oldest-within-source; high
  priority never stolen.** *(M2 gap: priority ignored; plain truncation.)*
- **CE-R5.6** Modulator→modulator references disallowed.

## 6. Note / voice transition semantics (by `id`)
- **CE-R6.1** Both consecutive active steps ⇒ **glide** over `glide_ms` (same pitch ⇒
  held); no distance cap. **CE-R6.2** Absent ⇒ **sustain**. **CE-R6.3** `release:true`
  ⇒ stop (note-off + tail). **CE-R6.4** New id ⇒ fresh articulation. **CE-R6.5** Failed
  `prob` ⇒ absent ⇒ sustains (never a dropout).

## 7. Determinism & reproducibility
- **CE-R7.1** Root seed = `loop_id`; each probabilistic site uses a **fixed,
  documented, portable hash** of `(loop_id, source_id, step, voice_id)`.
  **Language/stdlib hashes forbidden** (byte-identical across macOS/iOS and any
  re-scorer). *(M2 gap: `std::hash`.)*
- **CE-R7.2** Free-running modulator phase captured in the **phase snapshot**.
- **CE-R7.3** Given (loop doc + phase snapshot), renders are **bit-reproducible** —
  the basis for the antithetic baseline, the −δ counterfactual re-render, and offline
  re-score/validation. *Amended by ADR 0054 (P10):* a render is re-creatable from its
  doc snapshot **with the engine build that rendered it** (the sidecar's `engine_build`);
  a later schema or engine may not reproduce it, which is why the last v0.8 build is tagged
  `schema-0.8-final`.

## 8. Controller, Control Objective & Online Learner
- **CE-R8.1 Modes** (system SRD §5.1): `drift` (bounded random dither — default, and
  the primary sample source), `hold` (no steering; music still breathes), `pursue`
  (query the **online RLS** for high-effect deltas toward the objective's CLAP
  direction). Empty/absent objective ⇒ `drift`. **All three are in scope for v0.1.**
- **CE-R8.2 Control Objective input** (system SRD §6.2): optional, standing/complete;
  `mode`, signed `clap_objective[]` weights (sign=direction, magnitude=priority),
  `guardrails` (ceilings/floors, `max_change_per_loop`, `clap_avoid`). **Supplied via
  CLI flags** (CE-R12.2). Edits replace + increment `objective_id`; **re-base on change**
  (move from current state, never jump).
- **CE-R8.3 Per-loop evolution.** From operating point `Pₙ`, select a perturbation of
  magnitude **δ** bounded by `max_change_per_loop`; produce `Pₙ₊₁` by **re-basing from
  the actually-played state**. **v0 default: single-direction dither** (one base param
  per loop) for clean attribution and simplest antithetic pairs; **SPSA multi-direction
  is an opt-in policy** added once the loop is validated (system SRD R5.3.2).
- **CE-R8.4 Antithetic pairing (non-deferrable).** Realize each perturbation as a
  **±δ pair from the same `Pₙ`** (identical starting snapshot). **Locked decision: the
  `+δ` side is played live; the `−δ` side is re-rendered headless by the worker for
  scoring only (its end-state discarded).** Both are valid partners by determinism (§7).
  ΔCLAP = settled-CLAP(+δ) − settled-CLAP(−δ), drift-cancelled.
- **CE-R8.5 Online learner (RLS).** Recursive/streaming Bayesian-linear / RLS modeling
  **baseline-corrected ΔCLAP as a function of base-param δ over the working feature
  vector**; **forgetting factor** (non-stationarity); **regularized toward "no effect"**
  (unseen params inert until evidence); **partial pooling** (type-level prior early,
  instance-specific samples weighted up as they accumulate — **default: precision-
  weighted shrinkage**, instance weight = instance posterior precision ÷ (instance +
  prior precision), derivable from the posterior, not hand-tuned). Generalizes across
  instances with similar feature vectors (e.g. `lfo→poly.cutoff depth` informs
  `lfo→mono.cutoff depth`). Update runs on the worker thread (CE-R3.5).
- **CE-R8.6 Working feature vector v0 (versioned, provisional).** The RLS generalizes
  over a **fixed-width** feature vector φ computed **per perturbed base param**, tagged
  `feature_schema_version: "0.1"`. The *final* schema is a POC output (system SRD §7.5);
  v0 is the starting encoding. φ is **derived at learn-time from the raw sample** (path
  metadata + `effective_state` + `dsp_features`), **never stored as the canonical
  record** — so revising the encoding needs no re-render (CE-R10.3). v0 fields:
  - **(a) Identity / role (categorical, encoded):** `param_role` ∈ {cutoff, drive,
    level, detune, decay, band_lo, band_hi, reverb_mix, stereo_width, master_gain,
    matrix_depth, lfo_rate, …}; `param_domain` ∈ {instrument, global, matrix,
    modulator}; `instrument_slot` ∈ {mono, poly, rhythmic, event, none}; `source_kind`
    ∈ {lfo, step_seq, none}; `dest_kind` ∈ {filter, level, width, pitch, reverb, none};
    `pitched` (bool).
  - **(b) Operating-point context (continuous, normalized, from `effective_state`):**
    parent effective cutoff / level / drive / detune; register (mean active pitch),
    active-voice fill (count ÷ polyphony); effective globals (reverb_mix, stereo_width,
    master_gain); `mode` (categorical) and normalized `bpm`. (Conditioning here is what
    turns interactions — "drive matters more when cutoff is low" — into smooth feature
    dependence rather than discrete buckets; rationale §4.2.)
  - **(c) Acoustic context (continuous, from averaged `dsp_features`):** centroid,
    flatness, RMS, roughness, crest, stereo-corr.
  - **(d) Perturbation:** signed `delta`; SPSA sign-vector membership if multi-direction.
  - **Encoding:** categoricals one-hot/embedding; continuous features normalized to
    comparable scales; **fixed width regardless of param/instrument count** — new params
    describe into the same space at high uncertainty (the dimensionality firewall,
    system SRD R5.2.7). Model: ΔCLAP per term via RLS over φ scaled by `delta`,
    interaction terms `dᵢ·dⱼ` optional.
  - Raw storage (CE-R10.3) guarantees a schema revision never invalidates the corpus
    and the lab can retrain offline.
- **CE-R8.7 Guardrails & safety.** `max_change_per_loop` bounds every dither/step;
  ceilings/floors honored; `clap_avoid` is an always-on hard reflex independent of
  weights. **Default `clap_avoid`:** a per-term hard threshold; when a settled
  avoid-term score crosses it, a predefined conservative correction fires (back off the
  params most associated with that term per the current RLS) and pursuit is suppressed
  until it clears (trigger/correction tunable). Learner/scorer failure ⇒ degrade to
  **no worse than `drift`**.
- **CE-R8.8 Provenance emission.** Every sample records perturbation policy, δ(s) by
  canonical path, operating-point ref, seed, working-feature-schema version, objective
  id (if any), engine/scorer versions.

## 9. Observation & capture (runtime, always-on)
- **CE-R9.1 Cadence:** every ~4 steps ⇒ **4 captures per 16-step loop**.
- **CE-R9.2 Per-capture content:** (a) **effective param state** (post-modulation
  params, modulator outputs/phases, globals — surfaced by the renderer *at the capture
  instant*, a first-class output, not just end-of-loop); (b) **DSP features**; (c)
  **CLAP** embedding + per-term scores from the Scorer over a **broad term battery**
  (so steerable terms are chosen post-hoc; v0 ships a broad starter term list in
  `contracts/clap_terms_v0.txt` — warm/harsh/bright/dark/smooth/rough/dense/sparse/airy/
  muddy/lush/thin/metallic/… — with the full embedding stored so the steerable shortlist
  stays a post-hoc POC output) — computed per-step finely enough to (d) exclude transients.
- **CE-R9.3 Settling.** Per-step CLAP/feature trajectory locates the settled tail;
  attack/glide transients excluded from attributed values.
- **CE-R9.4 Aggregation.** Effective state + DSP features **averaged over the 4
  captures**; CLAP outcome from **settled** captures ⇒ **one sample per loop** per pair
  side; ΔCLAP joined from the pair (CE-R8.4).

## 10. Sample & corpus contract (self-contained, complete on emission)
- **CE-R10.1** Append-only, **schema-versioned**. **Configurable writer with one
  logical schema across both formats: JSONL for development/debugging, Parquet/Arrow for
  data-collection runs** (`--format jsonl|parquet`). **Identical record content online
  (on-device) and offline**; Parquet reads straight into pandas/Polars for the lab.
- **CE-R10.2 Per-loop sample record**, distinct sections (machine schema:
  `contracts/sample_v0.2.schema.json`; Rust types: Appendix B):
  - `meta`: `schema_version`, `engine_version`, `scorer_version`, `loop_id`(=seed),
    `prev_loop_id`, `timestamp`, `antithetic_pair_id`, `pair_sign`, `played_live`.
  - `operating_point`: **base** params before perturbation (canonical paths) + harmony
    + transport.
  - `perturbation`: `[{canonical_path, delta}]`, `policy`, `max_change_per_loop`,
    `objective_id?`.
  - `effective_state`: the **whole nested v0.7 loop-doc mirror with effective values**
    (all properties) at each of the 4 capture instants — instrument/global params
    post-modulation, `matrix.<id>.depth` post-slew, modulator defs authored. Captured
    per-instant whole (not pre-averaged); the learner selects/averages/transforms at
    ingest. JSONL now; Parquet later.
  - `dsp_features`: averaged measured features.
  - `clap_outcome`: **complete** — per-term scores (broad battery) + full embedding
    from settled captures; `delta_clap` per term = baseline-corrected ΔCLAP from the
    pair.
  - `phase_snapshot`: full operating-point snapshot (replay / counterfactual / re-score).
  - `provenance`: per CE-R8.8.
- **CE-R10.3 Re-derivability (acceptance test).** Raw effective state, DSP features,
  embedding, seed, and snapshot stored ⇒ a **revised feature schema is re-evaluable
  against all past samples with zero re-render** (system SRD R6.3.2). Never store only
  an abstracted form.

## 11. Streaming behavior
- **CE-R11.1** Start from a **starter loop doc**; continuously produce successive
  loops, each differing from the prior operating point by the controller's bounded
  perturbation, streaming live audio seamlessly.
- **CE-R11.2** Loop boundary: assemble + persist the prior loop's sample(s), update
  RLS, swap next control state atomically, continue synthesis uninterrupted.
- **CE-R11.3** Run parameterized: duration/loop count, mode + objective, dither policy,
  δ, max-change, cadence, output path, scorer selection (real/fake/null). Terminates
  cleanly, flushing pending samples.

## 12. Shell / CLI (macOS now; iOS-portable)
- **CE-R12.1** Thin **Swift CLI**: loads starter doc; opens device via **AVAudioEngine**
  (same on macOS/iOS); pulls audio blocks; runs worker (controller/capture/CLAP/RLS);
  writes corpus; **hosts the real CLAP Scorer (CoreML/ONNX)** and can swap in
  `fake_clap`/null.
- **CE-R12.2** Flags map to CE-R11.3 **and supply the Control Objective** (CE-R8.2),
  e.g. `--mode pursue|drift|hold`, `--objective "clap:warm=+1.0,clap:harsh=-0.8"`,
  `--max-change 0.05`, `--ceiling instruments.poly.params.cutoff_hz=3175`,
  `--floor instruments.mono.params.level_db=-20` (v0.9 keys and units, ADR 0054),
  `--clap-avoid "clap:harsh=0.7"`, `--scorer real|fake|null`, `--out corpus.parquet`.
  Output human-readable + machine-parseable (JSON).
- **CE-R12.3** Shell holds **all** platform specifics (device, files, model runtime);
  swapping it for the iOS app shell requires no core changes.

## 13. Non-goals (this core SRD)
The **offline exploration harness** (grouping discovery, coverage/confounding audit,
counterfactual-replay tooling, active sampling) and the **sweep rig** — Python lab; the
**CLAP and fake_clap model implementations** — injected via the Scorer interface, not
built here; on-device CLAP quantization/optimization; LLM intent; mobile UI;
modulator→modulator refs; automatable connection depth. *(The core **does** host
runtime CLAP scoring via the interface and the **online** RLS; the **offline** learning
lab and harness analyses remain separate and consume the emitted corpus.)*

## 14. Resolved decisions & remaining empirical tuning
**Resolved** (defaults specified in the requirements): working feature vector v0 →
CE-R8.6; Control Objective via CLI flags → CE-R8.2/R12.2; antithetic side (+δ live /
−δ re-rendered) → CE-R8.4; controller scope incl. online RLS → §8; runtime CLAP via
Scorer → CE-R3.4; audio windows not persisted → §13; dither breadth (single v0, SPSA
opt-in) → CE-R8.3; corpus format (JSONL debug / Parquet runs) → CE-R10.1; `clap_avoid`
default → CE-R8.7; term battery starter list → CE-R9.2; partial-pooling precision
weighting → CE-R8.5.

**Remaining = empirical / tuned during implementation, not blocking** (defaults are
specified; only exact values are TBD): the steerable-term shortlist (a POC output of the
sweep rig), specific `clap_avoid` thresholds + corrections, RLS forgetting-factor /
regularization constants, and the concrete starter term list contents.

---

## Appendix A — Canonical Param Registry (instantiated from `loop_contract_v0.9.json5`)

> **Schema v0.9 (ADR 0054):** the rows below carry the v0.9 keys and native ranges. A param is
> written in its unit; the controller dithers it in a normalized `0..1` through the seam's
> curves (log for Hz, dB for gains, linear for times). `global.reverb_mix` is gone: a doc
> without buses plays dry.
Convention: **full JSONPath (v0.7)** matching the loop doc — instruments/modulators/
matrix/voicing are id-keyed objects (`steps`/`pool` stay arrays), iterated in sorted-id
order. **Ctrl** = the controller may dither/learn it online (✓ = continuous learnable
knob; ✗ = structural/categorical/integer — authorable but not dithered). Every ✗ value
is still captured in `operating_point`/`effective_state` as **context** (and feeds φ
where relevant). Registry is **runtime-extensible** (CE-R4.1) — this is the v0.7
example's instantiation, not a closed set. `param_role` is the φ identity tag (CE-R8.6a).

**Transport / Harmony**
| Canonical path | param_role | type / range | Ctrl | notes |
|---|---|---|:--:|---|
| `transport.bpm` | tempo | float bpm | ✗ | structural; normalized φ context |
| `transport.master_steps` | loop_len | int | ✗ | structural |
| `transport.step_unit` | step_unit | enum(`beat`) | ✗ | structural |
| `transport.swing` | swing | float 0..1 | ✓ | feel/humanization (low priority) |
| `transport.timing_jitter_ms` | timing_jitter | float ms | ✓ | global humanization |
| `harmony.root` | root | enum(note) | ✗ | transpose; excluded from φ (transpose-invariant) |
| `harmony.mode` | mode | enum(scale) | ✗ | categorical φ context |

**Global**
| Canonical path | param_role | type / range | Ctrl | notes |
|---|---|---|:--:|---|
| `global.master_gain_db` | master_gain | float dB −60..+6 | ✓ | `-Infinity` = silent |
| `global.stereo_width` | stereo_width | float 0..1 | ✓ | mid-side width |

**Instruments** — common to each slot `S ∈ {mono, poly, rhythmic, event}`:
`instruments.S.enabled` (bool ✗), `instruments.S.pitched` (bool ✗, φ context), `instruments.S.note_engine.polyphony` (int ✗).
| Canonical path | param_role | type / range | Ctrl | notes |
|---|---|---|:--:|---|
| `instruments.mono.note_engine.glide_ms` | glide | float ms | ✓ | |
| `instruments.mono.params.wave` | wave | enum(`triangle`…) | ✗ | categorical |
| `instruments.mono.params.drive_db` | drive | float dB 0..20 | ✓ | saturation lever (pre-gain) |
| `instruments.mono.params.level_db` | level | float dB −60..+6 | ✓ | `-Infinity` = silent |
| `instruments.mono.params.cutoff_hz` | cutoff | float Hz 80..8000 | ✓ | LP filter lever (log) |
| `instruments.poly.note_engine.glide_ms` | glide | float ms | ✓ | |
| `instruments.poly.params.wave` | wave | enum(`saw_soft`…) | ✗ | categorical |
| `instruments.poly.params.voices_detune_cents` | detune | float cents 0..100 | ✓ | detune/beating lever |
| `instruments.poly.params.level_db` | level | float dB −60..+6 | ✓ | |
| `instruments.poly.params.cutoff_hz` | cutoff | float Hz 80..8000 | ✓ | LP filter lever (log) |
| `instruments.rhythmic.params.source` | source | enum(`filtered_noise`) | ✗ | categorical |
| `instruments.rhythmic.params.band_lo_hz` | band_lo | float Hz 20..4000 (ADR 0046 amendment) | ✓ | bandpass low edge (log) |
| `instruments.rhythmic.params.band_hi_hz` | band_hi | float Hz 20..4000 (ADR 0046 amendment) | ✓ | bandpass high edge (log) |
| `instruments.rhythmic.params.level_db` | level | float dB −60..+6 | ✓ | |
| `instruments.event.note_engine.glide_ms` | glide | float ms | ✓ | (0 in example) |
| `instruments.event.params.wave` | wave | enum(`sine_bell`…) | ✗ | categorical |
| `instruments.event.params.decay_s` | decay | float s 0..4 | ✓ | the bell's ring |
| `instruments.event.params.level_db` | level | float dB −60..+6 | ✓ | |

**Modulators** — common: `modulators.<id>.{output,type,sync,length,step_duration}` all ✗
(structural). Variable/learnable per id:
| Canonical path | param_role | type / range | Ctrl | feature tags / notes |
|---|---|---|:--:|---|
| `modulators.lfo_breath.wave` | lfo_wave | enum | ✗ | source_kind=lfo |
| `modulators.lfo_breath.polarity` | polarity | enum | ✗ | |
| `modulators.lfo_breath.rate_hz` | lfo_rate | float Hz | ✓ | source_kind=lfo |
| `modulators.seq_swell.interp` | interp | enum | ✗ | |
| `modulators.seq_swell.entry_value` | entry_value | float 0..1 | ✗ | engine-derived C0 continuity |
| `modulators.seq_swell.steps[i].value` | ctrl_step | float 0..1 | ✓ | automation-shape point (i∈{0,8,15}) |
| `modulators.chords.voicing.<vid>.offset` | pitch | int semis | ✗ | harmonic content |
| `modulators.chords.voicing.<vid>.cents_offset` | cents | float cents | ✓ | beating lever (v3,v4) |
| `modulators.chords.voicing.<vid>.prob` | prob | float 0..1 | ✓ | density (v4=0.6) |
| `modulators.bass_line.voicing.v1.offset` | pitch | int semis | ✗ | |
| `modulators.bass_line.voicing.v1.prob` | prob | float 0..1 | ✓ | step-8 = 0.6 |
| `modulators.arp_shimmer.humanize_ms` | humanize | float ms | ✓ | |
| `modulators.arp_shimmer.generator.pulses` | euclid_pulses | int | ✗ | structural rhythm (17) |
| `modulators.arp_shimmer.generator.rotation` | euclid_rotation | int | ✗ | (0) |
| `modulators.arp_shimmer.pitch_walk.pool` | pitch_pool | int[] | ✗ | provisional (rationale §2.8) |
| `modulators.arp_shimmer.pitch_walk.order` | walk_order | enum | ✗ | |
| `modulators.rhy_noise.generator.pulses` | euclid_pulses | int | ✗ | (9) |
| `modulators.rhy_noise.generator.rotation` | euclid_rotation | int | ✗ | (4) |
| `modulators.spark_events.voicing.e.offset` | pitch | int semis | ✗ | |
| `modulators.spark_events.voicing.e.prob` | prob | float 0..1 | ✓ | sparse-event density (0.03–0.06) |

**Matrix** — `mod`-kind has a learnable depth, written in its dest's unit under one key
(`depth` for a plain `0..1` dest, `depth_oct`, `depth_db`, `depth_s`, …; ADR 0054), plus
`depth_slew_ms`; `note`-kind routing is structural. `source_kind`/`dest_kind` are φ tags.
| Canonical path | param_role | type / range | Ctrl | feature tags / notes |
|---|---|---|:--:|---|
| `matrix.m_breath_w.depth` | matrix_depth | float ±1 | ✓ | lfo → width (a `0..1` dest) |
| `matrix.m_breath_w.depth_slew_ms` | depth_slew | float ms | ✓ | secondary |
| `matrix.m_breath_c.depth_oct` | matrix_depth | float oct ±6.64 | ✓ | lfo → filter |
| `matrix.m_breath_c.depth_slew_ms` | depth_slew | float ms | ✓ | secondary |
| `matrix.m_swell.depth_db` | matrix_depth | float dB ±66 | ✓ | step_seq → level |
| `matrix.m_{chords,arp,bass,rhy,spark}.voices` | voices_budget | int | ✗ | routing |
| `matrix.m_{chords,arp,bass,rhy,spark}.priority` | priority | int | ✗ | routing (lower=higher) |

---

## Appendix B — Rust contract types
Illustrative; mirrored by JSON schemas in `contracts/`. `BTreeMap` is used deliberately
(deterministic key ordering ⇒ reproducible serialization, CE-R7.3). `f32` audio-domain
throughout. Canonical paths (flat keys, e.g. `perturbation.deltas[].path`) are JSONPath
per Appendix A.

> **#2B update:** the capture's `effective_state` is now the **nested v0.7 loop-doc
> mirror** (`serde_json::Value`, all properties with effective values), not the flat
> `EffectiveState` struct below. The `Sample` embeds the loop's 4 nested captures; its
> final shape is settled in the controller/sample step (build-order 3). The flat
> `EffectiveState`/`OperatingPoint` types below predate #2B and will be revised there.

```rust
pub type CanonicalPath = String;   // e.g. "poly.params.cutoff_hz" (schema v0.9)
pub type TermId        = String;   // e.g. "clap:warm"

// ── Per-loop Sample record (CE-R10.2) ───────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub meta:            SampleMeta,
    pub operating_point: OperatingPoint,           // base params + δ axis context
    pub perturbation:    Perturbation,
    pub effective_state: EffectiveState,           // post-modulation, averaged
    pub dsp_features:    BTreeMap<String, f32>,    // averaged measures
    pub clap_outcome:    ClapOutcome,
    pub phase_snapshot:  PhaseSnapshot,            // engine type; exact replay
    pub provenance:      Provenance,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SampleMeta {
    pub schema_version:    String,    // "0.1"
    pub engine_version:    String,
    pub scorer_version:    String,
    pub loop_id:           u64,       // == RNG seed
    pub prev_loop_id:      u64,
    pub timestamp_unix_ns: i64,
    pub antithetic_pair_id: u64,
    pub pair_sign:         PairSign,
    pub played_live:       bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairSign { Plus, Minus }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperatingPoint {
    pub base_params: BTreeMap<CanonicalPath, f32>, // authored values, pre-perturbation
    pub harmony:     Harmony,
    pub transport:   Transport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Perturbation {
    pub deltas:              Vec<ParamDelta>,
    pub policy:              String,    // "single" | "spsa"
    pub max_change_per_loop: f32,
    pub objective_id:        Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParamDelta { pub path: CanonicalPath, pub delta: f32 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveState {
    pub params:            BTreeMap<CanonicalPath, f32>, // realized, post-modulation
    pub modulator_outputs: BTreeMap<String, f32>,        // avg output per modulator id
    pub globals:           BTreeMap<String, f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClapOutcome {
    pub scores:     BTreeMap<TermId, f32>,         // raw per-term cosine sim (settled)
    pub embedding:  Vec<f32>,                       // full audio embedding
    pub delta_clap: BTreeMap<TermId, Option<f32>>, // baseline-corrected; None until pair joined
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provenance {
    pub feature_schema_version: String,  // "0.1"
    pub perturbation_policy:    String,
    pub seed:                   u64,
    pub objective_id:           Option<u64>,
}

// ── Control Objective input (CE-R8.2) ───────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlObjective {
    pub objective_id:   u64,
    pub mode:           ControllerMode,
    pub clap_objective: Vec<TermWeight>,   // signed weights (sign=dir, mag=priority)
    pub guardrails:     Guardrails,
    pub meta_notes:     Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerMode { Drift, Hold, Pursue }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TermWeight { pub term: TermId, pub weight: f32 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Guardrails {
    pub max_change_per_loop: f32,
    pub ceilings:   BTreeMap<CanonicalPath, f32>,
    pub floors:     BTreeMap<CanonicalPath, f32>,
    pub clap_avoid: Vec<ClapAvoid>,        // always-on hard reflex
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClapAvoid { pub term: TermId, pub threshold: f32 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Harmony   { pub root: String, pub mode: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transport {
    pub bpm: f32, pub master_steps: u32, pub step_unit: String,
    pub swing: f32, pub timing_jitter_ms: f32,
}
```

---

*Companion documents: `doc/SRD_v1.1.md` (system requirements), `doc/decisions/0003-rust-core-swift-shell.md`
(stack ADR), `doc/loop_contract_v0.9.json5` (loop schema), `contracts/` (machine schemas).*
