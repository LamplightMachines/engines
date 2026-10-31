> **Scope note (2026-10-09, [ADR 0031](decisions/0031-composition-rescope-and-learner-removal.md)):** the project was re-scoped to a composing instrument for long-form generative tracks, and the CLAP learner (perceptual scoring, the ±δ corpus, the online RLS, `pursue`, the sweep/legibility gate, the offline harness) was **removed**. This SRD remains the record of the original design; the engine, songs, stems, determinism and the drift/Orrery controller still follow it. Where it describes the learner, ADR 0031 wins.

> **Units note (2026-10-27, [ADR 0054](decisions/0054-native-units-schema-v0-9.md)):** the loop doc is schema **v0.9**: a param is written in its unit under a suffixed key (`cutoff_hz`, `attack_s`, `level_db`, with `-Infinity` for silence), and a route's depth in its dest's unit (`depth_oct`, `depth_db`, …). The engine still drifts in a normalized `0..1` through the seam's curves (ADR 0005). Where this SRD writes a param as `0..1` or uses a v0.7/v0.8 key, that is the original design; ADR 0054 and `doc/loop_contract_v0.9.json5` win.

# Generative Drone System — System Requirements Document v1.1
 
## 1. Purpose & Vision
 
### 1.1 Product vision
A generative ambient/drone music application for sleep and focus, eventually targeting mobile (iOS first, then Android). Unlike conventional generative music, the system continuously "listens" to its own output via an audio-understanding model (CLAP) and adapts its generation to steer the music's perceptual character toward user-specified goals (e.g. "warmer," "less harsh") over hours-long sessions.
 
### 1.2 Layered architecture (full system)
1. **Layer 1 — Audio Synthesis & Sequencer Engine.** Continuously-running synths driven by a looped *control* timeline (the loop document). Renders audio. Also hosts the embedded CLAP + DSP observation (see §1.4).
2. **Layer 2 — Controller & Online Learner.** Per-loop: consumes observation data; learns how parameter changes affect CLAP; adjusts the next loop's configuration.
3. **Layer 3 — Intent Source.** Supplies goals to the controller. Full vision: on-device LLM emitting Directives. **MVP: the user directly.** LLM deferred.
### 1.3 Engineering philosophy
- This spec is **functional** except where it defines **contracts** (loop document, control objective, sample/corpus schema, canonical param query syntax), which are technical.
- **Language/framework is intentionally unspecified** — chosen by the coding agent and product owner. The design must be portable: desktop now, **structured so the core can later run on iOS, then Android.** Avoid desktop-only or platform-locked dependencies in the core engine/controller; isolate any such dependencies behind interfaces.
### 1.4 Observation is embedded in the engine
DSP feature extraction **and CLAP scoring run inside the engine**, producing scores **per step** alongside the param/feature state. Rationale: samples must associate CLAP outcomes with param deltas and effective feature state at fine granularity; this cannot be a detached offline pass.
 
### 1.5 Scope: three phases
- **Proof of Concept (POC)** — a headless desktop data-generation lab validating that CLAP-in-parameter-space is a learnable, steerable signal on our engine. Split into:
  - **POC v1** — get the full harness working correctly; **real-time rendering is acceptable**.
  - **POC v2** — scale for faster learning: **faster-than-real-time / offline batch rendering**.
- **MVP** — shippable mobile app (iOS first) with user-driven CLAP steering, built on validated POC findings.
- **Future (out of scope)** — the LLM intent layer; noted only for forward-compatibility.
---
 
## 2. Core Concepts & Terminology
 
