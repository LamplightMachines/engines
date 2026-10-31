# ADR 0021 — Phase 6d: cleanup & solidify (+ pursue directed exploration)

- **Status:** accepted
- **Date:** 2026-06-26
- **Context:** the original Phase 6 in the [ADR 0010](0010-runtime-control-plane-and-orrery-roadmap.md)
  roadmap (row 6) was *"Cleanup + solidify"* — remove deprecated paths, fix the ADR 0007 number
  collision, close gain-staging, finalize schemas/contracts/ADRs. The 6a–6c work (axis seam / Arranger
  / orchestration, ADRs 0017–0019) displaced it; this ADR records doing it as **Phase 6d**, plus the
  loose phase 1–5b deferrals that were either done here or moved to a tracked backlog, plus a real
  **pursue-mode bug** a live review surfaced.
- **Plan:** the Phase 6d plan (this branch).
- **Code anchors:** `doc/decisions/0020-send-bus-effects-fundsp-graph.md` (renumbered),
  `contracts/{loop_contract_v0.8.schema.json, objectives_v0.1.schema.json}`,
  `lab/tests/fixtures/loop_contract_v0.8.json`, `crates/sme_core/src/{controller.rs, contract.rs,
  engine.rs, orrery/arranger.rs, telemetry.rs, session.rs}`, `crates/sme_tui/src/{app.rs, ui.rs}`.

## Decisions

### 1. ADR 0007 number collision → send-bus renumbered to 0020 (WS6d.1)
Two ADRs claimed 0007. CLAUDE.md and the live cross-refs cite the **sme-tui live explorer** as 0007, so
the **send-bus / effects** ADR was renumbered `0007 → 0020` (next free) via `git mv`, with its header
noting the renumber. Live pointers (PLAN.md, ADR 0009's precedent line, ADR 0010's roadmap row, the
phase-2 plan note) were repointed; historical sme-tui 0007 references are unchanged.

### 2. Deprecated `control_objective_v0.1` schema removed (WS6d.2)
It was marked `deprecated` (ADR 0010), superseded by `objectives_v0.1` (`SessionControl`), with removal
scheduled for Phase 6. Nothing in code/tests/lab/make loaded it; deleted. The `objectives_v0.1`
"supersedes" note and the historical ADR 0004/0011 + phase-0 mentions stay as record.

### 3. Loop schema finalized to v0.8 (WS6d.3)
The rendered fixture `lab/tests/fixtures/loop_contract_v0.7.json` was already `schema_version 0.8`; the
name lagged (ADR 0011 deferred the ~15-path churn). Renamed → `loop_contract_v0.8.json`, and the
**redundant v0.7 artifacts retired**: the v0.7 JSON Schema (canonical is `loop_contract_v0.8.schema.json`,
which the Rust parity test already validates against) and the superseded `doc/loop_contract_v0.7.json5`
(v0.8.json5 is its superset). All live references repointed to v0.8 — the 8 Rust loaders, the bins, the
Makefile, the 3 Python lab tests, `lab/clap/export.py`, the READMEs, the SRD/design-rationale pointers,
and the ambient-loop-doc skill. Only historical ADRs (0004/0009/0011) keep their as-of-then v0.7 names.

### 4. Gain-staging: closed — no master limiter, by design (WS6d.4)
The engine intentionally ships **no master limiter** (`mix · master_gain` straight to output). A limiter
would *dynamically colour the very audio the CLAP path scores*, contaminating ΔCLAP and the corpus; that
path already peak-normalizes its own scoring window. Headroom is the doc author's responsibility
(per-voice `drive` soft-clips; `master_gain` sits on a Tier-2 safe floor so the controller can't mute).
A note at the gain path + `engine::tests::playback_fixtures_do_not_hard_clip` pin the contract for the
**auditioned** docs (showcase/orrery, all 0.71–0.98 FS; sandbox 0.85). The feature-demonstration /
correctness rigs (`loop_contract_v0.8` ~4.47×, `loop_contract_ambient` ~2.29×, `loop_modulation_smoke`)
deliberately stack voices and run hot — tracked as **content cleanup**, not a limiter bug.

### 5. Two deferred knobs exposed (WS6d.5) — additive, default-off, byte-identical when absent
- **`returning_home` telemetry** (ADR 0015 deferral): the controller flags the phrases where a homeward
  return *lands* — a per-track rest, or the chord voyage reaching home (breach snap / resolved cadence,
  detected via `loops_since_home` hitting 0, since `want_return` is only the hazard draw). Surfaced on
  `OrreryView`/`OrreryTelemetry` and as `⌂returning-home` in the TUI.
- **`Movement.arrange`** (ADR 0014/0016 deferral): an optional `objectives_v0.1` block exposing the
  Arranger's arp euclid-density band (`arp_density`, a fraction-of-length `Range01`, default 0.12..0.70)
  and a global `return_hazard_scale` (default 1.0). Every field defaults to the built-in constant, so an
  absent/partial block renders byte-identically (CE-R7.3). The full per-track hazard table stays internal.

### 6. TUI structured constraint / override editors (WS6d.6) — ADR 0013 deferral
The inline objective editor generalized to a kind-dispatched `EditField` (Objective | Constraint |
Override); new live keys `c`/`v` alongside `e`, each parsing into `control.{constraints,overrides}` and
reusing the existing `apply_edit` write-back (revision bump + echo-guarded watched-file write). Reusable
parsers `parse_constraint`/`parse_override` live next to `parse_objective`; the panel lists overrides.

### 7. Pursue directed exploration — bug fix (WS6d.8)
A live review found pursue **ignored the high-SNR params the TUI surfaces** for a freshly-selected
objective term and kept probing random params. The TUI's per-`(path,term)` SNR equals the pursue gate's
empirical SNR by construction (both Welford-accumulate `ΔCLAP/(2δ)` from the same sample stream), so SNR
was *not* the blocker. The deadlock was between two existing behaviors: the gate's **Explore** state
probed via **uniform-random drift**, while the **Explore→Exploit** transition (`entry_candidate`)
additionally requires the φ-generalized **per-term RLS** to agree in sign — and random probing rarely
gave the new term's RLS the *targeted* probes on those exact params it needs to agree. Fix: `explore_pick`
makes Explore **directed** — it ranks unpinned candidates by their best per-term empirical SNR and probes
the top-k (clearing `STEERABLE_Z`, looser than the gate's `ENTRY_Z`) in the empirically-favorable
direction, building the evidence the gate needs. It falls back to the exact prior `drift_pick` with no
evidence (degrade-no-worse-than-drift, CE-R8.7) and reads only the deterministic scored history +
seed/loop_id, so the pursue trajectory stays bit-reproducible (CE-R7.3).

## Consequences
- One ADR number per file; no schema marked `deprecated`; one loop-schema generation (v0.8) with one
  annotated source + one JSON Schema. `rg loop_contract_v0\.7` / `rg control_objective_v0.1` return only
  historical-ADR mentions.
- The runtime-control / Orrery epoch (Phases 1–6) is **complete**; the `objectives_v0.1` and
  `loop_contract_v0.8` schemas are **frozen** for the POC (the only change here was *additive*: the
  optional `arrange` block — no version bump).
- The remaining tech debt is consolidated in **PLAN.md → "Deferred backlog"** with source ADRs — most
  notably the RT String-key hot-path refactor (split out of 6d with its own benchmarking), gate-threshold
  tuning vs real CLAP, and the hot feature-fixture re-gain-staging.
