//! Scales & modes: degree tables (semitones from root) + scale-degree arithmetic.
//!
//! Degree tables are the ones documented in
//! `skills/shared/music-theory.md`, promoted into code.

use super::pitch::{pc_of_root, Pc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Ionian,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Aeolian,
    Locrian,
    HarmonicMinor,
    MelodicMinor,
}

impl Mode {
    /// Semitone offsets from the tonic, one octave.
    pub fn degrees(self) -> &'static [i32] {
        match self {
            Mode::Ionian => &[0, 2, 4, 5, 7, 9, 11],
            Mode::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Mode::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            Mode::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            Mode::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            Mode::Aeolian => &[0, 2, 3, 5, 7, 8, 10],
            Mode::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            Mode::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            Mode::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
        }
    }

    /// Every mode, in the order the schema lists them.
    pub const ALL: [Mode; 9] = [
        Mode::Ionian,
        Mode::Dorian,
        Mode::Phrygian,
        Mode::Lydian,
        Mode::Mixolydian,
        Mode::Aeolian,
        Mode::Locrian,
        Mode::HarmonicMinor,
        Mode::MelodicMinor,
    ];

    /// The doc spelling (`dorian`, `harmonic_minor`) — round-trips through [`Mode::try_from_name`].
    pub fn name(self) -> &'static str {
        match self {
            Mode::Ionian => "ionian",
            Mode::Dorian => "dorian",
            Mode::Phrygian => "phrygian",
            Mode::Lydian => "lydian",
            Mode::Mixolydian => "mixolydian",
            Mode::Aeolian => "aeolian",
            Mode::Locrian => "locrian",
            Mode::HarmonicMinor => "harmonic_minor",
            Mode::MelodicMinor => "melodic_minor",
        }
    }

    /// The scale-degree labels against the major scale: dorian `1 2 ♭3 4 5 6 ♭7`, lydian `… ♯4 …`,
    /// harmonic minor `1 2 ♭3 4 5 ♭6 7`.
    pub fn degree_labels(self) -> Vec<String> {
        const MAJOR: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
        self.degrees().iter().zip(MAJOR).enumerate().map(|(i, (&d, m))| format!("{}{}", accidental(d - m), i + 1)).collect()
    }

    /// The degree label of any interval `semis` above the tonic: the mode's own label when it's in the
    /// scale (locrian's 6 is `♭5`), otherwise the chromatic degree against major (`♭2 ♭3 3 ♯4 ♭6 6 ♭7 7`).
    pub fn interval_label(self, semis: i32) -> String {
        const CHROMATIC: [&str; 12] = ["1", "♭2", "2", "♭3", "3", "4", "♯4", "5", "♭6", "6", "♭7", "7"];
        let rel = semis.rem_euclid(12);
        match self.degrees().iter().position(|&d| d == rel) {
            Some(i) => self.degree_labels().swap_remove(i),
            None => CHROMATIC[rel as usize].to_string(),
        }
    }

    /// Parse a mode label (case-insensitive); `major`/`minor` alias ionian/aeolian.
    /// Unknown labels default to ionian.
    pub fn from_name(s: &str) -> Mode {
        Self::try_from_name(s).unwrap_or(Mode::Ionian)
    }

    /// [`Mode::from_name`] without the silent fallback to ionian — `None` for an unknown label (the
    /// doc checker reports it, ADR 0033).
    pub fn try_from_name(s: &str) -> Option<Mode> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "ionian" | "major" | "maj" => Mode::Ionian,
            "dorian" => Mode::Dorian,
            "phrygian" => Mode::Phrygian,
            "lydian" => Mode::Lydian,
            "mixolydian" => Mode::Mixolydian,
            "aeolian" | "minor" | "min" => Mode::Aeolian,
            "locrian" => Mode::Locrian,
            "harmonic_minor" | "harmonicminor" | "harmonic minor" => Mode::HarmonicMinor,
            "melodic_minor" | "melodicminor" | "melodic minor" => Mode::MelodicMinor,
            _ => return None,
        })
    }
}

fn accidental(semis: i32) -> &'static str {
    match semis {
        -2 => "𝄫",
        -1 => "♭",
        1 => "♯",
        2 => "𝄪",
        _ => "",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    pub root: Pc,
    pub mode: Mode,
}

impl Scale {
    pub fn new(root: Pc, mode: Mode) -> Scale {
        Scale { root, mode }
    }

