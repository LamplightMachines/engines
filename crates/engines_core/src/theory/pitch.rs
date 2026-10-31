//! Pitch-class & interval math (mod 12), and key-aware note spelling ([`Key`], [`note_name`]).

use super::scale::{Mode, Scale};

/// A pitch class, `0..11` with C = 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pc(pub u8);

impl Pc {
    /// Wrap any integer into a pitch class.
    pub fn new(v: i32) -> Pc {
        Pc(v.rem_euclid(12) as u8)
    }
    /// The pitch class `offset_st` semitones above `root`.
    pub fn from_offset(root: Pc, offset_st: i32) -> Pc {
        Pc::new(root.0 as i32 + offset_st)
    }
    pub fn as_i32(self) -> i32 {
        self.0 as i32
    }
}

/// Pitch class of a note-name root (reuses the engine's canonical mapping so the two can't
/// drift — [`crate::voice::root_semitone`]).
pub fn pc_of_root(root: &str) -> Pc {
    Pc::new(crate::voice::root_semitone(root) as i32)
}

/// Sharp-spelled note name for a pitch class (display/labels only).
pub fn pc_name(pc: Pc) -> &'static str {
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    N[pc.0 as usize]
}

/// The letter names, `0..7` = C D E F G A B.
const LETTERS: [char; 7] = ['C', 'D', 'E', 'F', 'G', 'A', 'B'];
/// The natural pitch class of each letter.
const NATURAL: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
/// Letter steps above the tonic for each chromatic interval, spelled the usual way against the
/// major scale: 1 ♭2 2 ♭3 3 4 ♯4 5 ♭6 6 ♭7 7.
const CHROMATIC_STEPS: [u8; 12] = [0, 1, 1, 2, 2, 3, 3, 4, 5, 5, 6, 6];

/// A key's **spelling** context: the scale plus the tonic's letter, so `C#` dorian spells with
/// sharps and `Db` dorian with flats. In-scale notes take one letter per scale degree (D dorian:
/// D E F G A B C); out-of-scale notes are spelled as the chromatic degree against the major scale
/// (♭2 ♭3 ♯4 ♭6 ♭7 — D dorian's raised 3rd is F#, its ♭6 is Bb). Display only: the engine works in
/// pitch classes and semitone offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub scale: Scale,
    /// The tonic's letter, `0..7` = C..B.
    pub letter: u8,
}

impl Key {
    /// From a doc's `harmony.root` / `harmony.mode` (`None` when either doesn't parse). The root's own
    /// spelling (`C#` vs `Db`) picks the letters; a lowercase natural (`d`) is accepted like the engine.
    pub fn new(root: &str, mode: &str) -> Option<Key> {
        let pc = Pc::new(crate::voice::try_root_semitone(root)? as i32);
        let mode = Mode::try_from_name(mode)?;
        let letter = LETTERS.iter().position(|&l| root.starts_with(l) || root.starts_with(l.to_ascii_lowercase()))? as u8;
        Some(Key { scale: Scale::new(pc, mode), letter })
    }

    /// A key with only its pitch-class root known: of the tonic's enharmonic spellings, the one whose
    /// scale needs the fewest accidentals (Eb major, not D#; C# minor, not Db), sharps on a tie.
    pub fn from_scale(scale: Scale) -> Key {
        let candidates = (0u8..7).filter(|&l| {
            let acc = (scale.root.as_i32() - NATURAL[l as usize] + 6).rem_euclid(12) - 6;
            acc.abs() <= 1
        });
        let cost = |k: &Key| -> (i32, i32) {
            let accs: Vec<i32> = (0..scale.len() as i32).map(|d| k.accidental_of_degree(d)).collect();
            (accs.iter().map(|a| a.abs()).sum(), i32::from(accs.iter().any(|&a| a < 0)))
        };
        candidates.map(|letter| Key { scale, letter }).min_by_key(cost).unwrap_or(Key { scale, letter: 0 })
    }

