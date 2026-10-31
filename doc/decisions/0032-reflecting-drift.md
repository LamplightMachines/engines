# ADR 0032 — Drift reflects off band edges; freezing a lever is explicit

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** bands are written in the param's unit. The amendment's default ranges are `level_db` −26..0 dB (was 0.05–1) and `master_gain_db` −8..0 dB (was 0.4–1), and a freeze is a zero-width band in dB (v0.8's `0.2` is `-13.98`). `max_change` and `max_drift` stay fractions, scaled by 0.3 for a gain. D5's mute is now `level_db: -Infinity`: it never drifts, and a route that adds to it is inaudible, so `sme-check` warns and asks for a finite floor and a `depth_db`.

Status: accepted (built 2026-10-09)
Relates to: ADR 0025 (session-cumulative `max_drift`), ADR 0031 (learner removal — this fixes a ±δ-era
vestige it left in place on purpose so P1a could stay bit-identical), ADR 0010 / 0011 (Tier-2 safe
ranges, `safe_bounds`).

## Context

Drift walks one param per loop by a seeded step `δ` (≤ `max_change`, with directional momentum), inside a
band: the Tier-2 safe range ∩ guardrails ∩ the cumulative `±max_drift` band around the authored anchor
(or a param-specific `constraints` band, which replaces it). Under the learner, every step had to be an
**antithetic pair** `base ± δ`, so `δ` was clamped *symmetrically*: `|δ| ≤ min(ceil − base, base − floor)`.
Two consequences outlived the learner:

1. A param that reached a band edge had zero room **both ways** and was pinned there for the rest of the
   session (or block) — the walk could never come back.
2. Any param *near* an edge had its steps shrunk toward the nearer edge's distance, in both directions.

House-style docs leaned on (1) deliberately: capping a level at its authored value (`safe_bounds: { max:
<authored> }`) "pinned" it, which kept hours of drift from getting louder.

## Decision

**D1 — A reflecting bounded walk.** `controller::reflect_step(base, want, [lo, hi])`: the target is
`base + want`; a step that would cross an edge bounces back by its overshoot (`2·hi − t` / `2·lo − t`,
clamped for a step longer than the band); a base already outside the band steps `|want|` back toward it.
When the walk turns, the drift streak adopts the new direction, so momentum carries the param away from
the edge instead of grinding against it. The Tier-3 constraint attractor clamps its step to the safe
bounds (no symmetric shrink).

**D2 — Freezing is explicit: a zero-width band.** `min == max` pins a lever (`reflect_step` doesn't move
it). A plain `max` at the authored value now means "never louder, may breathe below" (up to `max_drift`
under the cap). The tracked house-style fixtures (`loop_engine_drone_a_mixolydian`, `song_idle_night`,
`song_glass_garden`, the skill's `reference/house/idle.json5`) now freeze their level-ish levers and
`master_gain` explicitly (each `max` was verified equal to its authored value), and the skill's recipe
teaches `min == max` — with "breathe" (a plain `max`) as an option to ask about.

**D3 — Categorical moves keep only the played side** (already true after ADR 0031): a flip records
`from → to` and plays `to`.

## Consequences

- Renders of docs whose params touch a band edge change. Golden: only `session/drift+orrery/idle x3`
  moved (re-blessed); engine-direct, hold, stems, the template drift run and the short song are unchanged.
- Freezing the fixture levers reproduces their pinned levels, but not the old near-edge step shrink (2),
  which the fix removes on purpose.
- The published `idle-night-v2` (caps slightly *above* its levels) was never strictly frozen; under
  reflection its levels wander within their bands instead of sticking at the caps. Re-rendering it will
  not reproduce the published audio bit-for-bit (it already couldn't — the doc was edited after that render).

## Amendment (2026-10-09) — a default range bounds drift, not the author

Found live: an instrument set to `level: 0` stayed faintly audible. `Controller::new` (and `swap_doc`)
clamped every authored value into its Tier-2 range — including the **canonical default** table
(`level` 0.05–1, `master_gain` 0.4–1, …) — so a mute played at 0.05 (−26 dB), and in drift a level-0
voice could also wander up by `max_drift`.

- **D4 — An authored value outside a default range is kept.** The enforced range is now
  `controller::tier2_norm`: each side of an explicit doc `safe_bounds` band is still a hard clamp
  (ADR 0010); a side left to the default table widens to include the authored value (the drift
  anchor), so the controller never moves a param past where the author put it but doesn't override
  the author either. `bounds()` / drift / the attractor all use it.
- **D5 — `level: 0` is a mute.** An instrument level authored exactly 0 is not a drift candidate
  (unless an explicit `safe_bounds` min > 0 overrules it — then it's clamped up as before). A matrix
  route that *adds* to a muted level still makes it sound; `sme-check` warns and points to
  `enabled: false`, which the controller never touches.
- Docs authoring below a default floor without a band now play as authored (quieter, never louder).
  Goldens unchanged (every tracked doc authors gain levers inside a band or inside the defaults).
