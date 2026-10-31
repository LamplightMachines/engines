# ADR 0025 — Session-cumulative `max_drift` cap for drift & pursue

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** `max_change` and `max_drift` stay fractions of the normalized `0..1` range, while the doc's values are now native. For a gain (dB over −60..+6) the controller multiplies both by a drift scale of 0.3, so the defaults 0.05 and 0.15 are about 1 dB a loop and ±3 dB a session. A `-Infinity` gain never drifts. `shift` is `shift_st`; 0 st still sits at 0.5 inside.

- **Status:** accepted
- **Date:** 2026-07-01
- **Context:** the controller caps only the **per-loop** step `|δ|` (`max_change_per_loop`, CE-R8.7).
  Each loop it plays the `+δ` side and **re-bases** the operating point to it, so the operating point
  *accumulates* travel with no ceiling on cumulative displacement — over an hours-long session a param
  can wander arbitrarily far from what the author intended, bounded only by the Tier-2 safe range. The
  author wants a knob that keeps generation *near* the authored patch: a cap on total drift, distinct
  from the per-loop step. Applies to both `drift` and `pursue` (both realize their pick through the
  same clamp path).
- **Code anchors:** `crates/sme_core/src/{loop_doc.rs, controller.rs}`,
  `crates/sme_core/src/bin/{sme-run.rs, sme-play.rs}`, `crates/sme_tui/src/{app.rs, engine_link.rs}`,
  `contracts/loop_contract_v0.8.schema.json`, `doc/loop_contract_v0.8.json5`.

## Decision

Add **`max_drift`**: a per-param cap on **cumulative** displacement from the authored value over the
session. `max_change` bounds one loop; `max_drift` bounds the total. Both are authorable in the loop
doc (`max_change`, `max_drift` top-level, optional).

- **Units.** `max_drift` is a fraction of the **normalized `0..1`** range — the same units as
  `max_change` (so a default `max_drift` 0.15 is a natural 3× the default `max_change` 0.05). Robust
  for `base = 0` and signed params like `shift` (norm 0.5 at 0 semitones).
- **Default 0.15** when the doc omits it.
- **Anchor.** Each candidate param's authored (post-safe-clamp) value is captured as the **center** of
  its `±max_drift` band — parallel to how the Orrery `home` is captured — at `Controller::new` and
  re-captured on `swap_doc` (a live authored edit re-centers the band).
- **Mechanism.** A new `drift_bounds(path)` = the safe `bounds(path)` further intersected with
  `[anchor − max_drift, anchor + max_drift]`. Every drift/pursue pick clamps to it (via `room`). The
  Tier-3 constraint attractor keeps clamping to the looser safe `bounds` (via `safe_room`) so it can
  always drive an out-of-band param back in — folding the band into the shared `room` would zero the
  attractor's room and strand it.
- **Precedence (runtime > doc; param-specific > all).** An explicit runtime `--max-change` overrides
  the doc's `max_change` (resolved in the binaries: `cli.or(doc).unwrap_or(0.05)`); `max_drift` is
  doc-authored only. A param-specific objectives **`constraints`** band **replaces** that param's
  `±max_drift` band — honored **even when *wider* (less conservative)**, capped only by the safe range.
  An objectives **`override`** **re-anchors** the drift band onto the overridden value, so an override
  beyond `±max_drift` from the authored value is never fought back.
- **Tier-2 safe range stays the ultimate hard cap** — every drift bound is intersected with it.

## Consequences

- **Determinism (CE-R7.3) preserved:** anchors derive deterministically from the authored doc; the
  override/constraint timeline is part of the revision-bumped control plane. Offline re-score / harness
  replay must reconstruct the controller from the **authored doc** (then re-apply rebase + the same
  control edits), not from a mid-session operating-point snapshot.
- **Behavioral change for pursue:** pursue can no longer drive a param to a far safe bound by default —
  it is held within `±max_drift` of the authored value unless a `constraints` band widens that param.
  Three existing tests that asserted far travel (safe-floor reachability, a plateau-window probe) now
  set a wide `max_drift` to isolate the mechanic under test.
- **No `DitherConfig` change:** `max_drift` lives on the loop doc + a default const, avoiding churn to
  the many `DitherConfig` literals. A runtime `--max-drift` override is a trivial future add.
- `max_drift` is a single doc-level scalar; per-param latitude (e.g. a `safe_bounds`-style map) is a
  clean future extension.
