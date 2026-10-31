# ADR 0011 — Two-axis control model + objectives doc (Phase 0)

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** Decision 3's Tier-2 defaults are written in native units now: `master_gain_db` −8..0 dB (was 0.4–1.0) and `cutoff_hz` 101..8000 Hz (was 0.05–1.0). `fm_feedback` stays 0..0.95, and every gain's safe ceiling is 0 dB. A doc's `safe_bounds` use the v0.9 keys and units. The schema and template named here are now `contracts/loop_contract_v0.9.schema.json` and `doc/loop_contract_v0.9.json5`.

- **Status:** accepted
- **Date:** 2026-06-25
- **Parent:** [ADR 0010](0010-runtime-control-plane-and-orrery-roadmap.md) (the multi-phase roadmap).
  This ADR records the **decisions actually implemented in Phase 0**; Phases 1–6 follow per 0010.
- **Code:** `crates/sme_core/src/{contract,controller,session,runtime,ffi,loop_doc}.rs`,
  `crates/sme_core/src/orrery/gravity.rs`, `crates/sme_core/src/bin/{sme-run,sme-play}.rs`,
  `crates/sme_tui/src/{app,ui,engine_link,main}.rs`. Schemas:
  `contracts/{loop_contract_v0.8.schema.json, objectives_v0.1.schema.json}`,
  `doc/loop_contract_v0.8.json5`.

## Context

`ControllerMode = {Drift, Hold, Pursue, Orrery}` conflated two independent choices — *how params are
optimized* and *whether the harmony is generative* — so you could not pursue a term target while
Orrery evolved the harmony. The control plane was also scattered (CLI flags + a hardcoded
`OrreryConfig::default()` + an implicit `[0,1]` guardrail), and pursue could drive `master_gain` to
`0.0` (silence) because there was no safe lower bound.

## Decisions

1. **Two orthogonal axes** replace the 4-way enum:
   - `optimize ∈ {hold, drift, pursue}` (`OptimizeMode`) — the ±δ timbre layer's pick.
   - `movement` (`Movement { orrery: bool, … }`) — the generative Orrery layer, independent.
   Any combination is legal; `optimize=pursue` + `movement.orrery=true` pursues timbre **while**
   Orrery evolves harmony. The old "Orrery auto-picks pursue-if-CLAP-else-drift" special-case is
   deleted — the user chooses both axes explicitly.

2. **`SessionControl` (objectives_v0.1)** is the runtime control plane: `optimize`, `movement`,
   `objective` (was `clap_objective`), `guardrails`, plus `constraints` (Tier-3, parsed-inert in
   Phase 0) and `overrides` (loop-doc overlay, parsed-inert in Phase 0), and a `revision` counter
   (sourced into each Sample's `objective_id` — the replay timeline). Replaces `ControlObjective`.
   `Controller::set_objective` → `set_control`; `Session::start`/`run_collect`/FFI take
   `Option<SessionControl>`. CLI: `--mode` → `--optimize` + a separate `--orrery` flag (legacy
   `mode=orrery` is rejected with a pointer to `--orrery`).

3. **Three-tier param bounds; Tier-2 "safe range" shipped.** `param_range` (Tier-1 native↔norm)
   unchanged. New `safe_range_default(path)` (Tier-2) = the native range except safety overrides
   (`master_gain→[0.4,1.0]`, `cutoff→[0.05,1.0]`, `fm_feedback→[0,0.95]`); a loop doc may override
   per-param via top-level `safe_bounds`. `Controller::bounds` now defaults to the safe range
   (guardrail floors/ceilings only *tighten* it), and `Controller::new` clamps any out-of-range
   authored value into range at start. This kills the `master_gain→0.0` bug (regression-tested).
   Tier-3 runtime constraint *attractor* is Phase 1.

4. **Loop doc v0.8.** Adds top-level `safe_bounds` (JSONPath→`{min,max}`, native units) and
   per-modulator `orrery_role` (`chord|bass|arp|oneshot|context`; consumed in Phase 4). The schema
   carries the **canonical Tier-2 default table** under `x-safe-bounds-defaults`, parity-tested
   against `safe_range_default`. A param-description audit corrected the "all 0..1 except…" blurb
   (now lists `osc_b_ratio` and bus `predelay` alongside `shift`). All fixtures bumped to `"0.8"`.

5. **`music_coca` is the default scorer** at the live entry points (TUI default, `sme-play`, FFI
   null). `fake` stays explicit and `make_scorer("")` is unchanged, so tests are unaffected.

## Consequences

- Pursue-while-Orrery is expressible and runs (validated end-to-end). The antithetic ±δ pair stays
  valid under Orrery because both sides render from the same just-evolved operating point — Orrery
  only varies the cross-loop baseline (the richer training distribution the RLS forgetting factor is
  for).
- `ControlObjective` and the single `mode` flag are gone; `control_objective_v0.1.schema.json` is
  marked deprecated (removed in Phase 6). Fixture *filenames* still say `v0.7` (content is `0.8`);
  renaming is deferred to Phase 6 to avoid churning ~15 path references.
- Tests: 167 → 172 sme_core (+ master_gain floor, safe_bounds override, v0.8 parse, schema parity,
  session-control round-trip), 10 sme_tui, all green.

## Alternatives considered

- *Keep the 4-way enum, add an `orrery: bool` beside `mode`.* Rejected — leaves `mode=orrery` as a
  dead variant and keeps the conflation in the type.
- *`MovementMode` as a tagged enum `{Static, Orrery(cfg)}`.* Rejected in favor of a `Movement` struct
  (`orrery: bool` + always-present config) — matches the objectives-doc JSON shape and is easier to
  edit/patch at runtime (Phase 1) than a tagged enum payload.
- *Safe-bounds defaults as a Rust-only table.* Rejected — the schema is the canonical table (ADR 0010
  D5) so the lab and any external author see the same defaults; the Rust table is parity-tested.