| Term | Definition |
|---|---|
| **Loop document** | Complete, serializable description of control state for the *next* loop (schema v0.6). Describes control, not audio. |
| **Control timeline** | The looped automation/note scaffold. Synths run continuously; only this loops. |
| **Modulator (source)** | An `lfo` or `step_seq` producing `control` (normalized) or `note` (event) output. |
| **Matrix** | Routing of sources to destinations; `mod` (summed into 0–1 params) or `note` (voice pools with budget+priority). |
| **Base param** | An instrument/global/connection parameter's authored value before modulation. The learnable unit. |
| **Effective state** | Actual sounding values/phases at a moment, after modulation/glide/probability. |
| **Feature vector** | Description of a base param by `(role, parent constraints, dest/source kind, current effective context, …)`. The abstraction the learner generalizes over. |
| **CLAP score** | Cosine similarity between rendered-audio embedding and a text-term embedding. A *sensor*, never directly set. |
| **Operating point** | The current full state (param values, active chord, modulator phases) — the "from here" the controller works relative to. |
| **Sample** | One learning datum per loop: `(base-param delta(s), effective feature state averaged over the loop's captures) → baseline-corrected ΔCLAP`. |
| **Control Objective** | Intent handed to the controller. MVP: user-authored CLAP-term goals. Future: LLM Directive. |
| **Dither** | Small bounded random parameter perturbation: cold-start behavior *and* sample source. |
| **Antithetic pair** | Two opposite perturbations (+δ and −δ) from the same operating point, differenced to cancel ambient drift (see §5.3). |
 
---
 
## 3. Layer 1 — Audio Synthesis & Sequencer Engine
 
