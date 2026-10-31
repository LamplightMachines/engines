//! melody_fit: a **quality-aware** harmonization score for a fixed melody against a candidate chord.
//!
//! Unlike [`super::chord_fit`] (a consonance *floor* / clash-veto for held drones), this classifies
//! each fixed pitch by its **role** over the chord — chord tone / available tension / avoid note /
//! chromatic — so it can be the *objective* when a returned-home melody (arp) leads the arrangement and
//! the chord must harmonize it (Phase 5b). The role test is quality-aware for free: a pitch a **whole
//! step above** a chord tone is an available tension (the 9, the 11 over a minor 7th, the 13), while a
//! pitch a **semitone above** a chord tone is an avoid note (the natural 11 over a major 3rd, the ♭9) —
//! and because chord tones differ by chord quality, the same melody note is correctly read as a tension
//! over one chord and an avoid note over another. Pure, deterministic, mod-12.

use super::chord::Chord;
use super::pitch::Pc;
use super::scale::Scale;

/// Harmonization score of `chord` for a fixed set of melody pitch-classes, in ~[-1, 1]. Mean over the
/// fixed pcs; `0.0` when `fixed` is empty (no melody constraint).
pub fn melody_fit(chord: &Chord, scale: &Scale, fixed: &[Pc]) -> f32 {
    if fixed.is_empty() {
        return 0.0;
    }
    let chord_pcs = chord.pitch_classes(scale);
    if chord_pcs.is_empty() {
        return 0.0;
    }
    let sum: f32 = fixed.iter().map(|&m| role(m, &chord_pcs, scale)).sum();
    sum / fixed.len() as f32
}

/// Role weight of a melody pc over a chord: chord tone +1.0 · avoid note (a semitone above any chord
/// tone) −1.0 · available tension (in-scale, a whole step above a chord tone) +0.5 · other in-scale
/// 0.0 · chromatic −0.5. The semitone-above clash dominates (checked first).
fn role(m: Pc, chord_pcs: &[Pc], scale: &Scale) -> f32 {
    if chord_pcs.contains(&m) {
        return 1.0; // chord tone
    }
    let above = |k: i32| chord_pcs.iter().any(|&c| (m.as_i32() - c.as_i32()).rem_euclid(12) == k);
    if above(1) {
        return -1.0; // a semitone above a chord tone — avoid note / clash (♭9, the natural 11 vs maj3)
    }
    if scale.contains(m) {
        if above(2) {
            0.5 // a whole step above a chord tone — available tension (9, 13, 11-over-minor)
        } else {
            0.0 // in-scale but not adjacent above a chord tone
        }
    } else {
        -0.5 // chromatic / outside the scale
    }
}

#[cfg(test)]
mod tests {
    use super::super::chord::Extension;
    use super::*;

    #[test]
    fn empty_is_neutral() {
        let s = Scale::from_harmony("C", "major");
        assert_eq!(melody_fit(&Chord::tonic(), &s, &[]), 0.0);
    }

    #[test]
    fn chord_tone_beats_tension_beats_avoid() {
        let s = Scale::from_harmony("C", "major");
        let cmaj7 = Chord::diatonic(0, Extension::Seventh); // C E G B
        let chord_tone = melody_fit(&cmaj7, &s, &[Pc(4)]); // E — chord tone
        let tension = melody_fit(&cmaj7, &s, &[Pc(2)]); // D — the 9th (whole step above C)
        let avoid = melody_fit(&cmaj7, &s, &[Pc(5)]); // F — the natural 11 (semitone above E)
        assert!(chord_tone > tension, "chord tone {chord_tone} > tension {tension}");
        assert!(tension > avoid, "tension {tension} > avoid {avoid}");
        assert!(avoid < 0.0, "avoid note is penalized: {avoid}");
    }

    #[test]
    fn quality_aware_eleventh() {
        // The natural 11 (F) is an avoid note over a major 7th but an available tension over a minor 7th.
        let maj = Scale::from_harmony("C", "major");
        let cmaj7 = Chord::diatonic(0, Extension::Seventh); // C E G B → F clashes with E
        let dor = Scale::from_harmony("C", "dorian");
        let cm7 = Chord::diatonic(0, Extension::Seventh); // C Eb G Bb → F is a whole step above Eb
        let over_maj = melody_fit(&cmaj7, &maj, &[Pc(5)]);
        let over_min = melody_fit(&cm7, &dor, &[Pc(5)]);
        assert!(over_min > over_maj, "11 is a tension over m7 ({over_min}) but avoid over maj7 ({over_maj})");
        assert_eq!(over_maj, -1.0);
        assert_eq!(over_min, 0.5);
    }
}
