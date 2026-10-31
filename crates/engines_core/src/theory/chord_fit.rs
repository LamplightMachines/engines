//! Chord-fit: how well a chord agrees with a set of sustained "fixed" pitch-classes — a held
//! drone, or any `orrery_disable` / `orrery_role: context` note modulator. The Orrery uses this
//! to bias its voyage toward chords consonant with pitches it cannot rewrite, so an excursion
//! never fights an uncontrollable drone (ADR 0010 §6 / D1). Pure, deterministic, mod-12.

use super::chord::Chord;
use super::pitch::{interval_class, Pc};
use super::scale::Scale;

/// Consonance of `chord` against the sustained `fixed` pitch-classes, normalized to ~[-1, 1].
/// Each fixed pc is scored by its **smallest** interval to any chord tone and the scores are
/// averaged, so the term is independent of the chord size and the number of fixed pitches.
/// Returns `0.0` when `fixed` is empty — no floor ⇒ no bias (the voyage is unconstrained).
pub fn chord_fit(chord: &Chord, scale: &Scale, fixed: &[Pc]) -> f32 {
    if fixed.is_empty() {
        return 0.0;
    }
    let chord_pcs = chord.pitch_classes(scale);
    if chord_pcs.is_empty() {
        return 0.0;
    }
    let sum: f32 = fixed
        .iter()
        .map(|&f| {
            let ic = chord_pcs.iter().map(|&c| interval_class(f, c)).min().unwrap_or(6);
            consonance(ic)
        })
        .sum();
    sum / fixed.len() as f32
}

/// Consonance weight of an interval class (`0..=6` semitones): a shared tone is ideal, perfect
/// and imperfect consonances are good, a whole tone is neutral, and the semitone / tritone clash.
fn consonance(ic: u8) -> f32 {
    match ic {
        0 => 1.0,     // shared tone (the fixed pitch *is* a chord tone)
        5 => 0.6,     // perfect fourth / fifth
        3 | 4 => 0.4, // thirds / sixths
        2 => 0.0,     // whole tone — neutral
        6 => -0.7,    // tritone
        1 => -1.0,    // semitone clash (m2 / M7)
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::super::chord::Extension;
    use super::*;

    fn dorian() -> Scale {
        Scale::from_harmony("D", "dorian")
    }

    #[test]
    fn empty_fixed_is_neutral() {
        let s = dorian();
        assert_eq!(chord_fit(&Chord::tonic(), &s, &[]), 0.0);
    }

    #[test]
    fn shared_tone_beats_a_semitone_clash() {
        let s = dorian(); // root D = pc 2
        let tonic = Chord::diatonic(0, Extension::Seventh); // Dm7 = D F A C (pcs 2,5,9,0)
        // A held A (pc 9) is a chord tone of Dm7 → high fit.
        let consonant = chord_fit(&tonic, &s, &[Pc(9)]);
        // A held G# (pc 8) is a semitone off both A and G → clash → low fit.
        let clashing = chord_fit(&tonic, &s, &[Pc(8)]);
        assert!(consonant > 0.5, "shared tone scores high: {consonant}");
        assert!(clashing < 0.0, "semitone clash scores negative: {clashing}");
        assert!(consonant > clashing);
    }

    #[test]
    fn fit_is_bounded_and_averages() {
        let s = dorian();
        let c = Chord::tonic();
        for deg in 0..7 {
            let f = chord_fit(&Chord::diatonic(deg, Extension::Seventh), &s, &[Pc(2), Pc(9)]);
            assert!((-1.0..=1.0).contains(&f), "fit in range for degree {deg}: {f}");
        }
        let _ = c;
    }
}