### 3.1 Engine quality bar (critical)
Judged by **machine legibility**, not subjective polish:
- **R3.1.1 CLAP-legibility.** Output must occupy a CLAP region where target terms have meaningful dynamic range as params vary. Validated by the sweep rig (§7).
- **R3.1.2 Monotonic, smooth, artifact-free param response.** Every exposed param responds monotonically, without discontinuities, dead zones, or zipper noise. Automation is click-free (slew/glide honored). The learner cannot distinguish a real acoustic effect from an implementation artifact.
- **R3.1.3 Determinism.** Same `loop_id` + phase snapshot ⇒ reproducible (bit-similar) output.
### 3.2 Effort allocation (normative)
- **High fidelity (primary CLAP levers):** low-pass filter incl. resonance, saturation/drive harmonic character, reverb/space, detune/beating, artifact-free automation.
- **Minimal acceptable:** oscillator pedigree (band-limited basics suffice), polyphony elegance, features/UI (none for POC).
### 3.3 Loop document support (schema v0.6)
- **R3.3.1** Parse and render the full v0.6 loop document (canonical example in project is authoritative).
- **R3.3.2 Transport:** `bpm`, `master_steps`, `step_unit` (`beat` for POC), `swing`, `timing_jitter_ms`.
- **R3.3.3 Harmony:** `root`, `mode` as tonal reference; pitch offsets are semitones from `root`.
### 3.4 Instruments
- **R3.4.1** Four POC slots: `mono`, `poly` (chords/arps), `rhythmic` (mid/high filtered-noise, `pitched:false`), `event` (sparse bell/pluck).
- **R3.4.2** Instruments hold **base params + note engine only** — no embedded time-variation.
- **R3.4.3** `note_engine`: `polyphony`, `glide_ms`; `pitched` flag (false ⇒ offsets ignored, bare triggers).
- **R3.4.4** Per-instrument params ~~normalized 0–1; engine maps to real units~~ are written in their units (Hz, seconds, dB; `-Infinity` is silent) and the engine maps them to a normalized `0..1` internally, for drift. *(Amended by ADR 0054, schema v0.9.)*
- **R3.4.5** Instrument set **extensible at runtime** (e.g. add `event2`) without contract recompilation; new instruments introduce new canonical param paths (§3.8).
### 3.5 Modulators (sources)
- **R3.5.1** Two types: `lfo` and `step_seq`. A `step_seq` has **either** explicit `steps` **or** a `generator` (e.g. `euclid{pulses,rotation}`), never both.
- **R3.5.2** Two output domains: `control` (normalized uni/bipolar) and `note` (events). A `control` route scales the source by a depth written in its dest's unit (`depth_oct` into a cutoff, `depth_db` into a gain; ADR 0054).
- **R3.5.3** `sync`: `loop` (resets at master boundary) or `free` (phase persists, engine-owned).
- **R3.5.4** Control step_seqs honor `entry_value` (C0 continuity); final step value becomes next loop's entry.
- **R3.5.5** `step_duration` is a fraction of the master loop; `length × step_duration ≠ 1.0` ⇒ polymetric drift.
- **R3.5.6** Note steps are always a `voicing` array (mono ⇒ one voice). `prob` lives **only** on voice objects.
- **R3.5.7** Voice objects: `id`, `offset` (semitones), optional `cents_offset`, `prob`, `release`.
- **R3.5.8** **Modulator→modulator references are not allowed** (POC and MVP).
### 3.6 Note/voice transition semantics (by voice `id`)
- **R3.6.1** Present in both ⇒ **glide** old→new over `glide_ms` (identical pitch ⇒ held); no glide-distance cap.
- **R3.6.2** Absent now ⇒ **sustain** prior pitch.
- **R3.6.3** `release:true` ⇒ stop (note-off + tail); only explicit way to end a sustained voice.
- **R3.6.4** New id ⇒ fresh articulation.
- **R3.6.5** Failed `prob` ⇒ treated as **absent** ⇒ sustains (never a dropout).
### 3.7 Matrix (routing)
- **R3.7.1** `kind:"mod"`: `value = clamp01(base + Σ depth·source·polarity)`; supports `mode:"add"`, `depth`, `depth_slew_ms` (depth changes ramp; omit ⇒ instant).
- **R3.7.2** `kind:"note"`: source feeds a voice pool; connection carries `voices` budget + `priority`.
- **R3.7.3 Fan-out:** instrument consumes up to `polyphony` voices in declared id order.
- **R3.7.4 Fan-in:** multiple note sources → one instrument; allocate by `priority` (lower=higher), each up to its `voices`.
- **R3.7.5 Voice stealing:** lowest-priority-source-first, then oldest-within-source; high-priority never stolen.
- **R3.7.6** Connection params (esp. `depth`) are **learnable**, with canonical paths, runtime-addable.
### 3.8 Global section & param addressing
- **R3.8.1** `global` block with real automatable params: `master_gain`, `stereo_width`, `reverb_mix` (+ reverb size/character as needed). *(Schema v0.9: `master_gain_db`; `reverb_mix` is gone — the reverb is a send bus, ADR 0054.)*
- **R3.8.2 Canonical param paths.** Every learnable base param has a stable canonical id (e.g. `poly.params.cutoff`, `mono.params.drive`, `matrix.m_breath_c.depth`, `global.reverb_mix`; in schema v0.9 `poly.params.cutoff_hz`, `mono.params.drive_db`, `matrix.m_breath_c.depth_oct`, and `reverb_mix` is gone — ADR 0054). Used as the key for samples (§6) and queries (§6.4).
### 3.9 Determinism & reproducibility
- **R3.9.1** Root RNG seed = `loop_id`; each probabilistic site derives a stream via `hash(loop_id, source_id, step, voice_id)`.
- **R3.9.2** Free-running modulator phase is captured in a separate **phase snapshot** artifact (not derivable from `loop_id`).
- **R3.9.3** Boundary handoff: engine holds current state; next loop diffs by `id`; persisting ids keep state, new ids init, removed ids release. (Live/MVP: lock-free atomic swap; if next doc not ready, current loop repeats once — safe non-event.)
### 3.10 Rendering modes
- **R3.10.1 (POC v1)** Headless rendering; **real-time acceptable**.
- **R3.10.2 (POC v2)** Headless **offline, faster-than-real-time** batch rendering.
- **R3.10.3 (POC, both)** Deterministic render given (loop document + phase snapshot).
- **R3.10.4 (MVP)** Real-time, glitch-free continuous rendering on-device with lock-free handoff; portable core (iOS→Android).
---
 
## 4. Embedded Observation (DSP + CLAP)
 
- **R4.1** Compute per-capture **DSP features**: at minimum RMS/loudness, spectral centroid, spectral flatness, spectral flux, crest factor, stereo correlation/width, roughness/beating estimate. Final set confirmed in POC.
- **R4.2** Compute **CLAP embedding + scores in-engine, per step**, using a pretrained open CLAP model. POC captures the **full embedding / broad term battery** (not a preset shortlist) so steerable terms are chosen post-hoc (§7).
- **R4.3 Capture cadence:** every ~4 steps (~4s @ 65bpm) → **4 captures per 16-step loop**. CLAP is also scored per step (R4.2) to expose the within-window trajectory for transient exclusion and antithetic comparison.
- **R4.4 Settling:** features/scores attributed to a window use the **settled portion** (exclude attack/glide transients), leveraging per-step scores to locate the settled tail.
- **R4.5 Sample aggregation:** **one sample per loop**; effective **feature state averaged over the loop's 4 captures**; CLAP outcome taken from settled captures.
- **R4.6** Capture, per loop, the **full instance-specific effective state** (all base param values, modulator phases, derived feature context) — not only perturbed params.
---
 
