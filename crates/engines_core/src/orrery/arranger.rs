//! The Arranger — Orrery's producer/composer brain (Phase 6b, ADR 0018).
//!
//! A seeded, session-stateful **energy arc** drives a single per-phrase [`ArrangePlan`]: where each
//! surface sits in register (collision-aware, octave-granular), how busy the arp is, whether the
//! accents read as a distinct high sparkle, and how the harmonic layer should build vs release. It is
//! the slow "intensity" knob a producer rides over minutes — wander + thicken + climb, then resolve +
//! thin + settle. Pure of I/O; all randomness is seeded via [`crate::hash`] so CE-R7.3 holds.

use crate::hash::{hash_site, hash_to_unit};

/// Minimum vertical gap (semitones) the Arranger keeps between the pad and the arp registers; if the
/// arp descends inside it, the pad drops an octave to make room (the "accommodate" rule).
const PAD_ARP_GAP: i32 = 5;
/// How many phrases an arp-octave choice is held before it can change again — keeps register motion
/// *sectional* (a slow form) rather than jittery phrase-to-phrase.
const OCTAVE_HOLD: u64 = 6;
/// The energy baseline the arc mean-reverts toward (calm-leaning, for sleep/focus).
const ENERGY_BASELINE: f32 = 0.45;
/// Default arp euclid-density band as fractions of the arp's authored length: pulses interpolate
/// across `[lo·len, hi·len]` by the energy/tension drive. Overridable via `Movement.arrange.arp_density`.
const DENSITY_LO_FRAC: f32 = 0.12;
const DENSITY_HI_FRAC: f32 = 0.70;

/// One phrase's arrangement decision, consumed by the Orrery writers + the controller (Phase 6b).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ArrangePlan {
    /// Octave offset (×12 semitones) applied to each surface at write time, keeping voices spaced.
    pub pad_octave: i32,
    pub bass_octave: i32,
    pub arp_octave: i32,
    pub accent_octave: i32,
    /// Euclid density (pulses) for the arp this phrase — busier when energetic/tense.
    pub arp_pulses: u32,
    /// Accents prefer upper-extension color tones (a distinct sparkle) when the texture is sparse.
    pub accent_color: bool,
    /// Producer-arc overlays on the 6a harmonic layer: the build raises the tension target
    /// (`tension_bias`) and damps the return hazards (`return_scale`); the release does the opposite.
    pub tension_bias: f32,
    pub return_scale: f32,
    /// Current energy (0..1) — telemetry.
    pub energy: f32,
}

/// The Arranger: a slow energy arc + the register/density/role rules it drives.
#[derive(Clone, Debug)]
pub struct Arranger {
    /// Slow intensity arc in 0..1 (mean-reverting random walk, one step per phrase).
    energy: f32,
    /// The held arp octave (sectional); re-chosen at most every [`OCTAVE_HOLD`] phrases.
    arp_octave: i32,
    /// Phrase index of the last arp-octave change (hysteresis).
    last_octave_change: u64,
    /// The arp modulator's authored length — bounds the density band.
    arp_len: u32,
    /// Arp euclid-density band as fractions of `arp_len` (default `DENSITY_LO_FRAC`/`DENSITY_HI_FRAC`;
    /// overridable via `Movement.arrange.arp_density`, Phase 6d).
    density_lo_frac: f32,
    density_hi_frac: f32,
}

impl Arranger {
    /// Start mid-calm with the arp at its base octave. `arp_len` is the bound for density (pulses).
    pub fn new(arp_len: u32) -> Self {
        Self {
            energy: ENERGY_BASELINE,
            arp_octave: 0,
            last_octave_change: 0,
            arp_len: arp_len.max(2),
            density_lo_frac: DENSITY_LO_FRAC,
            density_hi_frac: DENSITY_HI_FRAC,
        }
    }

    /// Override the arp euclid-density band (fractions of `arp_len`) from `Movement.arrange.arp_density`
    /// (Phase 6d). Clamped to a sane ordered range; absent ⇒ the built-in `DENSITY_*_FRAC` defaults.
    pub fn with_density_band(mut self, lo_frac: f32, hi_frac: f32) -> Self {
        self.density_lo_frac = lo_frac.clamp(0.0, 1.0);
        self.density_hi_frac = hi_frac.clamp(self.density_lo_frac, 1.0);
        self
    }

    /// Current energy (0..1) — telemetry.
    pub fn energy(&self) -> f32 {
        self.energy
    }

