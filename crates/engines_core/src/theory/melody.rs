//! Melodic generation: a seeded, scale- and chord-aware contour walk that carries and
//! develops a short motif. Output is in-scale by construction (every note is a scale degree),
//! chord-tone-anchored on strong beats, with stepwise motion in between.

use super::chord::{Chord, Source};
use super::scale::Scale;
use crate::hash::{hash_site, hash_to_unit};

#[derive(Clone, Debug)]
pub struct MelodyParams {
    /// Ambitus in scale degrees around `register`.
    pub range: i32,
    /// Probability a (weak-beat) note snaps to a chord tone — consonance, 0..1.
    pub consonance: f32,
    /// Center of the line, in scale-degree index from the root (e.g. 7 ≈ an octave up).
    pub register: i32,
    /// Bias toward ascending (>0) vs descending (<0) motion, −1..1.
    pub direction: f32,
}
impl Default for MelodyParams {
    fn default() -> Self {
        Self { range: 7, consonance: 0.6, register: 7, direction: 0.0 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Motif {
    /// Degree steps, e.g. `[1, 1, -2]`. Empty until first generated.
    pub contour: Vec<i32>,
}

/// Generate `n` melodic offsets (semitones from the root) for `chord`, deterministically
/// from `seed`. Returns the offsets and the (possibly developed) motif to carry forward.
pub fn next_phrase(
    scale: &Scale,
    chord: &Chord,
    motif: &Motif,
    p: &MelodyParams,
    seed: u64,
    n: usize,
) -> (Vec<i32>, Motif) {
    let anchors = chord_anchors(scale, chord);
    let motif = if motif.contour.len() < 2 {
        Motif { contour: seeded_contour(seed) }
    } else {
        develop(motif, seed)
    };

    let (lo, hi) = (p.register - p.range, p.register + p.range);
    let mut out = Vec::with_capacity(n);
    let mut deg = p.register;
    for i in 0..n {
        let strong = i % 2 == 0;
        let u = hash_to_unit(hash_site(seed, "mel", i as u64, ""));
        if strong || u < p.consonance {
            deg = nearest_anchor(&anchors, deg);
        } else {
            let mut step = motif.contour[i % motif.contour.len()];
            // direction bias: occasionally flip the step toward the preferred direction.
            let d = hash_to_unit(hash_site(seed, "dir", i as u64, ""));
            if d < (0.5 + 0.5 * p.direction).clamp(0.0, 1.0) {
                step = step.abs();
            } else {
                step = -step.abs();
            }
            deg += step;
        }
        deg = deg.clamp(lo, hi);
        out.push(scale.degree_semitone(deg));
    }
    (out, motif)
}

/// Chord tones expressed as scale-degree indices to anchor the melody.
fn chord_anchors(scale: &Scale, chord: &Chord) -> Vec<i32> {
    let by_degree = |step: i32| vec![chord.degree, chord.degree + step, chord.degree + 2 * step];
    let anchors = match chord.source {
        Source::Diatonic => by_degree(2),
        Source::Quartal => by_degree(3),
        _ => chord.pitch_classes(scale).iter().filter_map(|&pc| scale.degree_of(pc)).collect(),
    };
    if anchors.is_empty() {
        vec![0, 2, 4]
    } else {
        anchors
    }
}

/// Nearest octave-placement of any anchor degree to the current degree (keeps the line near
/// its register rather than collapsing toward the tonic).
fn nearest_anchor(anchors: &[i32], deg: i32) -> i32 {
    anchors
        .iter()
        .map(|&a| {
            let k = ((deg - a) as f32 / 7.0).round() as i32;
            a + 7 * k
        })
        .min_by_key(|&cand| (cand - deg).abs())
        .unwrap_or(deg)
}

fn seeded_contour(seed: u64) -> Vec<i32> {
    const STEPS: [i32; 4] = [-2, -1, 1, 2];
    (0..3)
        .map(|i| {
            let u = hash_to_unit(hash_site(seed, "contour", i, ""));
            STEPS[(u * 4.0) as usize % 4]
        })
        .collect()
}

fn develop(m: &Motif, seed: u64) -> Motif {
    let u = hash_to_unit(hash_site(seed, "develop", 0, ""));
    let mut c = m.contour.clone();
    if u < 0.5 {
        for x in c.iter_mut() {
            *x = -*x; // inversion
        }
    } else if !c.is_empty() {
        c.rotate_left(1); // fragment/rotate
    }
    Motif { contour: c }
}

#[cfg(test)]
mod tests {
    use super::super::chord::Extension;
    use super::*;

    fn dorian() -> Scale {
        Scale::from_harmony("D", "dorian")
    }

    #[test]
    fn deterministic_for_a_fixed_seed() {
        let s = dorian();
        let ch = Chord::diatonic(0, Extension::Seventh);
        let p = MelodyParams::default();
        let a = next_phrase(&s, &ch, &Motif::default(), &p, 42, 16);
        let b = next_phrase(&s, &ch, &Motif::default(), &p, 42, 16);
        assert_eq!(a.0, b.0, "same seed → identical melody");
        let c = next_phrase(&s, &ch, &Motif::default(), &p, 43, 16);
        assert_ne!(a.0, c.0, "different seed → different melody");
    }

    #[test]
    fn stays_in_scale_and_in_range() {
        let s = dorian();
        let ch = Chord::diatonic(3, Extension::Seventh);
        let p = MelodyParams { range: 5, consonance: 0.5, register: 7, direction: 0.3 };
        let (offs, _) = next_phrase(&s, &ch, &Motif::default(), &p, 7, 32);
        assert_eq!(offs.len(), 32);
        for o in &offs {
            let pc = super::super::pitch::Pc::from_offset(s.root, *o);
            assert!(s.contains(pc), "note {o} is in scale");
        }
        // bounded by the register ± range (in semitones, generously)
        for o in &offs {
            assert!(*o >= s.degree_semitone(p.register - p.range) - 1);
            assert!(*o <= s.degree_semitone(p.register + p.range) + 1);
        }
    }

    #[test]
    fn strong_beats_are_chord_tones() {
        let s = dorian();
        let ch = Chord::diatonic(0, Extension::Seventh); // Dm7
        let p = MelodyParams { consonance: 0.0, ..Default::default() };
        let (offs, _) = next_phrase(&s, &ch, &Motif::default(), &p, 5, 8);
        let chord_pcs = ch.pitch_classes(&s);
        for (i, o) in offs.iter().enumerate() {
            if i % 2 == 0 {
                let pc = super::super::pitch::Pc::from_offset(s.root, *o);
                assert!(chord_pcs.contains(&pc), "strong beat {i} (off {o}) is a chord tone");
            }
        }
    }
}
