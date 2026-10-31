# Design Rationale & Decision Log — Generative Drone System

> **Scope note (2026-10-09, [ADR 0031](0031-composition-rescope-and-learner-removal.md)):** the project was re-scoped to a composing instrument for long-form generative tracks, and the CLAP learner (perceptual scoring, the ±δ corpus, the online RLS, `pursue`, the sweep/legibility gate, the offline harness) was **removed**. This document remains the record of the original reasoning. Where it describes the learner, ADR 0031 wins: §0's product paragraph (a CLAP-steered app whose POC deliverable is a corpus + harness), §4 (controller & learning), §5.1–5.2's CLAP goals, `clap_avoid` and objective weights, §5.3, §7 (the POC framing: corpus, harness, sweep gate) and the learner / CLAP parts of §1.3, §6, §8 and §9. Still authoritative: the engine and loop-document reasoning — §1.1, §1.2, §1.4, §2 (the schema has since moved to v0.9: `doc/loop_contract_v0.9.json5`, [ADR 0054](0054-native-units-schema-v0-9.md), which also reversed §2.6's normalized authoring), §3, §5.2's guardrails (ceilings / floors / `max_change_per_loop` only tighten), §6's deferrals of modulator→modulator references and automatable depth, and §8's determinism, versioning and portability invariants. The Orrery, drift, songs and the composition workflow are reasoned elsewhere, not here: `orrery-generative-harmony-design.md` + ADRs 0014–0019 (the Orrery + Arranger), 0025 + 0032 (drift: `max_drift`, reflecting drift), 0029 (songs) and 0031–0039 (the re-scope, control in the doc, pieces, live editing, the editor, parts).

> **Purpose of this document.** The SRD (`doc/SRD_v1.1.md`) and the loop contract
> (`doc/loop_contract_v0.9.json5`) state *what* to build. This document
> records *why* — the reasoning, the alternatives considered, and the alternatives
> rejected — so future agents and contributors don't re-litigate settled questions
> or accidentally undo a load-bearing decision.
>
> When this conflicts with the SRD, the SRD is authoritative for *what*; this doc
> explains the *why* behind it. If you believe a decision here is wrong, raise it
> explicitly rather than quietly reverting it — most of these were reached after
> weighing specific tradeoffs documented below.

---

## 0. The product in one paragraph
A generative ambient/drone app for sleep and focus that continuously "listens" to
its own output through a CLAP model (which maps audio and text into a shared
embedding space) and steers generation toward user-named perceptual goals
("warmer," "less harsh") over hours-long sessions. The near-term effort is a
**Proof of Concept**: a headless desktop *data-generation lab* whose real
deliverable is a clean sample corpus + an offline analysis harness, not a finished
controller or app.

---

## 1. Foundational architectural decisions

### 1.1 Loop the *control timeline*, not the audio
**Decision.** Synthesis (oscillators, filters, reverb tails) runs continuously and
never restarts. What "loops" is a 16-step control scaffold — chord/note events and
parameter automation that the synths read from.

**Why.** This dissolves the loop-seam problem entirely. There is no audio buffer
wrap, no click, no phase reset at the boundary, because nothing in the audio path
actually loops — only the *source of control values* hands off. A drone built this
way is seamless by construction.

**Consequence.** The unit of change is "the control state for the next loop." The
controller decides the next loop's configuration *during* the current loop and
swaps it in at the boundary. This is why the central artifact is the **loop
document** ("the complete intended control state for the NEXT loop").

### 1.2 Per-loop control, not continuous nudging
**Decision.** The controller adjusts parameters for the *next loop*, not at
arbitrary time intervals.

**Why.**
- **Latency decoupling.** Next-loop config is computed during the current loop
  (~15 s at 65 bpm), so on-device inference speed stops mattering. A late decision
  is never an audible failure — the current loop simply repeats once.
- **Musicality.** Changes that land on loop boundaries read as intentional
  composition; the same change at a random moment reads as drift or glitch.
- **Good LLM/optimizer target.** Editing a discrete symbolic structure between
  loops plays to a model's strengths; emitting smooth continuous floats in
  real time plays to its weaknesses.

### 1.3 Three-layer architecture along timescale boundaries
**Decision.**
- **Layer 1** — synthesis + sequencer engine (audio/control rate; renders + hosts
  embedded observation).
- **Layer 2** — DSP/observation + controller/learner (per-loop; senses + decides).
- **Layer 3** — intent source (multi-loop; plans). **MVP: the user. Future: an LLM.**

**Why.** Each boundary is simultaneously a timescale boundary and a concern
boundary; when layering follows the natural clock rates of the problem, the
interfaces between layers stay simple and each layer becomes independently testable.

### 1.4 Pass serializable documents between layers, not behavior
**Decision.** Inter-layer interfaces are plain-data contracts: the **loop document**
(controller → engine), the **feature/observation data** (engine → controller), and
the **control objective / directive** (intent → controller).

**Why.** Plain-data boundaries make every layer testable in isolation: run the
engine from a hand-written loop document; run the controller against recorded
observations with no audio; etc. This property was designed for deliberately.

---

## 2. The loop document (schema v0.6) — key decisions

The full annotated schema lives in `doc/loop_contract_v0.9.json5`. The
reasoning behind its shape:

### 2.1 Separate sources from destinations (mod-matrix model)
**Decision.** Instruments hold only **base parameter values + a note engine** — no
time-variation. All time-variation lives in **modulators** (sources). A **matrix**
routes sources to destinations.

**Why.**
- Cleanly separates "what changes over time" from "what it changes."
- Puts phase/runtime state in the engine (where it belongs), so the controller
  doesn't bookkeep phase, and continuity across the seam is nearly free for
  free-running modulators.
- Subsumes machinery (per-instrument trigger fields, ad-hoc envelopes) into one
  concept: a source routed through the matrix.

### 2.2 Two source types, two output domains
**Decision.** Source `type` is `lfo` or `step_seq`. Output `domain` is `control`
(normalized) or `note` (events). A `step_seq` has **either** explicit `steps`
**or** a `generator` (e.g. Euclidean), never both.

**Why.**
- `lfo` was kept as its own type (rather than folding into `step_seq`) as the one
  principled exception, because an LFO isn't really "stepped." Everything else is a
  `step_seq`.
- Euclidean rhythm is not a distinct *type* — it's a compact *generator* that
  expands into steps. Keeping it as a generator preserves the compact control
  surface ("17 pulses over 44") that both the optimizer and a future LLM want to
  turn, without multiplying types.

### 2.3 Resolution / pattern / drift are independent
**Decision.** A stepped source declares resolution (`step_duration`, a fraction of
the master loop), pattern (explicit `steps` or generator params), and drift
(`sync: "loop"` vs `"free"`) independently.

**Why.** These are genuinely separable concerns. An arpeggio is
`length:44, step_duration:"1/44", sync:"loop"`; a polymetric layer is
`length:23, step_duration:"1/16", sync:"free"`. Same primitive, different settings.
`length × step_duration ≠ 1.0` produces polymetric drift — a deliberate
anti-repetition tool.

### 2.4 Note events: voicing-always, identity by `id`
**Decision.** Every note step is a `voicing` array (mono = a one-voice voicing).
Each voice has a stable `id`. Probability (`prob`) lives **only** on voice objects.

**Why.**
- Voicing-always removed the last polymorphism in the note domain (no more "bare
  offset vs. voicing array"), which in turn fixed where probability lives.
- Voice `id` makes glide-vs-retrigger explicit and emergent at once: same `id`
  across steps = glide; new `id` = fresh articulation; vanished `id` = sustain.
  This replaced an earlier, more verbose `voice_leading` block — the `id` carries
  the one bit that mattered (identity), and the engine infers the transition.

### 2.5 Voice transition semantics (the four cases)
**Decision (engine must implement exactly):**
- Present in both consecutive active steps ⇒ **glide** old→new over `glide_ms`
  (identical pitch ⇒ held). **No glide-distance cap** — any interval glides; it is
  the intent layer's job to avoid undesirable large glides.
- Absent now ⇒ **sustain** prior pitch.
- `release:true` ⇒ **stop** (note-off + tail) — the only explicit way to end a
  sustained voice.
- New `id` ⇒ **fresh articulation**.
- Failed `prob` ⇒ treated as **absent** ⇒ sustains (never a dropout).

**Why.** "Absent = sustain" guarantees a drone never accidentally drops a voice;
the explicit `release` is the escape hatch. Dropping the glide cap keeps the engine
simple and flexible; the consequences of a large glide are a musical concern pushed
up to the controller/intent layer, not an engine guardrail.

### 2.6 Pitch as offsets from a root; normalized control values

> **Reversed in part by [ADR 0054](0054-native-units-schema-v0-9.md) (2026-10-27, schema v0.9):** pitch as offsets from a root stands; normalized authoring doesn't. The learned sensitivity map that justified it went with ADR 0031, and v0.9 writes every param in its native unit (`cutoff_hz`, `attack_s`, `level_db`). The `0..1` space survives inside the engine, where the seam (ADR 0005) keeps one drift step for every param.

**Decision.** Pitches are integer **semitone offsets** from `harmony.root`
(+ optional `cents_offset` for detuned/beating voicings). Control values are
normalized 0–1 (or −1..+1 bipolar).

**Why.**
- Offsets let the whole piece transpose or change mode by editing one field, and
  make a sequence reusable across keys. They also make relative-motion constraints
  and the learner's math cleaner.
- Normalized control values give resolution-independence and make the constraint
  sandbox and the learned sensitivity map tractable; the engine owns the map to
  real units (Hz, dB).

### 2.7 Fan-in arbitration (the non-obvious matrix requirement)
**Decision.** When multiple **note** sources target one instrument, each connection
carries a `voices` budget and a `priority` (lower = higher priority). The engine
allocates the instrument's polyphony by priority, each source up to its budget.
**Voice stealing:** lowest-priority-source-first, then oldest-within-source;
high-priority sources (e.g. a sustained pad at priority 1) are never stolen.

**Why.** A mod matrix is many-to-one by design. For `control` destinations,
overlapping sources *sum*. But you cannot "sum" notes. The real gap was fan-in
arbitration for pitched events — needed immediately for "a chord *and* an arpeggio
on the same synth," which is in scope for focus music. (Note: the earlier idea of
`note_select` for "which chord tone a mono takes" was rejected — polyphony +
voice-id ordering already covers fan-*out*.)

### 2.8 `pitch_walk` is provisional
**Decision.** The arp source carries a self-contained `pitch_walk` pool rather than
reading the chord source's current pitches.

**Why.** This exists *only* because modulator→modulator references are deferred
(see §6). When those are added, the arp would instead read `chords` directly and
`pitch_walk` disappears. Until then, the controller must update the arp's pool to
track harmony. Flagged as provisional so it isn't mistaken for permanent design.

### 2.9 Determinism: `loop_id` as root seed + phase snapshot
**Decision.** No separate seed field — `loop_id` is the root RNG seed. Each
probabilistic site derives an independent stream via
`hash(loop_id, source_id, step, voice_id)`. Free-running (`sync:"free"`) modulator
phase is **not** a function of `loop_id`; it is captured in a separate **phase
snapshot** artifact.

**Why.**
- Hashing `loop_id` with a per-site key decorrelates adjacent `loop_id`s (naive
  adjacent seeds can produce visibly similar early draws) and is robust to parallel
  evaluation / changed evaluation order.
- `loop_id` reproduces a loop's *decisions*; the phase snapshot reproduces the
  drifting layers' *absolute phase*. Together they make any moment reproducible —
  which is what makes the antithetic-pair baseline (§4.3) and offline replay
  possible. Probabilistic non-repetition (good at runtime) and reproducibility
  (needed for the lab) coexist because the seed lives in the snapshot, not the doc.

---

## 3. Anti-repetition strategy (why the music doesn't sound like a loop)
For a *sleep* app especially, audible repetition is the enemy. Three mechanisms,
all used together:

1. **Hybrid timing.** Quantize only structural/harmonic change to the loop grid.
   Let micro-textural parameters evolve via **free-running LFOs independent of the
   grid**, so the texture never lines up with the boundary (prevents a ~15 s
   "pulsing" at the loop period).
2. **Polymetric layers.** Run layers of different lengths (16, 17, 23 steps) so
   they drift against each other and the combined texture never exactly repeats.
3. **Subtle scaffold.** Treat the 16-step grid as a control clock, not an audible
   rhythm. A drone often has no beat; the grid just sets automation resolution and
   chord-change points.

Also relevant: the rhythmic-noise layer is the most loop-detectable (the ear locks
onto rhythm faster than slow harmonic change), so it is the layer made polymetric
and/or probabilistic.

---

## 4. The controller & learning — the most-revised area

This is where the design evolved most. The progression of thinking matters because
it explains why the *final* approach looks the way it does.

### 4.1 Why the controller does NOT chase CLAP with hand-written logic
**Reasoning.** Closed-loop feedback control toward a CLAP target requires a model
relating parameter changes to CLAP outcomes (a "plant model"). A purely
deterministic controller has none — and the true param→CLAP relationship is
non-monotonic and entangled. CLAP is also a poor tight-loop feedback signal
(computed only a few times per loop, multi-second window, semantically fuzzy).
Feedback control with measurement lag and no plant model oscillates or drifts.

**Therefore.** The controller must not be asked to steer toward CLAP scores using
fixed rules. It needs a *learned* model of param→CLAP, OR the perceptual loop must
be closed by a slower, model-bearing component.

### 4.2 The learning unit: feature vectors, NOT descriptors, NOT raw config
This went through three positions before settling:

1. **Descriptors (rejected as the learning unit).** An early idea: learn over ~10
   fixed mid-level axes (brightness, density, warmth…) expanded to params by a
   fixed table. *Rejected* because it cannot accommodate brand-new params from
   instruments/modulators added at runtime — a new param has no place in a fixed
   descriptor vocabulary. (Descriptors were also considered as a user/LLM-facing
   vocabulary; that too was set aside for the MVP, see §5.)
2. **Raw config params (rejected as the learning unit).** Learning directly over
   the loop JSON is hopeless — high-dimensional, structured, mostly discrete,
   un-learnable from scarce samples; and a new modulator would break a fixed-width
   model.
3. **Param feature vectors (ADOPTED).** Learn how param changes affect CLAP as a
   function of a **feature vector describing each param** — its `role`, parent
   instrument constraints (e.g. `pitched`, current `cutoff`/register/level), and
   for matrix params the `source_kind`/`dest_kind`. The model learns
   `effect_on(term) ≈ g(feature_vector(param))`.

**Why feature vectors win.**
- **Generalization across instances.** A sample on `lfo→poly.cutoff depth`
  informs `lfo→mono.cutoff depth` automatically, because their feature vectors are
  nearly identical. More learning from fewer samples.
- **Dimensionality firewall.** The feature space has *fixed width* even though the
  number of params is unbounded. A new instrument/modulator is *described* into the
  same feature space at high uncertainty — it never enlarges the model. This is the
  property that satisfies the runtime-extensibility requirement (SRD R5.2.7).
- **Interactions for free.** "Drive's effect depends on cutoff" is just `g` being a
  function of `parent_cutoff` — operating-point dependence and interaction become
  the same mechanism (feature dependence), no dense global model needed.

**Important nuance:** types/groupings are best treated as a **continuous feature
space**, not discrete buckets. Buckets have hard edges (a cutoff drifting 499→501
Hz flips the bucket), multiply combinatorially, and must be pre-enumerated. A
continuous feature vector avoids all three and is strictly more expressive
("drive matters *more* the lower the cutoff" — a gradient, not a cliff).

### 4.3 Sampling correctness: antithetic-pair baselines (non-deferrable)
**Decision.** ΔCLAP must be measured against a **paired/counterfactual baseline**,
never a raw loop-to-loop difference. POC method: **antithetic ±δ pairs**.

**What that means (plain English).** Instead of one nudge, do two opposite nudges
from the same operating point: render one loop with +δ on the target param and one
with −δ, and estimate the effect from the *difference* of their settled CLAP
outcomes. The ambient drift (free LFOs, probabilistic voicing, slow harmonic
motion) is roughly the same in both halves, so it **cancels** in the subtraction,
leaving the part actually caused by the param.

**Why non-deferrable.** Without it, ΔCLAP silently absorbs all the ambient motion
deliberately built in for non-repetition, and the learner chases ghosts. No amount
of offline cleverness recovers a clean signal from a contaminated corpus. The
engine's determinism (§2.9) is what makes matching the operating point across the
pair possible — the determinism work pays off here.

### 4.4 Storage: raw, instance-specific, with provenance
**Decision.** Store samples **non-abstracted and instance-specific** (raw), each
carrying the full effective state, the base-param delta(s), the baseline-corrected
ΔCLAP, and **provenance** (perturbation policy, operating point, `loop_id`/seed,
schema version).

**Why.**
- Raw storage lets you infer at both the instance level and the feature-abstraction
  level, AND **re-derive groupings under a revised feature schema without
  re-rendering**. If you'd stored only the abstracted form, any schema change would
  invalidate the whole corpus. This is what makes the offline lab iterable.
- Provenance is what lets the harness detect *sampling artifacts* — a "discovered"
  effect that is real in the corpus only because of how you happened to sample
  (e.g. you only ever moved drive while cutoff was low). Coverage auditing depends
  on it.

### 4.5 Partial pooling (type-level prior + per-instance residual)
**Decision.** Early on, an instance is predicted by its *type* (feature-level)
effect — fast, generalized. As an instance accumulates its own samples, they are
**weighted higher**, letting the instance deviate from its type — accurate,
personalized. The amount of trust shifts with evidence (derivable, not hand-tuned).

**Why.** Pure type-level pooling can't represent a genuinely deviant instance; pure
instance-level learning is starved. Partial pooling gets both. Outliers, before
they're trusted, move the sound no more than dithering does (bounded by
`max_change_per_loop`), so the risk is contained.

