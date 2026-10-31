# ADR 0018 — The Arranger: an energy arc that orchestrates register, density & form (Phase 6b)

- **Status:** accepted
- **Date:** 2026-06-26
- **Implements:** the Phase 6b deferral in [ADR 0017](0017-axis-seam-and-generative-arrangement.md) (the
  Arranger) plus a **producer/composer second pass over the 6a return/tension logic**.
- **Plan:** `doc/plans/phase-6-axis-seam-and-arrangement.md`.
- **Code anchors:** `crates/sme_core/src/orrery/arranger.rs` (new), `orrery/mod.rs`
  (`apply_octaves`/`write_arp_density`/accent differentiation), `orrery/voyager.rs`
  (`PhrasePlan.color_tones`), `controller.rs` (hold + advance the Arranger; energy → tension/returns),
  `telemetry.rs` + `crates/sme_tui/src/ui.rs`.

## Producer/composer second pass over 6a

Listening to 6a with an arranger's ear surfaced one structural gap and a few smaller notes:

1. **No sense of form.** 6a's returns are per-phrase memoryless hazards (time-away + tension). A
   producer arranges in *sections* — a piece **builds** (wanders, tension rises, texture thickens) and
   then **releases** (resolves home, calms, thins). 6a had no slow arc tying tension, return-rate, and
   density together. ⇒ the Arranger's **energy arc** now drives all three (below).
2. **Returns were a touch eager** with the raised γ₀ + tension term — fine when "settling", restless
   when the music should be exploring. ⇒ the return hazards are now **scaled by `(1 − 0.5·energy)`**: the
   build damps resolution, the release invites it.
3. **Static tension target.** The objective/`tension_target` set one fixed band. ⇒ the energy arc adds a
   **±0.25 bias** so the band breathes with the section.
4. Kept as-is (already musical): the bass *pedal* as anchor (rare/long-dwell hazard), the **cadence-gated**
   chord return (prepared, not a teleport), re-voicing the tonic on each return (voice-leading keeps it fresh).

## The register/role bugs the Arranger fixes (6b proper)

- **The arp and the one-shot doubled each other** — same chord tones, same octave. ⇒ accents move to a
  **distinct high register** and prefer **upper-structure color tones** (9 / #11 / 13) when the texture is
  sparse, so the sparkle reads as its own voice over the arp's chord-tone runs.
- **Density never breathed** — the euclid `pulses` (rhythm) were never touched by Orrery. ⇒ the Arranger
  now owns arp density, driving `generator.pulses` from energy + tension.
- **Registers were hardcoded** (`place_low(−17)`, `pad_center 7`, `mel_register 7`, accent center 19). ⇒
  the Arranger places voices in **evolving, collision-aware** registers.

## Decision — the Arranger

A seeded, session-stateful **`Arranger`** lives in the Orrery state and emits one `ArrangePlan` per phrase:

- **Energy arc** — a slow mean-reverting random walk in `0..1`, one step per phrase (deterministic via
  `hash_site`). It is the single "intensity" knob the producer logic reads.
- **Collision-aware registers (octave-granular).** The arp is the intentional mover: a **sectional** arp
  octave (held ≥ `OCTAVE_HOLD` phrases, lifts more readily at high energy). When the arp **descends into
  the pad**, the pad drops an octave **to accommodate** (the user's rule); accents always sit **one octave
  above the arp**. Surfaces are transposed at *write* time (`apply_octaves`) so voice-leading is preserved
  and the engine glides the octave change.
- **Density** — `arp_pulses = lerp(lo, hi, 0.55·energy + 0.45·tension)` within a band derived from the arp
  length: busier when energetic/tense, sparser when calm.
- **Accent role** — sparse/calm ⇒ upper-extension **color tones** high above; energetic ⇒ chord tones too.
- **Form overlay (the second pass)** — `tension_bias = 0.25·(energy − 0.45)` and `return_scale =
  1 − 0.5·energy`, applied to the voyage's tension target and the return hazards.

## Integration (minimal-churn)

The Voyager keeps producing plans in its **canonical** registers; the Arranger's octave offsets, density,
and accent placement are applied by the **writers** (`apply_octaves`, `write_arp_density`, the widened
`write_oneshots`), and the energy overlay is applied controller-side to `pref.tension_target` and the
hazard draws. So the Voyager's signatures barely change (only `PhrasePlan` gains `color_tones`).

## Consequences

- The arp and accents are differentiated (register + color); density and harmonic tension **breathe with a
  slow form**; registers evolve and re-space without collisions. Determinism (CE-R7.3) + I/O-free core
  preserved (the Arranger is seeded, controller/orrery-side). The axis seam (ADR 0017) holds — the Arranger
  writes only `modulators.*` (note content + density).
- Telemetry gains `energy` + `arp_pulses` (TUI Orrery panel). No schema bump (internal constants; an
  optional per-surface register/density hint in the doc is a future opt-in).

## Deferred / not done

Phase **6c** narrative orchestration (the release-drop as a tension lever) still pending. Per-surface
*independent* harmonic voyages remain out of scope. Doc-level Arranger config (register/density hints,
energy-arc shape) uses internal defaults for now.
