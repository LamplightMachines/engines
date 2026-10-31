//! Gravity (the restoring force toward the tonic) + the harmonic preference.
//!
//! Gravity is what guarantees borrowed/outer-orbit chords resolve home: the inward pull on a
//! transfer grows with the chord's tension and with how long we've been away. The preference
//! (tension band, register, adventurousness) is the musical steering — re-anchored by the movement
//! axis's `tension_target` (see `doc/decisions/orrery-generative-harmony-design.md`).

/// Weights of the transfer-scoring terms (see `voyager::advance`).
#[derive(Clone, Copy, Debug)]
pub struct Gravity {
    /// Weight on the musical prior (voice-leading smoothness + resolution pull).
    pub mu: f32,
    /// Base inward pull on tension.
    pub gamma0: f32,
    /// Extra inward pull per phrase spent away from home.
    pub gamma_growth: f32,
    /// Weight pulling chord tension toward the objective's preferred band.
    pub pref_weight: f32,
    /// Penalty for repeating the current chord.
    pub repetition: f32,
    /// Weight on consonance with the fixed-pitch context (the harmonic floor, WS4.2). A **soft
    /// bias**: a clashing chord is heavily penalized but never removed from the field (no hard veto).
    pub w_fit: f32,
}

impl Default for Gravity {
    fn default() -> Self {
        Self { mu: 1.0, gamma0: 0.55, gamma_growth: 0.25, pref_weight: 0.8, repetition: 0.6, w_fit: 0.8 }
    }
}

impl Gravity {
    /// Inward-pull coefficient after `loops_since_home` phrases away.
    pub fn gamma(&self, loops_since_home: u32) -> f32 {
        self.gamma0 + self.gamma_growth * loops_since_home as f32
    }
}

/// The harmonic preference — the "solar wind".
#[derive(Clone, Copy, Debug)]
pub struct HarmonicPreference {
    /// Preferred tension band center, 0..1.
    pub tension_target: f32,
    /// Pad voicing register center, in semitones from the root.
    pub pad_center: i32,
    /// Melody register center, in scale-degree index from the root.
    pub mel_register: i32,
    /// Softmax temperature for transfer choice (higher = more adventurous wandering).
    pub adventure: f32,
}

/// The built-in pad register centre (semitones from the root): chord-role voicings are stacked
/// around it ± 6.
pub const PAD_CENTER: i32 = 7;

impl Default for HarmonicPreference {
    fn default() -> Self {
        Self { tension_target: 0.32, pad_center: PAD_CENTER, mel_register: 7, adventure: 0.35 }
    }
}

/// The pad register for a chord track whose authored home voicing has these `offsets` (ADR 0041):
/// [`PAD_CENTER`] moved by whole octaves to the octave nearest the voicing's mean, so a pad written
/// low stays low (ties go toward the authored side). A home already in the default octave keeps
/// [`PAD_CENTER`] exactly; no offsets ⇒ [`PAD_CENTER`].
pub fn pad_center_for(offsets: &[f32]) -> i32 {
    if offsets.is_empty() {
        return PAD_CENTER;
    }
    let mean = offsets.iter().sum::<f32>() / offsets.len() as f32;
    PAD_CENTER + 12 * ((mean - PAD_CENTER as f32) / 12.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_grows_with_time_away() {
        let g = Gravity::default();
        assert!(g.gamma(5) > g.gamma(0));
    }

    #[test]
    fn the_pad_register_follows_the_authored_octave() {
        assert_eq!(pad_center_for(&[]), PAD_CENTER, "no home ⇒ the default");
        assert_eq!(pad_center_for(&[-5.0, 0.0, 7.0, 14.0]), PAD_CENTER, "the house halo (mean 4) keeps C5-ish");
        assert_eq!(pad_center_for(&[0.0, 7.0, 10.0, 14.0]), PAD_CENTER);
        assert_eq!(pad_center_for(&[-17.0, -12.0, -8.0, 2.0, 7.0]), PAD_CENTER - 12, "a C3–C5 pad (mean −5.6) drops an octave");
        assert_eq!(pad_center_for(&[19.0, 24.0, 28.0]), PAD_CENTER + 12, "a high pad rises an octave");
        assert_eq!(pad_center_for(&[1.0]), PAD_CENTER - 12, "a tie (mean 1) goes toward the authored side");
        assert_eq!(pad_center_for(&[13.0]), PAD_CENTER + 12);
        assert_eq!(pad_center_for(&[-30.0]), PAD_CENTER - 36);
    }
}