### 4.6 Algorithm: RLS over the feature vector, with a pretrained prior
**Decision.** Reference approach: recursive/streaming Bayesian-linear / RLS over the
feature vector, with a **forgetting factor** (non-stationarity) and regularization
**toward "no effect"** (unseen params stay inert until evidence accrues). May start
from a pretrained prior produced from the POC corpus. Nonlinear models over the
*same* feature vector are permitted upgrades.

**Why RLS is the right *starting* tool, not the whole answer.** RLS fits a local
linear model cheaply (microsecond updates, tiny state, principled forgetting) — a
good fit for "from here, which way moves term *t*." Its limit is that it's linear
and assumes a fixed feature vector. Both are handled by: (a) learning over a feature
vector that *includes context features* (so curvature/interaction appears as
feature dependence), optionally with interaction features `d_i·d_j`; and (b) using
an offline-fit global surrogate (e.g. a GP) as the **prior mean**, with online RLS
as a *local residual correction*. Global nonlinear model + local linear adaptation.

### 4.7 Perturbation policy: few directions, SPSA-style
**Decision.** Perturb few effective directions per loop; **SPSA-style** structured
perturbation is permitted to estimate many gradients from scarce samples.

**Why.** With raw-param learning, dithering many params at once makes attribution
under-determined from few samples. SPSA perturbs many params with random ± signs
and uses the correlation of the sign pattern with the outcome to estimate all their
gradients at once — the textbook answer to "estimate a high-dim gradient from
scarce, noisy measurements."