    /// `D dorian`, `Bb harmonic minor`.
    pub fn name(&self) -> String {
        format!("{} {}", self.spell(self.scale.root), self.scale.mode.name().replace('_', " "))
    }

    /// The letter steps (above the tonic's letter) and the semitones above the tonic for `pc`.
    fn steps_of(&self, pc: Pc) -> (u8, i32) {
        let rel = (pc.as_i32() - self.scale.root.as_i32()).rem_euclid(12);
        let steps = match self.scale.degree_of(pc) {
            Some(d) if self.scale.len() == 7 => d as u8,
            _ => CHROMATIC_STEPS[rel as usize],
        };
        (steps, rel)
    }

    /// The accidental (−2..=2) of the note `rel` semitones above the tonic, written on the letter
    /// `steps` above the tonic's.
    fn accidental(&self, steps: u8, rel: i32) -> (u8, i32) {
        let letter = (self.letter + steps) % 7;
        let acc = (self.scale.root.as_i32() + rel - NATURAL[letter as usize] + 6).rem_euclid(12) - 6;
        (letter, acc)
    }

    fn accidental_of_degree(&self, degree: i32) -> i32 {
        let rel = self.scale.degree_semitone(degree).rem_euclid(12);
        self.accidental(degree as u8, rel).1
    }

    /// The letter (0..7) and accidental (−2..=2) spelling `pc` in this key.
    pub fn letter_of(&self, pc: Pc) -> (u8, i32) {
        let (steps, rel) = self.steps_of(pc);
        let (letter, acc) = self.accidental(steps, rel);
        if acc.abs() <= 2 {
            return (letter, acc);
        }
        // A spelling the letter can't carry (only an odd root/mode pair gets here): plain default.
        let n = pc.as_i32();
        let l = NATURAL.iter().rposition(|&x| x <= n).unwrap_or(0);
        (l as u8, n - NATURAL[l])
    }

    /// The note name of `pc` in this key, without an octave (`Bb`, `F#`, `E`; `F##` where a mode
    /// really asks for it).
    pub fn spell(&self, pc: Pc) -> String {
        let (letter, acc) = self.letter_of(pc);
        let mut s = LETTERS[letter as usize].to_string();
        for _ in 0..acc.abs() {
            s.push(if acc > 0 { '#' } else { 'b' });
        }
        s
    }
}

/// MIDI note number of a note `offset_st` semitones above the harmony root (octave 4) — the
/// engine's pitch convention ([`crate::voice::pitch_to_hz`]): offset 0 on a D root is D4 = 62.
pub fn midi_of_offset(root: Pc, offset_st: i32) -> i32 {
    60 + root.as_i32() + offset_st
}

/// A MIDI note's name in `key`, with its octave (scientific pitch, C4 = 60): `A4`, `Bb3`, and
/// `Cb4` / `B#3` where the key spells them (the octave follows the letter, not the pitch class).
pub fn note_name(midi: i32, key: &Key) -> String {
    let (letter, acc) = key.letter_of(Pc::new(midi));
    let octave = (midi - acc - NATURAL[letter as usize]).div_euclid(12) - 1;
    format!("{}{octave}", key.spell(Pc::new(midi)))
}

