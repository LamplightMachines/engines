//! Chord construction. A chord is rooted on a scale degree and built by stacking thirds
//! *within a scale* (so its quality follows the mode automatically), with diatonic,
//! borrowed (modal-interchange), secondary-dominant, and quartal sources. Everything is
//! returned as semitone offsets from the working scale's root.

use super::pitch::{Key, Pc};
use super::scale::{Mode, Scale};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extension {
    Triad,
    Seventh,
    Ninth,
}

impl Extension {
    /// Number of stacked tones.
    pub fn count(self) -> usize {
        match self {
            Extension::Triad => 3,
            Extension::Seventh => 4,
            Extension::Ninth => 5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Stack scale thirds on `degree` in the working scale.
    Diatonic,
    /// Stack thirds on `degree` in a parallel mode (same tonic) — modal interchange.
    Borrowed(Mode),
    /// The dominant chord that resolves *to* the diatonic chord on the target scale degree
    /// (root a fifth above that degree's root).
    SecondaryDominant(i32),
    /// Stack diatonic fourths on `degree` (ambient, third-less).
    Quartal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    /// Scale degree the chord is rooted on (0 = tonic).
    pub degree: i32,
    pub ext: Extension,
    pub source: Source,
}

impl Chord {
    pub fn diatonic(degree: i32, ext: Extension) -> Chord {
        Chord { degree, ext, source: Source::Diatonic }
    }
    /// The home chord: a tonic seventh.
    pub fn tonic() -> Chord {
        Chord::diatonic(0, Extension::Seventh)
    }

    /// Chord tones as semitone offsets from the scale root, root-position (lowest first).
    pub fn tones(&self, scale: &Scale) -> Vec<i32> {
        match self.source {
            Source::Diatonic => self.stacked(scale, 2),
            Source::Quartal => self.stacked(scale, 3),
            Source::Borrowed(mode) => {
                let s = Scale::new(scale.root, mode);
                Chord { source: Source::Diatonic, ..*self }.stacked(&s, 2)
            }
            Source::SecondaryDominant(target) => {
                let root = scale.degree_semitone(target) + 7; // a fifth above the target
                const DOM: [i32; 5] = [0, 4, 7, 10, 14]; // dominant: maj3 / P5 / m7 / 9
                DOM.iter().take(self.ext.count()).map(|&iv| root + iv).collect()
            }
        }
    }

    /// Stack `ext` tones stepping `step` scale degrees each (2 = thirds, 3 = fourths).
    fn stacked(&self, scale: &Scale, step: i32) -> Vec<i32> {
        (0..self.ext.count() as i32)
            .map(|i| scale.degree_semitone(self.degree + i * step))
            .collect()
    }

    pub fn root_pc(&self, scale: &Scale) -> Pc {
        Pc::from_offset(scale.root, self.tones(scale).first().copied().unwrap_or(0))
    }

    pub fn pitch_classes(&self, scale: &Scale) -> Vec<Pc> {
        let mut pcs: Vec<Pc> = self.tones(scale).iter().map(|&t| Pc::from_offset(scale.root, t)).collect();
        pcs.dedup();
        pcs
    }

    /// The chord symbol for telemetry/logs (`Dm9`, `Gmaj7`, `C#m7b5`), spelled in the scale's key —
    /// [`identify`] on the chord's tones with its own root as the bass.
    pub fn label(&self, scale: &Scale) -> String {
        let tones = self.tones(scale);
        let Some(&first) = tones.first() else { return "?".into() };
        let pcs: Vec<Pc> = tones.iter().map(|&t| Pc::from_offset(scale.root, t)).collect();
        let key = Key::from_scale(*scale);
        let root = Pc::from_offset(scale.root, first);
        match identify(&pcs, Some(root)) {
            Some(name) => name.label(&key),
            None => key.spell(root),
        }
    }
}

/// A chord quality: its intervals above the root and its symbol suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Major,
    Minor,
    Dim,
    Aug,
    Sus2,
    Sus4,
    Power,
    Maj7,
    Dom7,
    Min7,
    MinMaj7,
    HalfDim7,
    Dim7,
    Aug7,
    AugMaj7,
    Dom7Sus4,
    Maj6,
    Min6,
    Add9,
    MinAdd9,
    Maj9,
    Dom9,
    Min9,
    SixNine,
    Min11,
    Maj7Sharp11,
}

impl Quality {
    /// In preference order: when one pitch-class set names two chords (Am7 = C6), the earlier wins
    /// (unless the bass says otherwise).
    pub const ALL: [Quality; 26] = [
        Quality::Major,
        Quality::Minor,
        Quality::Dom7,
        Quality::Min7,
        Quality::Maj7,
        Quality::HalfDim7,
        Quality::Dim,
        Quality::Dim7,
        Quality::Sus4,
        Quality::Sus2,
        Quality::Dom7Sus4,
        Quality::Aug,
        Quality::Maj9,
        Quality::Min9,
        Quality::Dom9,
        Quality::Add9,
        Quality::MinAdd9,
        Quality::Maj6,
        Quality::Min6,
        Quality::SixNine,
        Quality::MinMaj7,
        Quality::Aug7,
        Quality::AugMaj7,
        Quality::Min11,
        Quality::Maj7Sharp11,
        Quality::Power,
    ];