### 4.8 Dither mode = cold-start behavior AND sample source
**Decision.** With no objective, the controller applies bounded random **dither**
each loop. With an objective, it queries the learned model for the highest-effect
param deltas in the desired direction.

**Why.** This unifies cold-start and data collection: the system is never "broken
before trained" — absent an objective it does musically-acceptable random walk
*and* generates exactly the (Δparam → ΔCLAP) samples the learner needs. Directive-
mode and dither-mode produce the same *kind* of sample; the only difference is
whether the perturbation is chosen by the model or by chance. Cold-start is
therefore "no worse than dithering," which is acceptable.

---

## 5. Intent layer: user-first, LLM deferred

### 5.1 Why the LLM was dropped from the MVP
**Decision.** For MVP, the **user** occupies the intent slot directly: they are
shown CLAP terms (positive ones to boost, negative ones to suppress) and steer the
controller's optimization. The LLM is deferred to a future phase.

**Why.**
- Dropping the LLM removes the single hardest-to-validate piece and lets the team
  prove the load-bearing assumption first: *can CLAP optimization produce music that
  audibly evolves the way someone asked?* If yes, the LLM is a later luxury that
  automates the manual steering. If no, no LLM cleverness would have saved it.
- Long (hours-long) sessions make *slow* learning acceptable — converting slow
  convergence from a liability into a non-issue, and allowing the safe, slow
  algorithms.