/// Interval class: smallest semitone distance between two pitch classes (`0..=6`).
pub fn interval_class(a: Pc, b: Pc) -> u8 {
    let d = (a.as_i32() - b.as_i32()).rem_euclid(12);
    d.min(12 - d) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_intervals() {
        assert_eq!(Pc::new(13), Pc(1));
        assert_eq!(Pc::new(-1), Pc(11));
        assert_eq!(Pc::from_offset(Pc(2), 7), Pc(9)); // D + P5 = A
        assert_eq!(interval_class(Pc(0), Pc(7)), 5); // C–G
        assert_eq!(interval_class(Pc(0), Pc(6)), 6); // tritone
        assert_eq!(interval_class(Pc(11), Pc(0)), 1); // B–C
    }

    fn key(root: &str, mode: &str) -> Key {
        Key::new(root, mode).unwrap()
    }

    #[test]
    fn in_scale_notes_take_one_letter_per_degree() {
        let spell = |k: &Key| (0..7).map(|d| k.spell(k.scale.pitch_class(d))).collect::<Vec<_>>().join(" ");
        assert_eq!(spell(&key("D", "dorian")), "D E F G A B C");
        assert_eq!(spell(&key("F", "major")), "F G A Bb C D E");
        assert_eq!(spell(&key("Eb", "minor")), "Eb F Gb Ab Bb Cb Db");
        assert_eq!(spell(&key("C#", "dorian")), "C# D# E F# G# A# B");
        assert_eq!(spell(&key("Db", "dorian")), "Db Eb Fb Gb Ab Bb Cb");
        assert_eq!(spell(&key("B", "locrian")), "B C D E F G A");
        assert_eq!(spell(&key("G#", "harmonic_minor")), "G# A# B C# D# E F##");
        assert_eq!(spell(&key("a", "mixolydian")), "A B C# D E F# G");
    }

    #[test]
    fn out_of_scale_notes_spell_as_chromatic_degrees() {
        let d = key("D", "dorian");
        assert_eq!(d.spell(Pc(6)), "F#"); // raised 3rd
        assert_eq!(d.spell(Pc(10)), "Bb"); // ♭6
        assert_eq!(d.spell(Pc(3)), "Eb"); // ♭2
        assert_eq!(d.spell(Pc(8)), "G#"); // ♯4
        let c = key("C", "major");
        assert_eq!([1, 3, 6, 8, 10].map(|p| c.spell(Pc(p))), ["Db", "Eb", "F#", "Ab", "Bb"]);
    }

    #[test]
    fn note_names_carry_the_octave_of_their_letter() {
        let a = key("A", "mixolydian");
        assert_eq!(note_name(midi_of_offset(a.scale.root, 0), &a), "A4"); // offset 0 = the root in octave 4
        assert_eq!(note_name(midi_of_offset(a.scale.root, -24), &a), "A2"); // idle's 110 Hz crank
        assert_eq!(note_name(midi_of_offset(a.scale.root, 4), &a), "C#5");
        let eb = key("Eb", "minor");
        assert_eq!(note_name(59, &eb), "Cb4"); // B3's pitch, spelled Cb in Eb minor
        assert_eq!(note_name(60, &key("C#", "major")), "B#3");
        assert_eq!(note_name(60, &key("C", "major")), "C4");
        assert_eq!(note_name(21, &key("C", "major")), "A0");
    }

    #[test]
    fn a_pitch_class_root_picks_the_lighter_spelling() {
        let k = |pc: u8, mode| Key::from_scale(Scale::new(Pc(pc), mode));
        assert_eq!(k(3, Mode::Ionian).name(), "Eb ionian");
        assert_eq!(k(1, Mode::Aeolian).name(), "C# aeolian");
        assert_eq!(k(10, Mode::Dorian).name(), "Bb dorian");
        assert_eq!(k(8, Mode::Ionian).name(), "Ab ionian");
        assert_eq!(k(6, Mode::Aeolian).name(), "F# aeolian");
        assert_eq!(k(2, Mode::HarmonicMinor).name(), "D harmonic minor");
        assert!(Key::new("H", "dorian").is_none() && Key::new("D", "dorain").is_none());
    }

    #[test]
    fn root_parsing_and_names() {
        assert_eq!(pc_of_root("C"), Pc(0));
        assert_eq!(pc_of_root("D"), Pc(2));
        assert_eq!(pc_of_root("Bb"), Pc(10));
        assert_eq!(pc_name(Pc(2)), "D");
        assert_eq!(pc_name(Pc(10)), "A#");
    }
}