    /// Build from a `harmony.root` / `harmony.mode` pair (the loop-doc fields).
    pub fn from_harmony(root: &str, mode: &str) -> Scale {
        Scale { root: pc_of_root(root), mode: Mode::from_name(mode) }
    }

    pub fn degrees(&self) -> &'static [i32] {
        self.mode.degrees()
    }
    pub fn len(&self) -> usize {
        self.degrees().len()
    }
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Semitone offset from the root for (0-based) scale `degree`, wrapping octaves: degree
    /// `7` is the octave of degree `0`; negative degrees descend.
    pub fn degree_semitone(&self, degree: i32) -> i32 {
        let n = self.len() as i32;
        let oct = degree.div_euclid(n);
        let idx = degree.rem_euclid(n) as usize;
        self.degrees()[idx] + 12 * oct
    }

    pub fn pitch_class(&self, degree: i32) -> Pc {
        Pc::from_offset(self.root, self.degree_semitone(degree))
    }

    pub fn contains(&self, pc: Pc) -> bool {
        let rel = (pc.as_i32() - self.root.as_i32()).rem_euclid(12);
        self.degrees().contains(&rel)
    }

    /// 0-based scale-degree index of `pc`, if it belongs to the scale.
    pub fn degree_of(&self, pc: Pc) -> Option<i32> {
        let rel = (pc.as_i32() - self.root.as_i32()).rem_euclid(12);
        self.degrees().iter().position(|&d| d == rel).map(|i| i as i32)
    }

    /// Nearest in-scale semitone offset (from root) to a raw `offset_st`.
    pub fn quantize(&self, offset_st: i32) -> i32 {
        let base_oct = offset_st.div_euclid(12);
        (-1..=1)
            .flat_map(|oct| {
                self.degrees().iter().map(move |&d| d + 12 * (base_oct + oct))
            })
            .min_by_key(|&cand| (cand - offset_st).abs())
            .unwrap_or(offset_st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dorian_degrees_and_wrapping() {
        let s = Scale::from_harmony("D", "dorian");
        assert_eq!(s.degrees(), &[0, 2, 3, 5, 7, 9, 10]);
        assert_eq!(s.degree_semitone(0), 0);
        assert_eq!(s.degree_semitone(7), 12); // octave up
        assert_eq!(s.degree_semitone(-1), -2); // a step below the root
        assert_eq!(s.degree_semitone(2), 3); // dorian b3
    }

    #[test]
    fn membership_and_degree_lookup() {
        let s = Scale::from_harmony("C", "major");
        assert!(s.contains(Pc(4))); // E is in C major
        assert!(!s.contains(Pc(6))); // F# is not
        assert_eq!(s.degree_of(Pc(7)), Some(4)); // G is the 5th degree (index 4)
        assert_eq!(s.degree_of(Pc(6)), None);
    }

    #[test]
    fn quantize_snaps_to_nearest_scale_tone() {
        let s = Scale::from_harmony("C", "major");
        assert_eq!(s.quantize(6), 5); // F# → F (down 1) ties toward lower
        assert_eq!(s.quantize(1), 0); // C#/Db → C
        assert_eq!(s.quantize(11), 11); // already B
        assert_eq!(s.quantize(13), 12); // C an octave up stays
    }

    #[test]
    fn mode_names_round_trip_and_degree_labels_read_against_major() {
        for m in Mode::ALL {
            assert_eq!(Mode::try_from_name(m.name()), Some(m));
        }
        assert_eq!(Mode::Dorian.degree_labels().join(" "), "1 2 ♭3 4 5 6 ♭7");
        assert_eq!(Mode::Lydian.degree_labels().join(" "), "1 2 3 ♯4 5 6 7");
        assert_eq!(Mode::Locrian.degree_labels().join(" "), "1 ♭2 ♭3 4 ♭5 ♭6 ♭7");
        assert_eq!(Mode::HarmonicMinor.degree_labels().join(" "), "1 2 ♭3 4 5 ♭6 7");
        assert_eq!(Mode::Locrian.interval_label(6), "♭5");
        assert_eq!(Mode::Dorian.interval_label(4), "3"); // out of scale: the raised 3rd
        assert_eq!(Mode::Ionian.interval_label(-2), "♭7"); // a step below the root
        assert_eq!(Mode::Mixolydian.interval_label(19), "5");
    }

    #[test]
    fn unknown_mode_defaults_ionian() {
        assert_eq!(Mode::from_name("nonsense"), Mode::Ionian);
        assert_eq!(Mode::from_name("MINOR"), Mode::Aeolian);
    }
}
