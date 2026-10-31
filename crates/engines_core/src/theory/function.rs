//! Functional harmony: tonic / subdominant / dominant / color, plus resolution tendencies
//! — the gravity *direction* that pulls dominant and borrowed chords back home.

use super::chord::{identify, Chord, ChordName, Extension, Quality, Source};
use super::pitch::{Key, Pc};
use super::scale::Scale;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Function {
    Tonic,
    Subdominant,
    Dominant,
    Color,
}

/// Harmonic function of `c` within `scale`.
pub fn function_of(c: &Chord, scale: &Scale) -> Function {
    match c.source {
        Source::Borrowed(_) => Function::Color,
        Source::SecondaryDominant(_) => Function::Dominant,
        Source::Diatonic | Source::Quartal => match c.degree.rem_euclid(scale.len() as i32) {
            0 | 2 | 5 => Function::Tonic, // I, iii, vi
            1 | 3 => Function::Subdominant, // ii, IV
            4 | 6 => Function::Dominant, // V, vii
            _ => Function::Color,
        },
    }
}

/// Plausible resolutions for `c` (the gravity gradient). Targets are diatonic sevenths.
pub fn resolution_targets(c: &Chord, scale: &Scale) -> Vec<Chord> {
    let dia = |deg: i32| Chord::diatonic(deg, Extension::Seventh);
    match c.source {
        Source::SecondaryDominant(target) => vec![dia(target)],
        Source::Borrowed(_) => vec![dia(0), dia(4)], // home, or via the dominant
        Source::Diatonic | Source::Quartal => match function_of(c, scale) {
            Function::Dominant => vec![dia(0), dia(5)], // authentic + deceptive
            Function::Subdominant => vec![dia(4), dia(0)], // → dominant or home
            Function::Tonic => vec![dia(3), dia(4), dia(5)], // depart
            Function::Color => vec![dia(0)],
        },
    }
}

/// The roman numeral of a named chord in `key`, read against the **parallel major** (the usual
/// modal analysis): D dorian runs `i ii ♭III IV v vi° ♭VII`, a borrowed ♭VI shows its flat. Case
/// follows the third (minor/diminished lowercase); `°` diminished, `ø7` half-diminished, `+`
/// augmented; extensions and tensions as in the chord symbol. Inversions aren't figured (the chord
/// label's slash shows the bass).
pub fn roman(name: &ChordName, key: &Key) -> String {
    const NUMERALS: [&str; 7] = ["I", "II", "III", "IV", "V", "VI", "VII"];
    const MAJOR: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
    let (letter, _) = key.letter_of(name.root);
    let steps = (letter as i32 - key.letter as i32).rem_euclid(7) as usize;
    let semis = (name.root.as_i32() - key.scale.root.as_i32()).rem_euclid(12);
    let acc = match (semis - MAJOR[steps] + 6).rem_euclid(12) - 6 {
        -2 => "𝄫",
        -1 => "♭",
        1 => "♯",
        2 => "𝄪",
        _ => "",
    };
    let numeral = if name.quality.is_minor() { NUMERALS[steps].to_lowercase() } else { NUMERALS[steps].to_string() };
    let suffix = match name.quality {
        Quality::Major | Quality::Minor => "",
        Quality::Dim => "°",
        Quality::Aug => "+",
        Quality::Sus2 => "sus2",
        Quality::Sus4 => "sus4",
        Quality::Power => "5",
        Quality::Maj7 => "maj7",
        Quality::Dom7 | Quality::Min7 => "7",
        Quality::MinMaj7 => "(maj7)",
        Quality::HalfDim7 => "ø7",
        Quality::Dim7 => "°7",
        Quality::Aug7 => "+7",
        Quality::AugMaj7 => "+maj7",
        Quality::Dom7Sus4 => "7sus4",
        Quality::Maj6 | Quality::Min6 => "6",
        Quality::Add9 | Quality::MinAdd9 => "add9",
        Quality::Maj9 => "maj9",
        Quality::Dom9 | Quality::Min9 => "9",
        Quality::SixNine => "6/9",
        Quality::Min11 => "11",
        Quality::Maj7Sharp11 => "maj7♯11",
    };
    let mut s = format!("{acc}{numeral}{suffix}");
    if !name.tensions.is_empty() {
        let names: Vec<&str> = name.tensions.iter().map(|&t| tension_degree(t)).collect();
        s.push_str(&format!("({})", names.join(",")));
    }
    s
}