- The result is deterministic and inspectable: when the music does something, you
  can trace exactly why (objective → gradient → guardrail), far easier to debug
  than "the LLM said reduce brightness."

**Forward-compatibility seam.** The controller consumes an *intent* object; the user
(MVP) and an LLM (future) are interchangeable producers of it. The Control Objective
contract is that seam.

### 5.2 Control Objective contract — key decisions
- **Goals in CLAP terms (sensors); limits in param/descriptor space (actuators);
  vocabularies namespaced.** The controller never sets a CLAP value directly (it's
  a sensor) and never optimizes a param as a goal — that separation *is* the
  feedforward/feedback architecture.
- **Signed weights**, not two lists: one `clap_objective[]` where sign = direction
  (+boost/−suppress) and magnitude = relative priority. Maps cleanly to per-term
  UI sliders.
- **Relative, not absolute targets.** Weights are optimizer coefficients; the
  controller climbs/descends terms rather than aiming at specific similarity
  numbers (CLAP scores are too fuzzy to target absolutely).
- **Standing & complete; re-base on change.** Editing replaces the objective and
  increments `objective_id`; a new objective means "from current state, pursue
  this" — never a jump.
- **Guardrails dominate and only tighten.** `ceilings`/`floors`/`max_change_per_loop`
  compose with the controller's built-in sandbox by taking the *stricter* bound.