    /// Advance the arc one phrase and emit the plan. `tension` is the current chord tension (0..1);
    /// `phrase` seeds every draw (deterministic). `pad_center`/`arp_center` are the surfaces' base
    /// registers in semitones (from the harmonic preference), used for collision-aware re-spacing.
    pub fn advance(&mut self, phrase: u64, tension: f32, seed: u64, pad_center: i32, arp_center: i32) -> ArrangePlan {
        // 1. Energy arc — a slow, mean-reverting random walk (one step per phrase). Wide enough to
        //    visit genuinely calm and genuinely intense sections over a long session.
        let u = hash_to_unit(hash_site(seed, "energy", phrase, ""));
        let step = 0.16 * (u - 0.5);
        let reversion = 0.05 * (ENERGY_BASELINE - self.energy);
        self.energy = (self.energy + step + reversion).clamp(0.0, 1.0);

        // 2. Sectional arp octave — held ≥ OCTAVE_HOLD phrases; lifts more readily at high energy,
        //    settles lower when calm. The arp is the intentional mover (the user's rule).
        if phrase.saturating_sub(self.last_octave_change) >= OCTAVE_HOLD {
            let r = hash_to_unit(hash_site(seed, "arp_oct", phrase, ""));
            if r < 0.18 + 0.22 * self.energy {
                let up = hash_to_unit(hash_site(seed, "arp_dir", phrase, "")) < 0.4 + 0.4 * self.energy;
                let next = (self.arp_octave + if up { 1 } else { -1 }).clamp(-1, 1);
                if next != self.arp_octave {
                    self.arp_octave = next;
                    self.last_octave_change = phrase;
                }
            }
        }

        // 3. Collision-aware re-spacing: keep the arp ≥ PAD_ARP_GAP above the pad. When the arp has
        //    descended into the pad's register, drop the pad an octave to accommodate it.
        let arp_pos = arp_center + 12 * self.arp_octave;
        let pad_octave = if arp_pos < pad_center + PAD_ARP_GAP { -1 } else { 0 };
        // Accents always sit one octave above the arp (a distinct high sparkle).
        let accent_octave = self.arp_octave + 1;

        // 4. Density — busier when energetic/tense (energy-led), spanning sparse→busy within a band
        //    derived from the arp length.
        let drive = (0.65 * self.energy + 0.35 * tension).clamp(0.0, 1.0);
        let lo = (self.arp_len as f32 * self.density_lo_frac).round().max(2.0);
        let hi = (self.arp_len as f32 * self.density_hi_frac).round().max(lo + 2.0);
        let cap = (self.arp_len.saturating_sub(1)).max(1) as f32;
        let arp_pulses = (lo + (hi - lo) * drive).round().clamp(1.0, cap) as u32;

        // 5. Accent role — sparse/calm ⇒ upper-extension color tones; energetic ⇒ chord tones too.
        let accent_color = self.energy < 0.55;

        // 6. Producer arc over 6a — build raises tension + damps returns; release resolves + calms.
        let tension_bias = 0.25 * (self.energy - ENERGY_BASELINE);
        let return_scale = 1.0 - 0.5 * self.energy;

        ArrangePlan {
            pad_octave,
            bass_octave: 0,
            arp_octave: self.arp_octave,
            accent_octave,
            arp_pulses,
            accent_color,
            tension_bias,
            return_scale,
            energy: self.energy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the arc over `n` phrases at a fixed tension, returning the plans.
    fn run(seed: u64, tension: f32, n: u64) -> Vec<ArrangePlan> {
        let mut a = Arranger::new(16);
        (0..n).map(|ph| a.advance(ph, tension, seed, 7, 12)).collect()
    }

    #[test]
    fn energy_stays_bounded_and_is_deterministic() {
        let a = run(7, 0.4, 64);
        assert!(a.iter().all(|p| (0.0..=1.0).contains(&p.energy)), "energy stays in 0..1");
        assert_eq!(run(7, 0.4, 64), run(7, 0.4, 64), "same seed ⇒ identical arc");
        // the arc actually moves (not pinned at the baseline).
        assert!(a.iter().any(|p| (p.energy - ENERGY_BASELINE).abs() > 0.1), "energy explores");
    }

    #[test]
    fn density_rises_with_energy_and_tension() {
        // Compare the mean pulse count of a calm-low-tension run vs a busy-high-tension one.
        let mean = |seed: u64, tension: f32| {
            let r = run(seed, tension, 48);
            r.iter().map(|p| p.arp_pulses).sum::<u32>() as f32 / r.len() as f32
        };
        let calm = mean(3, 0.05);
        let busy = mean(3, 0.95);
        assert!(busy > calm, "denser arp when tense: busy={busy} calm={calm}");
        // pulses stay inside a sane band for a length-16 arp.
        assert!(run(3, 0.5, 32).iter().all(|p| (1..=15).contains(&p.arp_pulses)));
    }

    #[test]
    fn arp_octave_is_sectional_not_jittery() {
        // Across a long run, the arp octave changes at most once per OCTAVE_HOLD phrases.
        let r = run(9, 0.6, 60);
        let mut last_change = 0u64;
        for (ph, w) in r.windows(2).enumerate() {
            if w[0].arp_octave != w[1].arp_octave {
                assert!(ph as u64 - last_change >= OCTAVE_HOLD, "octave held ≥ {OCTAVE_HOLD} phrases");
                last_change = ph as u64;
            }
        }
        assert!(r.iter().all(|p| (-1..=1).contains(&p.arp_octave)), "arp octave bounded");
    }

    #[test]
    fn accents_sit_above_the_arp() {
        assert!(run(2, 0.5, 40).iter().all(|p| p.accent_octave == p.arp_octave + 1), "accents one octave above the arp");
    }

    #[test]
    fn pad_drops_to_accommodate_a_descended_arp() {
        // Force the arp down by passing a low arp_center so any base octave already collides with the
        // pad; the pad must drop an octave to make room.
        let mut a = Arranger::new(16);
        let mut accommodated = false;
        for ph in 0..40u64 {
            let plan = a.advance(ph, 0.5, 5, 7, 8); // arp_center 8 is within PAD_ARP_GAP of pad 7
            if plan.arp_octave <= 0 {
                assert_eq!(plan.pad_octave, -1, "pad drops beneath a low arp (phrase {ph})");
                accommodated = true;
            }
        }
        assert!(accommodated, "the scenario exercised a descended arp");
    }

    #[test]
    fn build_damps_returns_release_invites_them() {
        // return_scale < 1 and tension_bias > 0 when energy is above baseline; the inverse below it.
        let r = run(11, 0.5, 80);
        for p in &r {
            if p.energy > ENERGY_BASELINE {
                assert!(p.return_scale < 1.0 && p.tension_bias > 0.0, "build: damp returns, lift tension");
            } else if p.energy < ENERGY_BASELINE {
                assert!(p.return_scale > 0.75 && p.tension_bias < 0.0, "release: invite returns, drop tension");
            }
        }
    }
}
