# ADR 0010 — Runtime control plane, decoupled optimize×movement axes, Orrery v2 (roadmap)

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** `safe_bounds`, constraints and overrides are written in the param's own unit under its v0.9 key. §5.2's Tier-2 overrides are now `master_gain_db` −8..0 dB (was 0.4–1.0) and `cutoff_hz` 101..8000 Hz (was 0.05–1.0); §5.1's constraint is `global.master_gain_db` −6.02..0 dB (was 0.5–1.0) and its override is `shift_st: -12`. `max_change_per_loop` stays a fraction of the normalized range, scaled by 0.3 for a gain (0.05 is about 1 dB a loop).

- **Status:** proposed (master plan; each phase gets its own detailed plan/ADR as it is built)
- **Date:** 2026-06-25
- **Supersedes/sharpens:** the single `ControllerMode` enum (`contract.rs`), the hardcoded
  `OrreryConfig::default()` (`controller.rs`), and the implicit `[0,1]` guardrail default.
- **Related:** `doc/SRD_core_engine_v0.1.md` (CE-R8 controller/objective/learner, CE-R11 streaming,
  CE-R12 shell), ADR `0003` (Rust core is I/O-free / no wall-clock / no watching — the constraint
  that puts *all* file-watching in the shell), ADR `0005` (param-range seam), ADR `0007`
  (sme-tui live explorer) + ADR `0008` (decoupled scoring), `doc/decisions/orrery-generative-harmony-design.md`.
  Code anchors: `crates/sme_core/src/{contract,controller,session,learner,loop_doc,observe}.rs`,
  `crates/sme_core/src/orrery/*`, `crates/sme_core/src/theory/*`, `crates/sme_tui/src/*`.

> This is the **master plan** for the work between the merged synth-types epoch and the mobile-app
> pivot. It records the architectural decisions and a phased roadmap; it does **not** itself change
> code. The detailed plan for the *next* phase is written separately, against this.

## 1. Motivation — the shortcomings being addressed

The eight requests cluster into five themes:

1. **Optimization is welded to musical movement.** `ControllerMode = {Drift,Hold,Pursue,Orrery}`
   makes Orrery a peer of the optimizers, so you cannot pursue a term target *while* Orrery evolves
   the harmony. Decouple into two orthogonal axes.
2. **Stats and pursuit aren't legibly "whole-song."** The TUI's term *score* column flickers (raw
   per-loop score); pursue never converges (`controller.rs` always steps `signum·max_change`); the
   displayed model and the controlling model are two different things.
3. **Orrery is shallow and fragile.** It only rewrites pad voicing / one bass note / the arp pool,
   binds to **hardcoded modulator ids**, ignores read-only (`orrery_disable`) voices, can't drive
   one-hits or vary pool length, and has no controls for sparseness, max tension, or "home."
4. **No runtime control surface.** Mode, objective, Orrery on/off, per-param constraints, and
   live loop-doc tweaks can't change while a session runs.
5. **No safe operating bounds, plus accumulated tech debt.** The controller can drive `master_gain`
   to 0.0; schemas need a 0.8/objectives-v0.1 consolidation; deprecated paths + RT debt remain.

## 2. Resolved design forks (decisions taken 2026-06-25)

- **D1 — Orrery generation: chord-as-orbital-body, with all note modulators as harmonic context.**
  Keep the existing field/gravity model (chord = body, tension = radius, gravity → home); derive
  bass / arp / one-hit as **surfaces of the chosen chord**. *But:* **every** note-sequence modulator
  — including `orrery_disable` ones — is harvested into the **effective theory model** as a read-only
  *fixed-pitch context*. The field's reachable/preferred chords are filtered & biased by **consonance
  with those fixed pitches** (e.g. a drone-D pad steers Orrery toward chords that fit a held D).
  `orrery_disable` thus means "don't *rewrite* me," not "ignore me."
- **D2 — Pursue: RLS primary + SNR/convergence gate on top.** The φ-generalizing RLS (with
  forgetting, CE-R8.5) still chooses *direction*; a new **explore→exploit gate** decides *which*
  param to work and *when to stop*: explore until a param clears a per-(param,term) SNR gate for the
  target term, then exploit it until the objective error shrinks or its SNR decays — bounded by the
  tier-2 safe range. The TUI surfaces **both** the RLS predicted gradient (direction + reason) and
  the empirical SNR (confidence) + the gate state.