## 5. Layer 2 — Controller & Online Learner
 
### 5.1 Operating modes
- **R5.1.1 `drift`** (no objective): bounded random **dither** of one+ base params per loop. Cold-start behavior and primary POC sample source.
- **R5.1.2 `pursue`** (objective present): query the learned model for param deltas with highest effect in the objective's desired CLAP direction(s); apply within constraints.
- **R5.1.3 `hold`:** stop steering; maintain current base state (music still breathes via free modulators/probability).
### 5.2 Learning representation
- **R5.2.1** Learner models **ΔCLAP as a function of base-param deltas**, generalized via the **param feature vector**. Fixed "descriptors" are explicitly **rejected** as the learning unit.
- **R5.2.2 Generalization:** a sample on one param instance must inform other instances **sharing feature-vector similarity** (e.g. `lfo→poly.cutoff depth` informs `lfo→mono.cutoff depth`).
- **R5.2.3 Storage:** samples stored **non-abstracted, instance-specific (raw)**, enabling inference at both instance and feature-abstraction levels and re-derivation under revised feature schemas.
- **R5.2.4 Partial pooling:** feature-abstraction (type-level) priors used early; instance-specific samples **weighted higher as they accumulate**.
- **R5.2.5 Interactions / operating-point dependence** represented as **dependence of effect on context features** (e.g. drive's effect vs. parent cutoff), not a dense global param-space model.
- **R5.2.6 Cold-start:** may start from a pretrained prior (from POC corpus). Before useful, behavior is no worse than `drift` and still yields useful samples.
- **R5.2.7 Extensibility invariant:** adding instruments/modulators/params at runtime must **not** require enlarging or retraining the model from scratch; new params enter described in the existing feature space at high uncertainty.
### 5.3 Sampling & attribution (correctness-critical)
- **R5.3.1 Baseline correction (non-deferrable).** ΔCLAP measured against a **paired/counterfactual baseline**, never raw loop-to-loop difference, to avoid confounding with ambient drift. **POC method: antithetic ±δ pairs** — from the same operating point, render one loop with +δ and one with −δ on the target param(s), and estimate effect from the difference of their settled CLAP outcomes; the shared drift cancels. Determinism (R3.9) enables matching the operating point across the pair.
- **R5.3.2 Perturbation policy:** few effective directions per loop; **SPSA-style** structured perturbation permitted to estimate many gradients from scarce samples.
- **R5.3.3 Provenance:** every sample records perturbation policy, operating point, `loop_id`/seed, and schema version — enabling offline replay and confounding audits.
- **R5.3.4 Safety/smoothness:** all dithers and pursuit steps bounded by **`max_change_per_loop`**, so exploration is never more disruptive than dithering.
### 5.4 Algorithm guidance
- **R5.4.1** Online learner: recursive/streaming, fast updates, **forgetting factor** for non-stationarity, regularized toward "no effect" so unseen params stay inert until evidence accrues. Bayesian-linear/RLS over the feature vector (optionally with interaction features) is the reference approach; nonlinear models over the same feature vector are permissible upgrades.
---
 
## 6. Contracts (technical)
 
### 6.1 Loop Document — schema v0.6
Authoritative example stored in project. Engine MUST implement it exactly, including the §3.5–3.9 semantics.
 
### 6.2 Control Objective — schema v0.1
Per the Control Objective Contract in project: `mode` (`pursue`/`hold`/`drift`), `responsiveness` (0–1), `clap_objective[]` (signed weights on `clap:` terms), `guardrails` (param/descriptor ceilings & floors, `max_change_per_loop`, `clap_avoid` hard reflex), `meta.notes`.
- **R6.2.1** Goals in **CLAP terms** (sensors); limits in **param space** (actuators); vocabularies namespaced.
- **R6.2.2** Standing & complete (edit replaces; `objective_id` increments); **re-base on change** (move from current state, never jump).
- **R6.2.3** Guardrails dominate intent and only tighten; `clap_avoid` is an always-on hard reflex independent of weights.
- **R6.2.4** POC drives this programmatically via the CLI/harness; MVP exposes it to the user. Same contract.
### 6.3 Sample / Corpus — schema (versioned)
- **R6.3.1** Each sample records: `loop_id`; perturbed param(s) by canonical path + δ; **full instance-specific effective state** (all base params, modulator phases, derived feature context, averaged per R4.5); baseline-corrected ΔCLAP per term + raw CLAP scores/embedding; provenance (R5.3.3); schema version.
- **R6.3.2** Corpus is **append-only, schema-versioned**, and rich enough to **recompute features and re-derive groupings offline without re-rendering**. Acceptance test: a feature-schema change is re-evaluable against all past samples with zero new renders.
### 6.4 Canonical Param Query Syntax
- **R6.4.1** A **standardized JSON query syntax** addresses params by canonical name and by feature constraints, e.g. select `params.drive` on instruments where `pitched==true AND params.cutoff < 0.3`, or select `matrix.*.depth` where `source_kind=="lfo" AND dest_kind=="filter"`. Used by the CLI and harness for sample queries and gradient-inference queries. Exact grammar defined alongside the canonical-path scheme (R3.8.2).
---
 
## 7. Sweep Rig & Engine Validation (POC gate)
- **R7.1** A **render-sweep-and-score** rig sweeps each base param across its range (singly, plus selected pairs), renders, and CLAP-scores each point.
- **R7.2** Per-param report: response curve per CLAP term, dynamic range, monotonicity, smoothness — answering **"is the engine CLAP-legible?"** and **"which terms are steerable?"** in one pass.
- **R7.3** Terms with sufficient dynamic range become the **curated steerable-term shortlist** (resolves the deferred objective-vocabulary decision).
- **R7.4** This rig **gates** downstream learning: flat/jagged curves are engine defects to fix first.
- **R7.5 Determinism check:** repeated identical renders ⇒ bit-similar output/scores.
---
 
## 8. Learning Harness & Corpus (POC core deliverable)
The POC's true deliverable is **a clean sample corpus + an offline laboratory**, not a finished controller.
 
### 8.1 Three artifacts
- **R8.1.1 Instrumentation rig (in-engine/online):** dithers, captures, logs samples with full instance-specific feature state, base-param delta(s), baseline-corrected ΔCLAP, provenance.
- **R8.1.2 Corpus (durable):** per §6.3.
- **R8.1.3 Exploration harness (offline):** §8.2.
### 8.2 Harness required analyses
- **R8.2.1 Grouping discovery:** propose `feature → effect` structure; cluster feature vectors into candidate "types"; learn type→effect; export **rule-based groupings** for gradient-inference queries.
- **R8.2.2 Coverage / confounding audit:** report feature-space support for any claimed group/effect; flag effects resting on thin/skewed sampling.
- **R8.2.3 Counterfactual replay:** re-simulate "CLAP without the dither" from seed+deltas to validate the online baseline and test alternative baseline definitions on existing data.
- **R8.2.4 Active sampling:** recommend which params to dither next to most reduce uncertainty per sample.
### 8.3 Synthetic ground-truth validation (build before real CLAP)
- **R8.3.1** A `fake_clap(features)` mode with **planted, known structure**; the harness must **rediscover** it before being trusted on real CLAP.
- **R8.3.2** Engine and learner validated **independently**: synthetic-CLAP tests the harness (engine factored out); the sweep rig (§7) tests the engine (learner factored out). Full online loop runs only after both halves are trusted.
---
 
## 9. CLI (POC tooling)
- **R9.1** A **basic but robust CLI** to operate the system headlessly. Required commands (functional):
  - **Run engine** from a starter loop document (render to file/buffer; choose realtime or offline batch where available).
  - **Orchestrate sample capture** (start/stop a dither-mode data-collection run; set run parameters: duration/loops, dither policy, δ, max-change bounds).
  - **Query samples & features** using the canonical param query syntax (R6.4) — filter, count, summarize, export.
  - **Inspect coverage** (feature-space support; gaps).
  - **Run/inspect harness analyses** (grouping discovery, coverage audit, counterfactual replay, active-sampling recommendations).
  - **Check ML training/job status** (progress, last results, model/version in use).
  - **Drive a Control Objective** programmatically (set/replace objective; observe pursuit).
  - **Toggle synthetic-CLAP mode** for harness validation.
- **R9.2** CLI output must be human-readable and machine-parseable (e.g. JSON option) for scripting experiments.
---
 
## 10. Phase Scope Matrix
 
| Capability | POC v1 | POC v2 | MVP | Future |
|---|---|---|---|---|
| v0.6 loop-doc engine (4 instruments, modulators, matrix) | ✅ | ✅ | ✅ | |
| Headless render, **real-time ok** | ✅ | ✅ | — | |
| **Faster-than-real-time / offline batch** render | — | ✅ | — | |
| Real-time on-device render + lock-free handoff (iOS→Android) | — | — | ✅ | |
| Embedded DSP features | ✅ | ✅ | ✅ | |
| **Embedded CLAP scoring, per step** (pretrained open CLAP) | ✅ | ✅ | ✅ | |
| Sweep rig + legibility gate | ✅ | (reuse) | (reuse) | |
| Dither mode + sample capture + corpus | ✅ | ✅ | ✅ (runtime) | |
| Offline exploration harness (grouping/coverage/replay/active) | ✅ | ✅ | — | |
| Synthetic ground-truth validation | ✅ | ✅ | — | |
| Online learner (feature-vector, partial pooling, forgetting) | ✅ (explore) | ✅ | ✅ (productized) | |
| Pretrained prior shipped | — | (produce) | ✅ | |
| User Control Objective (CLAP-term steering) | programmatic | programmatic | ✅ (UI) | |
| `clap_avoid` hard-reflex safety | ✅ | ✅ | ✅ | |
| CLI tooling | ✅ | ✅ | (dev only) | |
| Mobile app / UI / session mgmt | — | — | ✅ | |
| Modulator→modulator refs; automatable depth | — | — | — | ✅ |
| LLM Directive intent layer | — | — | — | ✅ |
 
---
 
## 11. Cross-Cutting Requirements
- **R11.1 Determinism** end-to-end (seeded probability + phase snapshot) so any loop/sample is reconstructable.
- **R11.2 Schema versioning** on loop document, objective, sample/corpus, feature schema; all artifacts carry their version.
- **R11.3 Forward-compatibility seam:** the controller consumes an *intent* object; user (MVP) and LLM (future) are interchangeable producers.
- **R11.4 Graceful degradation:** empty objective ⇒ `drift` baseline; learner failure ⇒ no worse than dither; weak device (MVP) ⇒ deterministic controller, no LLM.
- **R11.5 Safety:** `max_change_per_loop` and `clap_avoid` enforced regardless of mode/intent.
- **R11.6 Portability:** core engine/controller/observation isolated from platform-specific audio I/O behind interfaces, so the same core targets desktop (now), iOS, then Android. Language/framework left to implementer but must not preclude this.
---
 
## 12. Explicit Non-Goals (POC)
- No mobile app, UI, or on-device deployment.
- No model quantization / on-device CLAP optimization (CLAP runs in the desktop engine for POC).
- No LLM.
- No modulator→modulator references or automatable connection depth.
- No subjective "production-grade" polish beyond the legibility bar (§3.1).
- No finalized feature schema or steerable-term list (both are **outputs** of the POC).
---
 
## 13. Resolved Assumptions (from v1 open questions)
1. Spec is functional; only contracts are technical. Language/framework chosen by implementer; core kept portable (desktop→iOS→Android). ✔
2. Headless with a **CLI**. Real-time render acceptable for **POC v1**; faster-than-real-time deferred to **POC v2**. ✔
3. **CLAP runs embedded in the engine**, scored **per step**; pretrained open CLAP. ✔
4. Baseline = **antithetic ±δ pairs** (plain-English definition in §5.3.1). ✔
5. **One sample per loop**; features **averaged over the 4 captures**. ✔
6. Global FX: `master_gain`, `stereo_width`, `reverb_mix` (+reverb size/character). ✔
7. **JSON query syntax** for canonical param names (§6.4). ✔
8. Modulator→modulator references **disallowed** (POC + MVP). ✔
---
 
*End of SRD v1.1.*
 