    /// Semitones above the root (ascending, within an octave).
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Quality::Major => &[0, 4, 7],
            Quality::Minor => &[0, 3, 7],
            Quality::Dim => &[0, 3, 6],
            Quality::Aug => &[0, 4, 8],
            Quality::Sus2 => &[0, 2, 7],
            Quality::Sus4 => &[0, 5, 7],
            Quality::Power => &[0, 7],
            Quality::Maj7 => &[0, 4, 7, 11],
            Quality::Dom7 => &[0, 4, 7, 10],
            Quality::Min7 => &[0, 3, 7, 10],
            Quality::MinMaj7 => &[0, 3, 7, 11],
            Quality::HalfDim7 => &[0, 3, 6, 10],
            Quality::Dim7 => &[0, 3, 6, 9],
            Quality::Aug7 => &[0, 4, 8, 10],
            Quality::AugMaj7 => &[0, 4, 8, 11],
            Quality::Dom7Sus4 => &[0, 5, 7, 10],
            Quality::Maj6 => &[0, 4, 7, 9],
            Quality::Min6 => &[0, 3, 7, 9],
            Quality::Add9 => &[0, 2, 4, 7],
            Quality::MinAdd9 => &[0, 2, 3, 7],
            Quality::Maj9 => &[0, 2, 4, 7, 11],
            Quality::Dom9 => &[0, 2, 4, 7, 10],
            Quality::Min9 => &[0, 2, 3, 7, 10],
            Quality::SixNine => &[0, 2, 4, 7, 9],
            Quality::Min11 => &[0, 2, 3, 5, 7, 10],
            Quality::Maj7Sharp11 => &[0, 4, 6, 7, 11],
        }
    }

    /// The lead-sheet suffix (ASCII accidentals, like the note names): `m7b5`, `maj9`, `7sus4`.
    pub fn suffix(self) -> &'static str {
        match self {
            Quality::Major => "",
            Quality::Minor => "m",
            Quality::Dim => "dim",
            Quality::Aug => "aug",
            Quality::Sus2 => "sus2",
            Quality::Sus4 => "sus4",
            Quality::Power => "5",
            Quality::Maj7 => "maj7",
            Quality::Dom7 => "7",
            Quality::Min7 => "m7",
            Quality::MinMaj7 => "m(maj7)",
            Quality::HalfDim7 => "m7b5",
            Quality::Dim7 => "dim7",
            Quality::Aug7 => "aug7",
            Quality::AugMaj7 => "maj7#5",
            Quality::Dom7Sus4 => "7sus4",
            Quality::Maj6 => "6",
            Quality::Min6 => "m6",
            Quality::Add9 => "add9",
            Quality::MinAdd9 => "m(add9)",
            Quality::Maj9 => "maj9",
            Quality::Dom9 => "9",
            Quality::Min9 => "m9",
            Quality::SixNine => "6/9",
            Quality::Min11 => "m11",
            Quality::Maj7Sharp11 => "maj7#11",
        }
    }

    /// Minor-family (a minor third, no major third): lowercase in roman numerals.
    pub fn is_minor(self) -> bool {
        let iv = self.intervals();
        iv.contains(&3) && !iv.contains(&4)
    }

    fn mask(self) -> u16 {
        self.intervals().iter().fold(0, |m, &i| m | 1 << i)
    }
}

/// A chord named from its pitch classes: root + quality, the tensions over it that the quality
/// doesn't account for, an omitted fifth, and a bass when it isn't the root (an inversion).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChordName {
    pub root: Pc,
    pub quality: Quality,
    /// Extra tones above the root (semitones, ascending): `1` = ♭9, `2` = 9, `6` = ♯11, `9` = 13…
    pub tensions: Vec<u8>,
    /// The quality's fifth isn't sounding (a common voicing: C7 as C E B♭).
    pub no_fifth: bool,
    /// The lowest note when it isn't the root — written as a slash chord.
    pub bass: Option<Pc>,
}

