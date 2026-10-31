//! The Orrery field: the reachable chord vocabulary for a scale, each a node with its tension
//! (orbital radius) and function. Generated from the theory core — diatonic sevenths + a small
//! curated outer shell of borrowed (modal-interchange) chords and one secondary dominant.

use crate::theory::chord::{Chord, Extension, Source};
use crate::theory::function::{function_of, Function};
use crate::theory::scale::Mode;
use crate::theory::tension::tension;
use crate::theory::Scale;

#[derive(Clone, Debug)]
pub struct FieldNode {
    pub chord: Chord,
    pub tension: f32,
    pub function: Function,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub scale: Scale,
    pub nodes: Vec<FieldNode>,
}

impl Field {
    /// Build the chord field for `scale`: the seven diatonic sevenths, plus borrowed bVI/bVII
    /// (from the parallel aeolian), a borrowed major subdominant (parallel ionian's bright IV),
    /// and V/V. Duplicate chords (a borrow that coincides with a diatonic one) are dropped.
    pub fn build(scale: Scale) -> Field {
        let mut chords: Vec<Chord> = Vec::new();
        for degree in 0..scale.len() as i32 {
            chords.push(Chord::diatonic(degree, Extension::Seventh));
        }
        // Curated outer shell (the eccentric orbits that resolve home via gravity).
        chords.push(Chord { degree: 5, ext: Extension::Seventh, source: Source::Borrowed(Mode::Aeolian) }); // bVI color
        chords.push(Chord { degree: 6, ext: Extension::Seventh, source: Source::Borrowed(Mode::Aeolian) }); // bVII color
        chords.push(Chord { degree: 3, ext: Extension::Seventh, source: Source::Borrowed(Mode::Ionian) }); // bright IV
        chords.push(Chord { degree: 0, ext: Extension::Seventh, source: Source::SecondaryDominant(4) }); // V/V

        let mut nodes: Vec<FieldNode> = Vec::new();
        for c in chords {
            // de-dup by realized pitch-class set so borrows that coincide with diatonic drop out.
            let pcs = c.pitch_classes(&scale);
            if nodes.iter().any(|n| n.chord.pitch_classes(&scale) == pcs) {
                continue;
            }
            nodes.push(FieldNode { chord: c, tension: tension(&c, &scale, None), function: function_of(&c, &scale) });
        }
        Field { scale, nodes }
    }

    /// Index of the home (tonic seventh) node.
    pub fn tonic_index(&self) -> usize {
        self.nodes
            .iter()
            .position(|n| n.chord.source == Source::Diatonic && n.chord.degree == 0)
            .unwrap_or(0)
    }

    pub fn is_tonic(&self, idx: usize) -> bool {
        idx == self.tonic_index()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_has_diatonic_plus_borrowed_shell() {
        let f = Field::build(Scale::from_harmony("D", "dorian"));
        // 7 diatonic + a few unique borrowed/secondary (some may dedup against diatonic).
        assert!(f.nodes.len() >= 8, "expected diatonic + outer shell, got {}", f.nodes.len());
        // the tonic is present and is the calmest node.
        let tonic = &f.nodes[f.tonic_index()];
        assert_eq!(tonic.function, Function::Tonic);
        let min_t = f.nodes.iter().map(|n| n.tension).fold(f32::MAX, f32::min);
        assert!((tonic.tension - min_t).abs() < 1e-6, "tonic should be (near) the lowest-tension node");
        // at least one node carries an out-of-scale (borrowed) tone.
        assert!(
            f.nodes.iter().any(|n| n.chord.pitch_classes(&f.scale).iter().any(|&pc| !f.scale.contains(pc))),
            "field should include an outer-orbit (borrowed) chord"
        );
    }
}
