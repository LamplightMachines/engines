//! Music labels for the editor: what an `offset` is in the doc's key, what a voicing spells, and
//! which just-intonation harmonic a `cents_offset` lands on. Thin glue over `engines_core::theory`.

use engines_core::theory::{identify, midi_of_offset, note_name, roman, Key, Pc};

/// The note an integer-ish `offset` (semitones from the root, octave 4) names: `A4`, or `A4 +30¢`
/// for a fractional offset.
pub fn note_of(key: &Key, offset: f64, cents: f64) -> String {
    let total = offset * 100.0 + cents;
    let st = (total / 100.0).round();
    let dev = total - st * 100.0;
    let name = note_name(midi_of_offset(key.scale.root, st as i32), key);
    if dev.abs() >= 0.5 { format!("{name} {dev:+.0}¢") } else { name }
}

/// The note a frequency in Hz names, spelled in the key: `A2`, or `A2 -8¢` off the tempered pitch
/// (a binaural `root_hz`, ADR 0047).
pub fn note_of_hz(key: &Key, hz: f64) -> String {
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    let st = midi.round();
    let dev = (midi - st) * 100.0;
    let name = note_name(st as i32, key);
    if dev.abs() >= 0.5 { format!("{name} {dev:+.0}¢") } else { name }
}

/// The degree label of an offset in the key (`5`, `♭3`, `♯4`).
pub fn degree_of(key: &Key, offset: f64) -> String {
    key.scale.mode.interval_label(offset.round() as i32)
}

/// The offset is (rounded) a scale tone.
pub fn in_scale(key: &Key, offset: f64) -> bool {
    key.scale.contains(Pc::from_offset(key.scale.root, offset.round() as i32))
}

/// Frequency of an offset (+ cents) — the engine's pitch convention.
pub fn hz_of(key: &Key, offset: f64, cents: f64) -> f64 {
    let midi = midi_of_offset(key.scale.root, 0) as f64 + offset + cents / 100.0;
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

/// `Dm7 (i7)` for a set of offsets (lowest = the bass), or `None` when they don't spell a chord.
pub fn chord_of(key: &Key, offsets: &[f64]) -> Option<String> {
    let mut sorted: Vec<f64> = offsets.to_vec();
    sorted.sort_by(f64::total_cmp);
    let pcs: Vec<Pc> = sorted.iter().map(|&o| Pc::from_offset(key.scale.root, o.round() as i32)).collect();
    let name = identify(&pcs, pcs.first().copied())?;
    Some(format!("{} ({})", name.label(key), roman(&name, key)))
}

/// A just-intonation reading of a tuned voice: the harmonic `n` of the root it sits on (octave
/// folded), as the ratio `num/den` above the root, and the interval's distance from 12-TET.
#[derive(Clone, Debug, PartialEq)]
pub struct Harmonic {
    pub n: u32,
    pub num: u32,
    pub den: u32,
    /// The JI interval minus its nearest equal-tempered semitone, in cents (h7 → −31.2).
    pub vs_tempered: f64,
}

/// The lowest harmonic (1–32) of the root whose pitch class lies within `tolerance` cents of
/// `offset` semitones + `cents` — `None` when none does.
pub fn harmonic_of(offset: f64, cents: f64, tolerance: f64) -> Option<Harmonic> {
    let c = (offset * 100.0 + cents).rem_euclid(1200.0);
    (1u32..=32).find_map(|n| {
        let hc = 1200.0 * (n as f64).log2();
        let folded = hc.rem_euclid(1200.0);
        let diff = (c - folded + 600.0).rem_euclid(1200.0) - 600.0;
        (diff.abs() <= tolerance).then(|| {
            let den = 1u32 << (n as f64).log2().floor() as u32;
            let g = gcd(n, den);
            Harmonic { n, num: n / g, den: den / g, vs_tempered: folded - (folded / 100.0).round() * 100.0 }
        })
    })
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_degrees_and_chords_in_d_dorian() {
        let k = Key::new("D", "dorian").unwrap();
        assert_eq!(note_of(&k, 7.0, 0.0), "A4");
        assert_eq!(note_of(&k, -12.0, 0.0), "D3");
        assert_eq!(note_of(&k, 3.0, 0.0), "F4");
        assert_eq!(note_of(&k, 4.0, -31.0), "F#4 -31¢");
        assert_eq!(degree_of(&k, 7.0), "5");
        assert_eq!(degree_of(&k, 4.0), "3");
        assert!(in_scale(&k, 10.0) && !in_scale(&k, 4.0));
        assert!((hz_of(&k, 0.0, 0.0) - 293.66).abs() < 0.01);
        assert_eq!(chord_of(&k, &[0.0, 3.0, 7.0, 10.0]).as_deref(), Some("Dm7 (i7)"));
        assert_eq!(chord_of(&k, &[14.0, 7.0, 3.0, 10.0]).as_deref(), Some("Fmaj7 (♭IIImaj7)"));
        assert_eq!(chord_of(&k, &[0.0]), None);
    }

    #[test]
    fn cents_offsets_read_as_harmonics() {
        // idle's partials over A: E +2¢ (h3), C# −14¢ (h5), G −31¢ (h7), B +4¢ (h9).
        let h = |o, c| harmonic_of(o, c, 3.0).map(|h| (h.n, h.num, h.den));
        assert_eq!(h(7.0, 2.0), Some((3, 3, 2)));
        assert_eq!(h(19.0, 2.0), Some((3, 3, 2))); // an octave up: the same harmonic class
        assert_eq!(h(4.0, -14.0), Some((5, 5, 4)));
        assert_eq!(h(10.0, -31.0), Some((7, 7, 4)));
        assert_eq!(h(14.0, 4.0), Some((9, 9, 8)));
        assert_eq!(h(0.0, 0.0), Some((1, 1, 1)));
        assert_eq!(h(6.0, 0.0), None); // a tempered tritone is no low harmonic
        let h7 = harmonic_of(10.0, -31.0, 3.0).unwrap();
        assert!((h7.vs_tempered + 31.17).abs() < 0.01, "{}", h7.vs_tempered);
    }
}