impl ChordName {
    /// `Dm7`, `C7(b9)`, `Fmaj7/A`, `G7(no5)`.
    pub fn label(&self, key: &Key) -> String {
        let mut s = format!("{}{}", key.spell(self.root), self.quality.suffix());
        if !self.tensions.is_empty() {
            let t: Vec<&str> = self.tensions.iter().map(|&t| tension_name(t)).collect();
            s.push_str(&format!("({})", t.join(",")));
        }
        if self.no_fifth {
            s.push_str("(no5)");
        }
        if let Some(b) = self.bass {
            s.push_str(&format!("/{}", key.spell(b)));
        }
        s
    }
}

fn tension_name(semis: u8) -> &'static str {
    match semis {
        1 => "b9",
        2 => "9",
        3 => "#9",
        4 => "3",
        5 => "11",
        6 => "#11",
        7 => "5",
        8 => "b13",
        9 => "13",
        10 => "b7",
        11 => "maj7",
        _ => "?",
    }
}

/// Name the chord a set of pitch classes spells. Each sounding pitch class is tried as the root; an
/// exact quality match or the same with its fifth omitted wins over the largest quality that fits
/// with the rest named as tensions (`Em7(b9)`). Among equals the root that is the `bass` (the lowest
/// note, when known) wins, then the exact match, then the quality's preference order
/// ([`Quality::ALL`]), then the earlier pitch class in `pcs`. `None` for fewer than two distinct pitch classes or nothing that reads as a
/// chord.
pub fn identify(pcs: &[Pc], bass: Option<Pc>) -> Option<ChordName> {
    let mut uniq: Vec<Pc> = Vec::new();
    for &p in pcs {
        if !uniq.contains(&p) {
            uniq.push(p);
        }
    }
    if uniq.len() < 2 {
        return None;
    }
    // (tension reading, bass isn't root, tier, quality rank, root index) — lower is better.
    let mut best: Option<((u8, u8, u8, usize, usize), ChordName)> = None;
    for (ri, &root) in uniq.iter().enumerate() {
        let rel: u16 = uniq.iter().fold(0, |m, p| m | 1 << (p.as_i32() - root.as_i32()).rem_euclid(12));
        for (qi, &q) in Quality::ALL.iter().enumerate() {
            let qm = q.mask();
            let fifth = q.intervals().iter().copied().find(|&i| (6..=8).contains(&i));
            let (tier, no_fifth, tensions) = if rel == qm {
                (0u8, false, 0u16)
            } else if q.intervals().len() >= 3 && fifth.is_some_and(|f| rel | 1 << f == qm && rel & 1 << f == 0) {
                (1, true, 0)
            } else if qm & rel == qm && q.intervals().len() >= 3 && q != Quality::Min11 {
                // The quality fits inside; the leftovers are tensions. Prefer the biggest quality.
                (2 + (6 - q.intervals().len() as u8), false, rel & !qm)
            } else {
                continue;
            };
            // A whole-chord reading (exact or fifth-less) beats a tension reading; among those, the
            // bass as root beats a slash chord (D G C over D is D7sus4, not Gsus4/D).
            let rank = (u8::from(tier >= 2), u8::from(bass.is_some_and(|b| b != root)), tier, qi, ri);
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                let tensions = (1..12u8).filter(|&i| tensions & 1 << i != 0).collect();
                let slash = bass.filter(|&b| b != root);
                best = Some((rank, ChordName { root, quality: q, tensions, no_fifth, bass: slash }));
            }
        }
    }
    best.map(|(_, n)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diatonic_seventh_follows_mode() {
        let c = Scale::from_harmony("C", "major");
        // I7 in C major = Cmaj7: C E G B → offsets 0 4 7 11
        assert_eq!(Chord::diatonic(0, Extension::Seventh).tones(&c), vec![0, 4, 7, 11]);
        // ii7 = Dm7: D F A C → 2 5 9 12
        assert_eq!(Chord::diatonic(1, Extension::Seventh).tones(&c), vec![2, 5, 9, 12]);
        // V7 = G7 (dominant) → 7 11 14 17
        assert_eq!(Chord::diatonic(4, Extension::Seventh).tones(&c), vec![7, 11, 14, 17]);
    }

    #[test]
    fn dorian_tonic_is_minor() {
        let d = Scale::from_harmony("D", "dorian");
        // i7 in D dorian = Dm7: root D, b3, 5, b7 → 0 3 7 10
        let t = Chord::diatonic(0, Extension::Seventh);
        assert_eq!(t.tones(&d), vec![0, 3, 7, 10]);
        assert_eq!(t.label(&d), "Dm7");
    }

    #[test]
    fn secondary_dominant_is_major_with_chromatic_third() {
        let c = Scale::from_harmony("C", "major");
        // V/V resolving to V (G): root a fifth above G = D, major 3rd F# (out of scale)
        let v_of_v = Chord { degree: 0, ext: Extension::Seventh, source: Source::SecondaryDominant(4) };
        let pcs = v_of_v.pitch_classes(&c);
        assert!(pcs.contains(&Pc(2))); // D root
        assert!(pcs.contains(&Pc(6))); // F# (the secondary leading tone, out of C major)
        assert!(!c.contains(Pc(6)));
    }

    #[test]
    fn borrowed_changes_color() {
        let c = Scale::from_harmony("C", "major");
        // borrow the iv from C aeolian (minor iv = Fm): F Ab C
        let borrowed_iv = Chord { degree: 3, ext: Extension::Triad, source: Source::Borrowed(Mode::Aeolian) };
        let pcs = borrowed_iv.pitch_classes(&c);
        assert!(pcs.contains(&Pc(8))); // Ab — the borrowed color, out of C major
        assert!(!c.contains(Pc(8)));
    }

    #[test]
    fn labels_name_half_diminished_and_major_ninths_correctly() {
        let c = Scale::from_harmony("C", "major");
        // vii7 in C major is half-diminished (B D F A), not "dim7"
        assert_eq!(Chord::diatonic(6, Extension::Seventh).label(&c), "Bm7b5");
        // I9 is a major ninth (C E G B D), not "C9"; V9 is the dominant ninth
        assert_eq!(Chord::diatonic(0, Extension::Ninth).label(&c), "Cmaj9");
        assert_eq!(Chord::diatonic(4, Extension::Ninth).label(&c), "G9");
        assert_eq!(Chord::diatonic(1, Extension::Ninth).label(&c), "Dm9");
        // iii9 carries a ♭9 over its m7
        assert_eq!(Chord::diatonic(2, Extension::Ninth).label(&c), "Em7(b9)");
        // a real diminished seventh (vii7 of harmonic minor)
        let a = Scale::from_harmony("A", "harmonic_minor");
        assert_eq!(Chord::diatonic(6, Extension::Seventh).label(&a), "G#dim7");
        // idle's A mixolydian iii7 — logged as "C#dim7" before the fix
        let mixo = Scale::from_harmony("A", "mixolydian");
        assert_eq!(Chord::diatonic(2, Extension::Seventh).label(&mixo), "C#m7b5");
        // flats where the key has them
        let f = Scale::from_harmony("F", "major");
        assert_eq!(Chord::diatonic(3, Extension::Seventh).label(&f), "Bbmaj7");
    }

    #[test]
    fn identify_names_chords_inversions_and_voicings() {
        let key = Key::new("C", "major").unwrap();
        let id = |pcs: &[u8], bass: Option<u8>| identify(&pcs.iter().map(|&p| Pc(p)).collect::<Vec<_>>(), bass.map(Pc)).map(|n| n.label(&key));
        assert_eq!(id(&[0, 4, 7], None).as_deref(), Some("C"));
        assert_eq!(id(&[4, 7, 0], Some(4)).as_deref(), Some("C/E"));
        assert_eq!(id(&[9, 0, 4, 7], Some(9)).as_deref(), Some("Am7"));
        assert_eq!(id(&[0, 4, 7, 9], Some(0)).as_deref(), Some("C6")); // the bass decides Am7 vs C6
        assert_eq!(id(&[0, 4, 10], Some(0)).as_deref(), Some("C7(no5)"));
        assert_eq!(id(&[2, 7, 0], Some(2)).as_deref(), Some("D7sus4(no5)"));
        assert_eq!(id(&[0, 7], None).as_deref(), Some("C5"));
        assert_eq!(id(&[0, 4, 7, 10, 1], Some(0)).as_deref(), Some("C7(b9)"));
        assert_eq!(id(&[0, 4, 7, 11, 2, 6], Some(0)).as_deref(), Some("Cmaj9(#11)"));
        assert_eq!(id(&[11, 2, 5, 8], None).as_deref(), Some("Bdim7"));
        assert_eq!(id(&[2, 5, 8, 11], Some(5)).as_deref(), Some("Fdim7")); // symmetric: the bass names it
        assert_eq!(id(&[0], None), None);
        assert_eq!(id(&[0, 1], None), None); // a cluster isn't a chord
    }

    #[test]
    fn quartal_has_no_third() {
        let d = Scale::from_harmony("D", "dorian");
        let q = Chord { degree: 0, ext: Extension::Triad, source: Source::Quartal };
        // stack diatonic 4ths from D: D G C → 0 5 10
        assert_eq!(q.tones(&d), vec![0, 5, 10]);
    }
}