- **`clap_avoid` is a separate hard reflex**, distinct from negative weights: if a
  term crosses a threshold, a predefined conservative correction fires immediately,
  overriding pursuit until it clears. A soft "ease harshness down" preference and a
  hard "if it ever gets harsh at 3 a.m., intervene now" safety cutout are different
  mechanisms and were kept separate.

### 5.3 The descriptor → param "expansion table" (authored, not learned)
Even though descriptors were rejected as the *learning* unit, the concept of a
fixed, **authored** map from perceptual knobs to engine params remains useful (and
is how a future LLM/user-facing descriptor vocabulary would actuate). Decisions:
- **Additive-over-base:** `final_param = clamp01(base + Σ contribution_d)`. Each
  param has a neutral base; each descriptor contributes a signed delta; they sum
  then clamp. Same mental model as the mod matrix; no descriptor "owns" a param, so
  overlaps compose instead of fighting.
- **Per-influence triple `{base, gain, shape}`** rather than full breakpoints. One
  gain number per influence = one Jacobian entry; easy to author, tune, and learn
  over; keeps the descriptor→param Jacobian constant and known.
- **This map is authored, not learned.** Only the param→CLAP half of the chain is
  learned/uncertain. That split means half the chain is known exactly and only the
  genuinely unpredictable half is estimated.