fn tension_degree(semis: u8) -> &'static str {
    match semis {
        1 => "♭9",
        2 => "9",
        3 => "♯9",
        5 => "11",
        6 => "♯11",
        8 => "♭13",
        9 => "13",
        _ => "?",
    }
}

/// The roman numeral of an Orrery chord: a secondary dominant reads as `V7/x` of its target, anything
/// else as [`roman`] of its sounding tones.
pub fn roman_of(c: &Chord, key: &Key) -> String {
    let scale = &key.scale;
    let name_of = |c: &Chord| {
        let tones = c.tones(scale);
        let pcs: Vec<Pc> = tones.iter().map(|&t| Pc::from_offset(scale.root, t)).collect();
        identify(&pcs, pcs.first().copied())
    };
    match c.source {
        Source::SecondaryDominant(target) => {
            let ext = match c.ext {
                Extension::Triad => "",
                Extension::Seventh => "7",
                Extension::Ninth => "9",
            };
            let target = name_of(&Chord::diatonic(target, Extension::Triad)).map(|n| roman(&n, key)).unwrap_or_else(|| "?".into());
            format!("V{ext}/{target}")
        }
        _ => name_of(c).map(|n| roman(&n, key)).unwrap_or_else(|| "?".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diatonic_functions() {
        let c = Scale::from_harmony("C", "major");
        assert_eq!(function_of(&Chord::diatonic(0, Extension::Seventh), &c), Function::Tonic);
        assert_eq!(function_of(&Chord::diatonic(1, Extension::Seventh), &c), Function::Subdominant);
        assert_eq!(function_of(&Chord::diatonic(4, Extension::Seventh), &c), Function::Dominant);
        assert_eq!(function_of(&Chord::diatonic(5, Extension::Seventh), &c), Function::Tonic); // vi
    }

    #[test]
    fn secondary_dominant_resolves_to_its_target() {
        let c = Scale::from_harmony("C", "major");
        let v_of_ii = Chord { degree: 0, ext: Extension::Seventh, source: Source::SecondaryDominant(1) };
        assert_eq!(function_of(&v_of_ii, &c), Function::Dominant);
        let targets = resolution_targets(&v_of_ii, &c);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].degree, 1); // resolves to ii
    }

    #[test]
    fn roman_numerals_read_against_the_parallel_major() {
        let key = Key::new("D", "dorian").unwrap();
        let romans: Vec<String> = (0..7).map(|d| roman_of(&Chord::diatonic(d, Extension::Triad), &key)).collect();
        assert_eq!(romans.join(" "), "i ii ♭III IV v vi° ♭VII");
        let sevenths: Vec<String> = (0..7).map(|d| roman_of(&Chord::diatonic(d, Extension::Seventh), &key)).collect();
        assert_eq!(sevenths.join(" "), "i7 ii7 ♭IIImaj7 IV7 v7 viø7 ♭VIImaj7");
        let c = Key::new("C", "major").unwrap();
        let borrowed_vi = Chord { degree: 5, ext: Extension::Triad, source: Source::Borrowed(super::super::scale::Mode::Aeolian) };
        assert_eq!(roman_of(&borrowed_vi, &c), "♭VI");
        let v_of_v = Chord { degree: 0, ext: Extension::Seventh, source: Source::SecondaryDominant(4) };
        assert_eq!(roman_of(&v_of_v, &c), "V7/V");
        let v_of_ii = Chord { degree: 0, ext: Extension::Triad, source: Source::SecondaryDominant(1) };
        assert_eq!(roman_of(&v_of_ii, &c), "V/ii");
        assert_eq!(roman_of(&Chord::diatonic(2, Extension::Ninth), &c), "iii7(♭9)");
        let hm = Key::new("A", "harmonic_minor").unwrap();
        assert_eq!(roman_of(&Chord::diatonic(6, Extension::Seventh), &hm), "vii°7");
        assert_eq!(roman_of(&Chord::diatonic(2, Extension::Triad), &hm), "♭III+");
    }

    #[test]
    fn dominant_resolves_home() {
        let c = Scale::from_harmony("C", "major");
        let v = Chord::diatonic(4, Extension::Seventh);
        let t = resolution_targets(&v, &c);
        assert!(t.iter().any(|c| c.degree == 0), "V → I available");
    }
}
