//! Harmonic tension = the orbital radius (0 = at rest on the tonic, 1 = far out).
//!
//! Blends functional distance from the tonic, chromatic borrowing, interval roughness
//! (semitone + tritone interval classes), and — when a previous voicing is supplied — the
//! voice-leading distance to reach the chord (momentary motion adds momentary tension).

use super::chord::{Chord, Source};
use super::function::{function_of, Function};
use super::pitch::interval_class;
use super::scale::Scale;

/// 0..1 tension for `c` in `scale`. `prev` is the previous chord's offsets, if any.
pub fn tension(c: &Chord, scale: &Scale, prev: Option<&[i32]>) -> f32 {
    let func = match function_of(c, scale) {
        Function::Tonic => 0.0,
        Function::Subdominant => 0.35,
        Function::Dominant => 0.55,
        Function::Color => 0.6,
    };

    // Chromatic borrowing: a base bump for non-diatonic sources + fraction of out-of-scale tones.
    let pcs = c.pitch_classes(scale);
    let out_frac = pcs.iter().filter(|&&pc| !scale.contains(pc)).count() as f32 / pcs.len().max(1) as f32;
    let borrow = match c.source {
        Source::Diatonic | Source::Quartal => 0.0,
        _ => 0.15,
    } + 0.3 * out_frac;

    // Roughness: share of interval pairs that are a semitone (ic1) or tritone (ic6).
    let n = pcs.len();
    let (mut rough, mut pairs) = (0.0f32, 0u32);
    for i in 0..n {
        for j in (i + 1)..n {
            let ic = interval_class(pcs[i], pcs[j]);
            if ic == 1 || ic == 6 {
                rough += 1.0;
            }
            pairs += 1;
        }
    }
    let rough = if pairs > 0 { rough / pairs as f32 } else { 0.0 };

    // Voice-leading momentum (optional).
    let vl = match prev {
        Some(p) if !p.is_empty() => {
            let tones = c.tones(scale);
            let mv: i32 = tones.iter().zip(p.iter()).map(|(a, b)| (a - b).abs()).sum();
            (mv as f32 / (12.0 * tones.len().max(1) as f32)).min(1.0)
        }
        _ => 0.0,
    };

    (0.5 * func + 0.25 * borrow.min(1.0) + 0.15 * rough + 0.1 * vl).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::super::chord::{Extension, Source};
    use super::*;

    #[test]
    fn tonic_is_calmer_than_subdominant_than_secondary_dominant() {
        let c = Scale::from_harmony("C", "major");
        let tonic = tension(&Chord::diatonic(0, Extension::Seventh), &c, None);
        let subdom = tension(&Chord::diatonic(1, Extension::Seventh), &c, None);
        let v_of_v = tension(
            &Chord { degree: 0, ext: Extension::Seventh, source: Source::SecondaryDominant(4) },
            &c,
            None,
        );
        assert!(tonic < subdom, "tonic {tonic} < subdominant {subdom}");
        assert!(subdom < v_of_v, "subdominant {subdom} < secondary dominant {v_of_v}");
        assert!((0.0..=1.0).contains(&v_of_v));
    }

    #[test]
    fn voice_leading_adds_tension() {
        let c = Scale::from_harmony("C", "major");
        let ch = Chord::diatonic(3, Extension::Seventh); // IV7
        let near = tension(&ch, &c, Some(&ch.tones(&c))); // zero movement
        let far = tension(&ch, &c, Some(&[0, 0, 0, 0])); // big leap
        assert!(far > near);
    }
}