- **D3 — Runtime edits: patch/overlay, persisted to the watched objectives doc.** "Modify the
  effective loop doc at runtime" = a JSONPath→value **overlay** merged onto the controller's evolving
  operating point at the next loop boundary. The **single watched objectives doc is the source of
  truth**; TUI edits write to it, and engine-initiated changes round-trip back into it (guarded by a
  revision counter so the shell ignores its own writes).

### 2.1 Additional decisions (second pass, 2026-06-25)

- **D4 — `music_coca` is the default scorer** at the live entry points (TUI default selection,
  `sme-play --scorer` default, FFI null default). `fake` stays the explicit synthetic scorer, and the
  `make_scorer("")` mapping is **unchanged** so unit tests that rely on planted structure are
  unaffected. When the `music_coca` manifest/weights are absent, the existing helpful `make_scorer`
  error stands (don't silently fall back — surface it).
- **D5 — the loop doc v0.8 schema is the canonical Tier-2 safe-bounds table.** Every steerable
  param's default `{min,max}` (native units) lives in the schema as a *complete* table; the engine's
  `safe_range()` defaults are asserted-against it (a parity test), and a doc may override per param
  via `safe_bounds`.
- **D6 — a param-description audit ships with v0.8.** Every "all 0..1 except…" blanket and per-param
  range note is corrected. Concretely: the schema's top-level description (line ~5) lists only
  `shift` and omits `osc_b_ratio` (native 0.5..12) and the bus `predelay` (ms); each safe-bounds entry
  states its unit.
- **D7 — phase reorder:** the TUI control + log views move up to **Phase 3** (right after runtime,
  telemetry, and pursue) so the whole session is observable/drivable before the deeper Orrery work.

## 3. Architectural commitments

- **AC1 — Separate "what to play" from "how to drive it."**
  - *Loop doc v0.8* = musical material (+ per-param safe bounds, + per-modulator Orrery roles).
  - *Objectives/control doc v0.1* = the runtime control plane (optimize axis, movement axis + Orrery
    config, term objectives, constraint bands, loop-doc overlay, guardrails).
- **AC2 — Two orthogonal axes** replace the 4-way enum:
  `optimize ∈ {hold, drift, pursue}` × `movement ∈ {static, orrery(cfg)}`. Any combination is legal.
  The session's `reads_learner` decision (`session.rs`) becomes
  `optimize==pursue || (movement==orrery && objective has CLAP terms)`.
  *Why the antithetic pair stays valid under Orrery+pursue:* the ±δ pair is rendered from the **same
  just-evolved operating point** each loop, so per-loop ΔCLAP attribution to the param δ is clean;
  Orrery only varies the cross-loop **baseline** — the richer training distribution the RLS forgetting
  factor is meant for.
- **AC3 — The core stays I/O-free (ADR 0003).** All file creation/watching lives in the **shell/TUI**.
  The core gains exactly one new control entry point, `Session::apply_control(SessionControl)`, applied
  **atomically at the loop boundary** with **re-base** (move from the current operating point, never
  jump) and an incremented `objective_id` (CE-R8.2, CE-R11.2). A read-only **telemetry** channel flows
  the other way (operating point, learner/SNR snapshot, Orrery state, constraint status, perturbation
  log) — the TUI reads ground truth instead of reconstructing it.
- **AC4 — Determinism under edits (CE-R7.3 preserved).** Each per-loop render stays bit-reproducible
  given (operating point + snapshot). A *session* is reproducible given (seed + the **control-edit
  timeline**); since edits round-trip through the objectives doc, that doc's revision history keyed by
  the loop_id it took effect *is* the timeline (mirror it into the corpus/provenance).

## 4. Three-tier param-bounds model

Untangles items 5 and 7 from the existing normalization seam. A param is governed by, in order:

| Tier | Name | Defined in | Behavior |
|---|---|---|---|
| 1 | **Native range** (`param_range`) | code (`loop_doc.rs`) | unit↔normalize only (shift ±24, predelay 0–100); unchanged |
| 2 | **Safe range** | **canonical default table in the loop doc v0.8 schema** (per-doc override via `safe_bounds`) | hard clamp the controller never exceeds; replaces the implicit `[0,1]` (fixes master_gain→0) |
| 3 | **Constraint band** | objectives doc (runtime) | *attractor*: if outside `[min,max]`, perturb toward it at ≤ `max_change`, with priority over drift/pursue for that param until satisfied |

Today's `guardrails.floors/ceilings` collapse into Tier 2. Tier 3 is new (the "drive into range" behavior).

## 5. Schemas

### 5.1 Objectives / control doc `objectives_v0.1` (new, watched)

JSONPath-keyed and native-unit-at-the-boundary, consistent with v0.7 conventions:

```json5
{
  schema_version: "objectives_v0.1",
  revision: 7,                         // ++ on every applied change (self-write detection + replay)
  optimize: "pursue",                  // hold | drift | pursue
  movement: {
    orrery: true,                      // false => static
    bind: { chord: "chords", bass: "bass_line", arp: "arp_shimmer", oneshot: ["event"] }, // by id OR by modulator orrery_role
    phrase_loops: 4,
    tension_target: 0.32,
    tension_max: 0.70,                 // hard: exceeded => GLOBAL return home
    voice_changes_per_loop: { min: 0.5, max: 3.0 },   // 4-loop moving-average band (governor)
    home: "global",                    // global | per_modulator
  },
  objective: [ { term: "mc:peaceful", weight: 0.3 } ],
  constraints: {                       // Tier-3 attractor bands, native units
    "global.master_gain": { min: 0.5, max: 1.0 },
  },
  overrides: {                         // live loop-doc overlay (JSONPath -> native value)
    "instruments.mono.params.shift": -12,
  },
  guardrails: { max_change_per_loop: 0.05, clap_avoid: [ { term: "mc:harsh", threshold: 0.7 } ] },
}
```

### 5.2 Loop doc `v0.8` deltas (additive where possible)

- **`safe_bounds`** (top-level, JSONPath→`{min,max}`, native units): Tier-2 ranges. The **schema
  carries the complete canonical default table** (D5) — every steerable param's default min/max; a
  doc only authors `safe_bounds` to *override*. Params stay scalars (no per-param object rewrite).
  Defaults: most params = their native `param_range`; the safety-relevant overrides are
  `global.master_gain` → `[0.4, 1.0]`, instrument `cutoff` → `[0.05, 1.0]`, `fm_feedback` → `[0, 0.95]`
  (full table in the Phase 0 plan). Ship the **param-description audit** (D6) in the same change.
- **`modulators.<id>.orrery_role`** ∈ `{chord, bass, arp, oneshot, context}`: replaces the hardcoded
  id binding so Orrery binds by *role*, not by demo ids. `orrery_disable: true` continues to mean
  "don't rewrite," and is read as `context` regardless.
- Bump `schema_version` → `0.8`; update `contracts/loop_contract_v0.8.schema.json` +
  `doc/loop_contract_v0.8.json5`; migrate fixtures.

## 6. Orrery v2

- **Effective theory model from the whole doc.** Build the field for the scale, then harvest the
  **fixed-pitch context** = the sounding pitch-classes of every note modulator (authored content for
  managed ones at "home," live content for `context`/`orrery_disable` ones). Add a theory primitive
  `chord_fit(chord, fixed_pcs)` and fold it into `voyager::advance` scoring (a reward for consonance
  with fixed pitches, a penalty for clashes) so excursions never fight an uncontrollable drone.
- **Derived surfaces (item 3).** After the chord is chosen, generate, in order: **bass** (already),
  **arp** (scale/chord-locked `pitch_walk` pool *and* generated `step_seq`s), and **one-hits**
  (`oneshot`-role event modulators get chord-tone accents placed by a seeded generator — replacing
  the `enabled`-toggle stub). Pools become variable in **offset and length** (length = density).
- **Movement governor (item 4).** Track a 4-loop moving average of voice-changes-per-loop; hold it in
  `voice_changes_per_loop.[min,max]` by modulating phrase rate + how many voices move per phrase
  (pedal/common-tone retention vs. full re-voice).
- **Tension + home (item 4).** `tension_max` is a **hard** trigger → **global** return to home;
  intermediate **per-modulator** returns are allowed (a single sequence resolves while others roam).
  **"Home" = the authored loop-doc content** (per modulator), captured at session start — not merely
  the tonic. Implies the Voyager generalizes from one shared traversal to **per-track tension/home**
  with a **global tension aggregate** driving the global snap-home.

## 7. Pursue v2 (per D2)

- **Direction:** unchanged RLS argmax of `Σ weightᵢ · predict(φ, termᵢ)` (`controller.rs::pursue_pick`).
- **Gate (new):** a per-(param,term) cumulative t-statistic (the `analytics.rs` Welford, promoted into
  the core as authoritative). **Explore** (drift fallback, memoryless) until a candidate clears the SNR
  gate for a target term; **exploit** that param until the objective error shrinks below a band *or*
  its SNR decays below the gate; **stop/clamp** at the Tier-2 safe range. Degrade-no-worse-than-drift
  (CE-R8.7) preserved.
- **Telemetry:** emit RLS predicted gradient per candidate + the empirical SNR + the gate state, so the
  TUI shows *direction and reason*.

## 8. Telemetry + TUI

- **New read-only telemetry** (AC3): operating point, learner/SNR snapshot (RLS predictions + empirical
  SNR), Orrery state (current chord/tension/loops-since-home/voice-change avg), constraint status, and a
  perturbation/score event stream.
- **TUI views:** (a) Params/Terms gain RLS columns (item 2 / D2); (b) a **log view** — perturbations
  (loop_id, ts, param, δ / categorical / harmonic move) + a per-term **top-score leaderboard** (item 6);
  (c) a **control editor** that reads/writes the objectives doc (item 5 / D3); (d) live mode/movement
  toggles.

## 9. Phased roadmap

| Phase | Theme | Delivers | Exit criteria |
|---|---|---|---|
| **0** | Schema + control-plane foundation | loop doc **v0.8** + objectives **v0.1**; split `optimize`×`movement`; 3-tier bounds w/ safe defaults | all modes/movements expressible; master_gain can't be driven below its safe floor; fixtures migrated; green |
| **1** | Runtime control + telemetry | `apply_control` atomic swap/re-base; shell+TUI file-watch (echo-guarded); Tier-3 constraint attractor; read-only telemetry | edit objectives doc mid-run → applied at next loop, re-based, recorded; TUI reads ground truth |
| **2** | Pursue v2 + whole-song stats | RLS+SNR/convergence gate; promote stats into core; TUI RLS+SNR columns; fix score flicker | pursue converges & stops; displayed model == controlling model; deterministic pursue trajectory retained |
| **3** | TUI control + log views | control editor (reads/writes objectives doc); perturbation/top-score log; constraint & Orrery state surfaced | full session drivable + observable from the TUI |
| **4** | Orrery decoupled + enriched | role-bound config; pursue-with-Orrery live; fixed-pitch context model; bass→chord→arp→one-hit; variable pools | Orrery respects `orrery_disable` drones; drives all 4 surfaces; works on non-demo docs |
| **5** | Orrery dynamics + home | voice-change governor; max-tension global return; per-modulator authored "home" + intermediate returns | sparseness held in band; bounded tension; verified global + single-sequence returns |
| **6** | Cleanup + solidify | remove deprecated paths; fix the **ADR 0007 number collision** (done: send-bus → 0020); RT String-key debt; gain-staging; finalize schemas/contracts/ADRs | no deprecated code; schemas frozen; docs current — done as **Phase 6d** (ADR 0021) |

Phase 0 is the keystone (small, everything leans on it). The TUI (Phase 3) lands as soon as
telemetry (Phase 1) + pursue stats (Phase 2) exist, so the whole session is observable and drivable
before the deeper Orrery work (Phases 4–5). Detailed per-phase plans live in `doc/plans/`.

## 10. Out of scope (unchanged from SRD §12 / core SRD §13)

LLM layer, mobile UI, modulator→modulator references, automatable connection depth, on-device CLAP
quantization. Faster-than-real-time rendering remains a non-goal for the POC.

## 11. Deferred to per-phase plans

- Exact SNR-gate thresholds, exploit-stop band, and explore/exploit dwell policy (Phase 2).
- `chord_fit` weighting + how strongly fixed-pitch context vetoes vs. biases (**Phase 4** — resolved in
  `doc/plans/phase-4-orrery-decoupled-and-enriched.md`: soft bias by default, no hard veto).
- Governor control law + tension_max hysteresis to avoid snap-home oscillation (**Phase 5** — specified
  in `doc/plans/phase-5-orrery-dynamics-and-home.md`: proportional nudge + `ε` re-arm band).
- Objectives-doc echo-guard mechanism (revision vs. content hash) + corpus control-timeline format (Phase 1).
- Whether Tier-2 safe bounds also clamp matrix-modulated values or only controller perturbations.