---

## 6. Deliberately deferred / out of scope (and why)
- **Modulator→modulator references** (e.g. an LFO modulating another source's rate,
  or the arp reading the chord source). Deferred (POC + MVP) to avoid evaluation-
  order and feedback-loop complexity. Its absence is the only reason `pitch_walk`
  (§2.8) exists. Adding it later (one level, no cycles) is the clean upgrade.
- **Automatable connection depth** (a connection's `depth` itself being a
  modulation destination). Deferred — it's a second way to do envelopes; depth stays
  a slewed scalar for now.
- **The LLM intent layer.** Future phase (§5.1).
- **On-device CLAP / quantization, mobile app, UI.** MVP/future concerns; the POC
  runs CLAP embedded in a desktop engine.
- **Coupling per-step pitch into note sources beyond what's specified** — kept
  timing and pitch decoupled where possible for modularity (arp = euclid timing +
  pitch source), except where coupling simplified the model (note+trigger unified
  into the `note` domain, collapsing an earlier separate "gate" domain).

---

## 7. The POC framing that should govern all early work

### 7.1 The POC is a data rig + corpus + offline lab — not a controller
The online controller is almost a byproduct. The real deliverables are: a clean
sample **corpus** and an offline **exploration harness** that turns the corpus into
the grouping rules. Hold this framing — it reorders priorities.

### 7.2 Three artifacts, and design them backwards
1. **Instrumentation rig** (in-engine/online): dithers, captures, logs samples with
   full effective state + delta + baseline-corrected ΔCLAP + provenance.
2. **Corpus** (durable): append-only, schema-versioned, rich enough to recompute
   features and re-derive groupings offline **without re-rendering**. Acceptance
   test: a feature-schema change is re-evaluable against all past samples with zero
   new renders.
3. **Exploration harness** (offline): grouping discovery; **coverage/confounding
   audit** (the guard against sampling artifacts); **counterfactual replay**
   (validate the baseline; test alternative baselines on existing data); **active
   sampling** (recommend what to dither next to most reduce uncertainty).

**Design backwards:** the harness's questions determine the corpus schema, which
determines the rig's logging format. Decide the harness questions first, or the
early corpus may lack fields (provenance, seeds) the lab later needs — wasting it.

### 7.3 Validate the two unknowns independently
The engine and the learning harness are both under construction and can mask each
other's failures. Two independent checks, both built **before** the full online
loop:
- **Synthetic ground-truth (`fake_clap`).** A known analytic function with planted
  structure replaces CLAP; the harness must *rediscover the planted structure* from
  rig samples. Tests the harness with the engine factored out. If it can't recover
  known structure from synthetic data, it can't be trusted on real CLAP — and real
  CLAP has no answer key.
