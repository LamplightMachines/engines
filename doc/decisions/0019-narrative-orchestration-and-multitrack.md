# ADR 0019 — Narrative orchestration (the drop) + multi-track role binding (Phase 6c)

- **Status:** accepted
- **Date:** 2026-06-26
- **Implements:** the Phase 6c deferral in [ADR 0017](0017-axis-seam-and-generative-arrangement.md) (the
  release-drop as a tension lever) and the multi-track binding the richer demo docs need.
- **Plan:** `doc/plans/phase-6-axis-seam-and-arrangement.md`.
- **Code anchors:** `crates/sme_core/src/orrery/mod.rs` (`drop_surface`, `OrreryConfig.{chord_mods,
  arp_mods,extra_*}`, looped writers), `controller.rs` (`pick_drop`, `role_mods`, drop in `orrery_step`),
  telemetry/TUI (`dropped`). Demo: `sandbox/loop_focus_a_dorian.json`.

## Decision

### Narrative drop (the producer's "thin before the drop")

At a **tension peak** — energy ≥ `DROP_ENERGY_MIN`, away from home, **no leader**, **no resolution
pending** — a prominent voice is silenced for one phrase so the texture breaks down before the return
lands; it re-enters on the next phrase's normal write (the resolution). The decision is a seeded per-phrase
hazard `clamp(p0 + k·(energy − min), 0, max)` (`pick_drop`), choosing among the bound prominent roles
(`chord`/`arp`), excluding any track that just returned home.

The drop is a **modulator move, never `instruments.enabled`** (the ADR 0017 seam): `drop_surface` writes a
**release-only step** to a step-voiced track (chord/bass voices tail out via their release envelope) or
zeroes a euclid arp's `pulses`. So it is captured by the home snapshot and reverts on Orrery-off for free,
and it tails out *musically* rather than hard-cutting.

### Multi-track role binding

`orrery_role` is no longer one-modulator-per-role for chords/arps. `OrreryConfig` keeps a primary
`chord_mod`/`arp_mod` plus `extra_chord_mods`/`extra_arp_mods`; `orrery_config` collects **all** role-tagged
mods (`role_mods`), and every writer (`write_phrase`/`vary_arp_pool`/`apply_octaves`/`write_arp_density`)
and `track_modulators` loops them. Each additional same-role track is **octave-staggered by index** in
`apply_octaves`, so two pads/keys and two arps layer the harmony in distinct registers. `bass` stays single.

## Why these choices

- **Release, not mute** (user-confirmed, ADR 0017 Q3): keeps Orrery modulator-only and lets a dropped voice
  decay naturally — essential for a musical breakdown rather than a click.
- **Gate on peak + no-leader + no-resolution**: the drop *precedes* a resolution (high tension ⇒ the
  tension-coupled return is near), so the re-entry lands on the cadence — the breakdown-then-drop arc. It
  never fights the leader-led arrangement (Phase 5b) or thins the resolution itself.
- **Octave-stagger by index**: a deterministic, voice-leading-preserving way to spread layered tracks
  without per-mod register config; the engine glides the octave like any other Arranger move.

## Consequences

- Two chord tracks + multiple arps are all Orrery-driven (the demo doc layers a Dual pad + Modal vibe and a
  Dual pluck + Modal bell). The texture **breaks down before returns** and re-enters on the resolution.
- Determinism (CE-R7.3) preserved (seeded `pick_drop`/`drop_surface`); the axis seam holds (drops + extra
  tracks are `modulators.*` only); Orrery-off reverts every track (the widened snapshot already covers
  them). Telemetry/TUI gain `dropped` (panel `⊘drop chord|arp`). No schema bump.
- **Sandbox demo** `sandbox/loop_focus_a_dorian.json` (not a test) exercises every Phase 6 feature in an
  electronic-focus style: four-on-floor + busy hats + backbeat over a context sub-drone, with the full
  Arranger driving the layered harmony, interlocking arps, density, registers, and drops.

## Deferred / not done

Per-arp **independent density** (all arps currently share the Arranger's pulse count, differing by
rotation/rate/register/timbre). Dropping the **bass** (allowed by the seam but excluded from `pick_drop`'s
prominence set for now — a bass drop is a stronger, rarer move). Doc-level drop/prominence config (internal
constants for now).
