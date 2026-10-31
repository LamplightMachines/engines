# ADR 0017 — The axis seam + a generative arrangement layer (Phase 6)

- **Status:** accepted (Phase 6a); 6b/6c planned
- **Date:** 2026-06-26
- **Supersedes:** the random `enabled`-toggle orchestration in [ADR 0015](0015-orrery-dynamics-and-home.md)
  (`apply_orchestration`) and the Phase-5b `home == global` default in [ADR 0016](0016-leader-led-arrangement-chains.md).
- **Driven by:** a live-session review (drift + Orrery on `loop_orrery_roles.json`) that surfaced four
  regressions, all rooted in the two control axes overlapping on the same loop-doc leaves.
- **Plan:** `doc/plans/phase-6-axis-seam-and-arrangement.md`.

## Context — what the live session exposed

A 20-minute drift+Orrery run showed: (1) the **chord pad dropped out and never returned**; (2) toggling
Orrery off **did not return the note sequences home** and didn't restore the pad; (3) the **arp/one-shot
density never changed**; (4) the **arp and the one-shot played nearly the same sequence**.

The common cause: **the optimize axis (drift/pursue) and the movement axis (Orrery) both write the same
parts of the loop doc.** Drift's categorical layer was toggling `instruments.*.enabled` (muting the
harmonic-core pad with no restoring force) and walking the arp's `generator`/`pitch_walk.order`, while
Orrery owned the note content — so neither axis fully owned anything, `restore_home` couldn't revert a
state drift had also touched, and the random `enabled` walk had no musical logic. Separately, Orrery never
modulated rhythmic **density** (only pitch), and gave the arp and the accent surface the **same register
and pitch-class palette**, so they doubled each other.

## Decision

### 1. The seam is the JSON namespace

Each axis **exclusively owns** a disjoint set of loop-doc leaves:

| Axis | Owns | Means |
| --- | --- | --- |
| **drift / pursue** (optimize) | `instruments.*.params.*`, `matrix.*.depth`, `global.*`, `buses.*` | timbre, mix, FX |
| **Orrery** (movement) | `modulators.*` — note content (`steps`/`voicing`, `pitch_walk.{pool,order}`, `generator.{pulses,rotation}`) **and** control-LFO `wave` | harmony, density, arrangement, modulation texture |
| **neither** | `instruments.*.enabled` | stays authored — see (2) |

So drift/pursue is the **sound** axis and Orrery is the **notes + arrangement** axis, and the boundary is
enforced where candidates are *enumerated*: `categorical_candidates` no longer emits `enabled`, LFO `wave`,
`generator.pulses/rotation`, or `pitch_walk.order`; those are Orrery's. (Numeric `dither_candidates` was
already clean — instrument params, `global.*`, `matrix.*.depth`, `buses.*`.) The LFO wave-shape becomes an
Orrery orchestration-texture lever and is part of the return-home footprint.

### 2. Dropping a voice is a modulator move, never `enabled`

Orrery silences a voice by writing a **blank or release-only sequence** into its bound *note modulator* —
not by toggling `instruments.*.enabled`. This keeps Orrery strictly modulator-only (rule 1), tails the
voice out **musically** through its release envelope instead of a hard cut, and is captured by the home
snapshot for free. **Any** surface may be dropped as a tension/orchestration move *except* a **context-only**
instrument (one fed solely by `context`/`orrery_disable` modulators — the harmonic floor), which is never
dropped. Even the chord or bass may be dropped briefly as a deliberate move. *(The mechanic lands with the
narrative that drives it in Phase 6c; Phase 6a removes only the broken random muting.)*

### 3. Returns home grow with tension **and** time

The organic return-home hazard becomes `clamp(p0 + k_n·(n−dwell) + k_t·tension, 0, HAZ_MAX)` — rising with
both `loops_since_home` and the current chord tension. `home == per_modulator` is now the **default** scope
(per-track organic returns + leader-led arrangement, ADR 0016), and **global** scope gains its own organic,
tension-rising whole-voyage return (previously it resolved only on a hard `tension_max` breach). The
`tension_max` breach stays the absolute ceiling. Gravity's base inward pull (γ₀) is raised so the voyage
sits closer to home between explicit returns.

### 4. Orrery becomes a generative arranger (composer/producer mindset)

Orchestration, register placement, and density stop being incidental and become an explicit,
**probabilistic-rule** subsystem — the *Arranger* — that reasons like a producer: differentiate surface
roles, separate voices in register, vary activity over time, thin before a resolution. Encoded as seeded
rules (deterministic, CE-R7.3), however much code that takes.

## Roadmap

- **Phase 6a (this PR) — the seam + resolution.** Enforce rule 1 (subtract from the categorical candidates);
  retire the random `enabled` orchestration; extend `HomeSnapshot`/`restore_home` to the full Orrery
  footprint so Orrery-off returns *everything* home (rule 1 makes this total); strengthen returns (rule 3).
  No voice drops in 6a — the broken random one is removed, the intentional one arrives in 6c.
- **Phase 6b — the Arranger.** Per-surface **register targets that evolve over the session** with
  collision-aware re-spacing (move the arp an octave and the other surfaces re-space to keep separation);
  **arp density** driven by the voyage (Orrery owns `generator.pulses/rotation`); **accent role/register
  differentiation** (the one-shot surface takes a distinct octave and/or upper-extension color tones so it
  no longer doubles the arp).
- **Phase 6c — narrative orchestration.** The release-drop (rule 2) as a **producer's tension lever**:
  thin a prominent voice approaching a cadence/return and restore it on resolution; prominence- and
  register-aware (builds on 6b).

## Consequences

- The four regressions are fixed at the root: the pad can't be randomly muted (rule 1); Orrery-off fully
  returns home (rule 1 + the extended snapshot); density (6b) and arp/accent separation (6b) get real
  mechanisms; returns are more frequent and tension-driven (rule 3).
- **Determinism (CE-R7.3) + I/O-free core (AC3) preserved** — all changes are controller/voyager/orrery-side
  and seeded. The seam is pure subtraction from the candidate enumerators; the snapshot/return changes are
  controller-side. Antithetic ±δ (AC2) is unaffected — the optimize axis still perturbs only its owned leaves.
- **No schema bump.** Droppability and registers are *derived* (rule 2: context-only ⇒ non-droppable);
  per-track hazards use internal constants. Optional doc hints (prominence, register) may be added later as
  opt-in fields, never required.
- Behavior change: `home` now defaults to `per_modulator`; the `optional_instruments` enabled-toggle
  mechanism is removed. Docs relying on the old random orchestration get the new (quieter in 6a, then
  arranged in 6b/6c) behavior.

## Deferred / not done

The Arranger (6b) and narrative orchestration (6c) themselves; a fully independent per-surface harmonic
voyage (surfaces still derive from the one chord spine); per-track hazard/register config in the objectives
schema (internal defaults for now).