- **Sweep rig (render-sweep-and-score).** Tests the engine with the learner factored
  out (see §8).

### 7.4 The engine's quality bar is legibility, not polish
The engine is judged by **machine legibility**, not subjective sound quality:
- **CLAP-legibility** — output must sit where target terms have dynamic range as
  params vary.
- **Monotonic, smooth, artifact-free param response** — because the learner cannot
  distinguish a real acoustic effect from an implementation artifact (zipper noise,
  dead zones, non-monotonic folds). Zipper noise in particular reads as "harsh" to
  CLAP and contaminates every sample where a param moved.
- **Determinism** — same `loop_id` + phase snapshot ⇒ bit-similar output.

**Effort allocation that follows from this:** spend the budget on the **low-pass
filter, saturation/drive, reverb/space, detune/beating, and artifact-free
automation** (the primary CLAP levers for ambient material). Spend the minimum on
oscillator pedigree, polyphony elegance, and features. The judge is CLAP, not a
musician.

### 7.5 The sweep rig is the gate, and it picks the steerable terms
A **render-sweep-and-score** rig sweeps each base param across its range, renders,
and CLAP-scores each point, reporting per-param response curves, dynamic range,
monotonicity, and smoothness. It answers "is the engine CLAP-legible?" and "which
terms are steerable?" in one pass. Two consequences:
- It **gates** downstream learning: flat or jagged curves are engine defects to fix
  *first*, with data instead of ear-guessing.
- The terms with sufficient dynamic range become the **curated steerable-term
  shortlist** — which is *why* the steerable-term list is an **output** of the POC,
  not an input. Likewise the **feature schema** is an output (discovered by the
  harness), not something to hardcode.

---

## 8. Cross-cutting invariants (don't violate silently)
- **Determinism end-to-end** (seeded probability + phase snapshot) so any
  loop/sample is reconstructable.
- **Schema versioning** on loop document, objective, sample/corpus, and feature
  schema; every artifact carries its version.
- **Forward-compatibility seam:** the controller consumes an intent object; user
  and LLM are interchangeable producers.
- **Graceful degradation:** empty objective ⇒ `drift` baseline; learner failure ⇒
  no worse than dither; weak device ⇒ deterministic controller, no LLM.
- **Safety:** `max_change_per_loop` and `clap_avoid` enforced regardless of
  mode/intent.
- **Portability:** core engine/controller/observation isolated from platform audio
  I/O behind interfaces; the same core targets desktop → iOS → Android. Language and
  framework are the implementer's choice but must not preclude this.

---

## 9. Quick index of "why" answers (for fast lookup)
- *Why does the music loop without sounding looped?* §1.1, §3.
- *Why per-loop control instead of continuous?* §1.2.
- *Why are modulators separate from instruments?* §2.1.
- *Why voice `id`s and not a voice-leading block?* §2.4, §2.5.
- *Why offsets instead of MIDI notes?* §2.6.
- *Why does fan-in need priority/voices?* §2.7.
- *Why does `pitch_walk` exist and why is it temporary?* §2.8, §6.
- *Why `loop_id` as seed + a separate phase snapshot?* §2.9, §4.3.
- *Why not let the controller chase CLAP with fixed rules?* §4.1.
- *Why feature vectors and not descriptors or raw params?* §4.2.
- *Why antithetic-pair baselines, and why non-deferrable?* §4.3.
- *Why store samples raw with provenance?* §4.4.
- *Why partial pooling?* §4.5.
- *Why RLS, and what are its limits?* §4.6.
- *Why SPSA-style perturbation?* §4.7.
- *Why is dither both cold-start and data source?* §4.8.
- *Why was the LLM dropped from the MVP?* §5.1.
- *Why goals-in-CLAP / limits-in-params?* §5.2.
- *Why is the descriptor→param table authored, not learned?* §5.3.
- *Why is the POC "a lab, not a controller"?* §7.1–7.3.
- *Why is the engine judged on legibility, not polish?* §7.4.
- *Why are the feature schema and term list outputs, not inputs?* §7.5.

---

*Companion documents: `doc/SRD_v1.1.md` (authoritative requirements),
`doc/loop_contract_v0.9.json5` (authoritative loop schema).*
