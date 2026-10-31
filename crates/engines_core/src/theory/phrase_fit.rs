//! phrase_fit (ADR 0028): re-voice a sequence-pool phrase against the current chord, and
//! develop it — the theory half of the Orrery's phrase selection. Pure and deterministic;
//! everything is semitone offsets from the harmony root (the theory layer's native unit).

use crate::hash::{hash_to_unit, hash64};
use crate::loop_doc::PhraseDef;

/// A modulator's `orrery_note_min..=max` (ADR 0042) handed to the fit: semitone offsets from the
/// harmony root, `None` = that side open. With either side set it takes precedence over the line as
/// written (ADR 0042 amendment, 2026-10-11).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NoteRange {
    pub min: Option<f32>,
    pub max: Option<f32>,
}

impl NoteRange {
    pub fn is_set(&self) -> bool {
        self.min.is_some() || self.max.is_some()
    }

    /// Whole-semitone bounds (`min` rounds up, `max` down); an open side is `i32::MIN` / `i32::MAX`.
    fn bounds(&self) -> (i32, i32) {
        let lo = self.min.map_or(i32::MIN, |m| m.ceil().clamp(i32::MIN as f32, i32::MAX as f32) as i32);
        let hi = self.max.map_or(i32::MAX, |m| m.floor().clamp(i32::MIN as f32, i32::MAX as f32) as i32);
        (lo, hi)
    }
}

/// Re-fit `def`'s pitches for the current chord, preserving rhythm, gates, legato, prob and
/// velocity. `fit`: `"chord"` = strong (whole-step `t`) notes snap to the nearest chord
/// tone, weak (sub-step) notes to the nearest scale tone with the melody-fit avoid rule (a
/// pc a semitone above a chord tone bumps one scale step up); `"root"` = parallel transpose
/// following the chord root; `"arp"` = every note to a chord tone along the written line (see
/// [`refit_arp`]); `"literal"` = as written. `chord_tones` / `scale_degrees` are semitone offsets
/// from the harmony root.
///
/// `range` (the modulator's note range) bounds the fit when either side is set: `"arp"` uses it
/// in place of the written range, `"chord"` moves a note past an edge to the nearest fitting tone
/// inside it, `"root"` picks the transposition octave that fits best ([`octave_fit`]) and folds any
/// stray by octaves. A written line that lies wholly outside the range (an `"arp"` or `"chord"`
/// entry with no note inside it) first moves by the octaves that fit it best, contour kept; a line
/// that overlaps the range keeps its written register. `"literal"` stays as written (`orrery::enforce_ranges` still folds it afterwards).
pub fn refit(def: &PhraseDef, chord_tones: &[i32], scale_degrees: &[i32], fit: &str, range: NoteRange) -> PhraseDef {
    let mut out = def.clone();
    match fit {
        "literal" => {}
        "arp" => refit_arp(&mut out, chord_tones, scale_degrees, range),
        "root" => {
            let c = chord_tones.first().map(|&r| center(r)).unwrap_or(0);
            let (lo, hi) = range.bounds();
            // The centred root unless another octave leaves fewer notes outside the range.
            let delta = if range.is_set() {
                let moved: Vec<i32> = out.notes.iter().map(|n| n.offset.round() as i32 + c).collect();
                c + 12 * octave_fit(&moved, lo, hi)
            } else {
                c
            };
            for n in out.notes.iter_mut() {
                n.offset += delta as f32;
                if range.is_set() {
                    let r = n.offset.round() as i32;
                    n.offset += (fold_into(r, lo, hi) - r) as f32;
                }
            }
        }
        _ => {
            // "chord"
            if chord_tones.is_empty() {
                return out;
            }
            let chord_pcs: Vec<i32> = chord_tones.iter().map(|c| c.rem_euclid(12)).collect();
            let scale_pcs: Vec<i32> = scale_degrees.iter().map(|d| d.rem_euclid(12)).collect();
            let avoid = |p: i32| chord_pcs.iter().any(|&c| (p - c).rem_euclid(12) == 1);
            let (lo, hi) = range.bounds();
            let written: Vec<i32> = out.notes.iter().map(|n| n.offset.round() as i32).collect();
            let shift = if range.is_set() { off_range_shift(&written, lo, hi) } else { 0 };
            for n in out.notes.iter_mut() {
                n.offset += shift as f32;
                let strong = n.t.fract() == 0.0;
                let target = if strong { &chord_pcs } else { &scale_pcs };
                let mut snapped = snap(n.offset.round() as i32, target);
                if !strong && avoid(snapped) {
                    // the avoid rule: a semitone above a chord tone clashes — resolve up.
                    snapped = next_scale_above(snapped, &scale_pcs);
                }
                if !(lo..=hi).contains(&snapped) {
                    // Past an edge of the note range: the nearest fitting tone inside it.
                    let fits = |p: i32| target.contains(&p.rem_euclid(12)) && (strong || !avoid(p));
                    snapped = inside(snapped, lo, hi, fits).unwrap_or(snapped);
                }
                n.offset = snapped as f32;
            }
        }
    }
    out
}

/// The plain-render degradation of `"chord"` fit (ADR 0028 amendment): without a controller
/// there is no chord, but the doc's authored `harmony` IS known — a note that is already a
/// scale tone plays **exactly as written** (a fully diatonic entry renders bit-identically,
/// the 4th in major and the ♭6 in minor included); an out-of-scale note snaps to the nearest
/// scale tone, and only THAT snapped note runs the avoid rule against the implied tonic
/// triad (scale degrees 1-3-5: a semitone above a triad tone resolves up a scale step).
/// Keeps the library's major-triad contours in-key under plain `engines-render`/`engines-play`.
/// Rhythm, gates, legato, prob and velocity untouched.
pub fn refit_static(def: &PhraseDef, scale_degrees: &[i32]) -> PhraseDef {
    let mut out = def.clone();
    if scale_degrees.len() < 5 {
        return out;
    }
    let scale_pcs: Vec<i32> = scale_degrees.iter().map(|d| d.rem_euclid(12)).collect();
    let triad_pcs = [scale_pcs[0], scale_pcs[2], scale_pcs[4]];
    for n in out.notes.iter_mut() {
        let authored = n.offset.round() as i32;
        if scale_pcs.contains(&authored.rem_euclid(12)) {
            continue; // in scale: the author's note stands
        }
        let mut snapped = snap(authored, &scale_pcs);
        if triad_pcs.iter().any(|&c| (snapped - c).rem_euclid(12) == 1) {
            snapped = next_scale_above(snapped, &scale_pcs);
        }
        n.offset = snapped as f32;
    }
    out
}

/// The `"arp"` fit: an arpeggio re-voiced onto the current chord with its contour and range kept.
/// Taken in time order, every note (on or off the grid) moves to the nearest chord tone; a tie
/// goes the way the written line moves (up while it rises, down while it falls); where the
/// written line moves, a note never repeats the previous pitch — it steps on to the next chord
/// tone that way; and nothing leaves the range (a note past an edge folds to the nearest chord
/// tone inside it). So F3 A3 C4 F4 A4 C5 F5 under Gm7 plays F3 B♭3 D4 F4 B♭4 D5 F5, where the
/// nearest-tone `"chord"` fit would collapse A and C onto B♭.
///
/// The range is the modulator's `range` when either side is set (an unset side is open; a line
/// wholly outside it first moves by [`off_range_shift`]), else the entry's written range (ADR 0042
/// amendment). When no new chord tone fits inside it, the step
/// still sounds a new pitch: a tension tone over the chord (see [`tension_pcs`]) the way the line
/// moves, else the nearest chord or tension tone to the written note — never a re-strike.
fn refit_arp(out: &mut PhraseDef, chord_tones: &[i32], scale_degrees: &[i32], range: NoteRange) {
    if chord_tones.is_empty() || out.notes.is_empty() {
        return;
    }
    let pcs: Vec<i32> = chord_tones.iter().map(|c| c.rem_euclid(12)).collect();
    let tensions = tension_pcs(chord_tones, scale_degrees);
    let mut written: Vec<i32> = out.notes.iter().map(|n| n.offset.round() as i32).collect();
    let (lo, hi) = if range.is_set() {
        let (lo, hi) = range.bounds();
        let shift = off_range_shift(&written, lo, hi);
        written.iter_mut().for_each(|w| *w += shift);
        (lo, hi)
    } else {
        (*written.iter().min().unwrap(), *written.iter().max().unwrap())
    };
    let mut order: Vec<usize> = (0..out.notes.len()).collect();
    order.sort_by(|&a, &b| out.notes[a].t.total_cmp(&out.notes[b].t));
    let mut prev: Option<(i32, i32)> = None; // (written, realized)
    for i in order {
        let w = written[i];
        let dir = prev.map_or(0, |(pw, _)| (w - pw).signum());
        let mut r = snap_toward(w, &pcs, dir);
        if let Some((pw, pr)) = prev {
            if r == pr && w != pw {
                r = step_tone(r, &pcs, if dir > 0 { 1 } else { -1 });
            }
        }
        if r > hi {
            r = step_tone(hi + 1, &pcs, -1);
        }
        if r < lo {
            r = step_tone(lo - 1, &pcs, 1);
        }
        if !(lo..=hi).contains(&r) {
            r = snap_toward(w, &pcs, dir); // no chord tone inside the range: the plain nearest
        }
        if let Some((pw, pr)) = prev {
            if r == pr && w != pw {
                r = fresh_tone(pr, w, dir, lo, hi, &pcs, &tensions).unwrap_or(r);
            }
        }
        out.notes[i].offset = r as f32;
        prev = Some((w, r));
    }
}

/// The tensions that sit well over a chord: scale pitch classes a 9th, 11th or 13th above its root
/// (`chord_tones[0]`, intervals 1 2 3 5 6 8 9 — not a 7th, which recolours the chord), that are
/// not chord tones and not avoid notes (a semitone above a chord tone: F over Am or E7, the 11th
/// over a major triad).
fn tension_pcs(chord_tones: &[i32], scale_degrees: &[i32]) -> Vec<i32> {
    let Some(&root) = chord_tones.first() else { return Vec::new() };
    let chord: Vec<i32> = chord_tones.iter().map(|c| c.rem_euclid(12)).collect();
    let mut out: Vec<i32> = scale_degrees
        .iter()
        .map(|d| d.rem_euclid(12))
        .filter(|&p| [1, 2, 3, 5, 6, 8, 9].contains(&(p - root).rem_euclid(12)))
        .filter(|&p| !chord.contains(&p) && !chord.iter().any(|&c| (p - c).rem_euclid(12) == 1))
        .collect();
    out.dedup();
    out
}

/// A new pitch for an arp step that would re-strike `pr` (no new chord tone fits the range): the
/// nearest tension tone past `pr` the way the line moves (`dir`), else the chord or tension tone
/// inside the range nearest the written note `w` (a tie goes the line's way). Within an octave of
/// `pr`; `None` when the range holds nothing else.
fn fresh_tone(pr: i32, w: i32, dir: i32, lo: i32, hi: i32, chord: &[i32], tensions: &[i32]) -> Option<i32> {
    let ok = |c: i32| (lo..=hi).contains(&c);
    let ahead = (1..=12).map(|k| pr + dir * k).filter(|&c| ok(c)).find(|c| tensions.contains(&c.rem_euclid(12)));
    ahead.or_else(|| {
        (-12..=12)
            .map(|k| pr + k)
            .filter(|&c| c != pr && ok(c))
            .filter(|c| chord.contains(&c.rem_euclid(12)) || tensions.contains(&c.rem_euclid(12)))
            .min_by_key(|&c| ((c - w).abs(), -(c - w).signum() * dir))
    })
}

/// The pitch nearest `from` whose pitch class is in `pcs`; a tie goes up when `dir > 0`, else down.
fn snap_toward(from: i32, pcs: &[i32], dir: i32) -> i32 {
    let down = snap(from, pcs);
    let up = from + (from - down);
    if dir > 0 && up != down && pcs.contains(&up.rem_euclid(12)) {
        up
    } else {
        down
    }
}

/// The next pitch strictly past `from` in direction `dir` (±1) whose pitch class is in `pcs`.
fn step_tone(from: i32, pcs: &[i32], dir: i32) -> i32 {
    (1..=12).map(|k| from + dir * k).find(|c| pcs.contains(&c.rem_euclid(12))).unwrap_or(from)
}

/// Seeded, length-preserving development of a phrase: identity | rotate the pitch row
/// against the (unchanged) rhythm | fragment — keep the front half, leave the back of the
/// tile as a breath. The motif-development family of `theory::melody::develop`, lifted to
/// timed notes.
pub fn develop_notes(def: &PhraseDef, seed: u64) -> PhraseDef {
    let mut out = def.clone();
    let n = out.notes.len();
    if n < 2 {
        return out;
    }
    match hash64(seed, 0xDE7, 0, 0) % 3 {
        1 => {
            // rotate pitches against the rhythm skeleton
            let r = 1 + (hash_to_unit(hash64(seed, 0xDE7, 1, 0)) * (n - 1) as f32) as usize % (n - 1);
            let offsets: Vec<f32> = out.notes.iter().map(|x| x.offset).collect();
            for (i, note) in out.notes.iter_mut().enumerate() {
                note.offset = offsets[(i + r) % n];
            }
        }
        2 => {
            // fragment: the front half plays, the back half of the tile breathes
            out.notes.truncate(n.div_ceil(2));
        }
        _ => {}
    }
    out
}

/// Octave-place a pitch-class offset to its instance nearest 0 (in `[-6, 6]`).
fn center(off: i32) -> i32 {
    let o = off.rem_euclid(12);
    if o > 6 {
        o - 12
    } else {
        o
    }
}

/// The pitch nearest `from` whose pitch class is in `pcs` (ties break downward).
fn snap(from: i32, pcs: &[i32]) -> i32 {
    let mut best = from;
    let mut best_d = i32::MAX;
    for &pc in pcs {
        let base = from - (from - pc).rem_euclid(12); // largest candidate ≤ from with this pc
        for cand in [base, base + 12] {
            let d = (cand - from).abs();
            if d < best_d || (d == best_d && cand < best) {
                best = cand;
                best_d = d;
            }
        }
    }
    best
}

/// The whole-octave move (in octaves) that leaves the fewest of `pitches` outside `lo..=hi`: no move
/// preferred, then the smallest (the rule `orrery::enforce_ranges` moves a surface by, ADR 0042 D3).
/// Besides the neighbouring octaves it tries the ones that align the line's lowest note with `lo`
/// and its highest with `hi`, so a range far from the line is reached in one move.
fn octave_fit(pitches: &[i32], lo: i32, hi: i32) -> i32 {
    let (Some(&pmin), Some(&pmax)) = (pitches.iter().min(), pitches.iter().max()) else { return 0 };
    let (lo, hi) = (lo as i64, hi as i64);
    let mut ks: Vec<i64> = vec![0, -1, 1];
    if lo > i32::MIN as i64 {
        let k = (lo - pmin as i64 + 11).div_euclid(12); // the lowest note just inside `lo`
        ks.extend([k, k - 1]);
    }
    if hi < i32::MAX as i64 {
        let k = (hi - pmax as i64).div_euclid(12); // the highest note just inside `hi`
        ks.extend([k, k + 1]);
    }
    let outside = |k: i64| pitches.iter().filter(|&&p| !(lo..=hi).contains(&(p as i64 + 12 * k))).count();
    ks.into_iter().map(|k| k.clamp(-1_000, 1_000)).min_by_key(|&k| (outside(k), k.abs())).unwrap_or(0) as i32
}

/// The semitone move for a written line with NO note inside `lo..=hi`: the octaves that fit it best
/// ([`octave_fit`]), so a range far from the line keeps the contour instead of piling every note
/// onto the edge. A line with any note inside keeps its written register (0): the author's line
/// stands, and an open side stays open rather than becoming room to relocate it.
fn off_range_shift(written: &[i32], lo: i32, hi: i32) -> i32 {
    if written.iter().any(|w| (lo..=hi).contains(w)) {
        0
    } else {
        12 * octave_fit(written, lo, hi)
    }
}

/// The pitch nearest the edge `x` crossed, inside `lo..=hi`, that `fits` (within an octave of it).
fn inside(x: i32, lo: i32, hi: i32, fits: impl Fn(i32) -> bool) -> Option<i32> {
    if x > hi {
        (0..12).map(|k| hi - k).take_while(|&c| c >= lo).find(|&c| fits(c))
    } else if x < lo {
        (0..12).map(|k| lo + k).take_while(|&c| c <= hi).find(|&c| fits(c))
    } else {
        Some(x)
    }
}

/// Fold `x` by octaves into `lo..=hi`; a range narrower than an octave that holds no instance of
/// `x`'s pitch class keeps the instance nearest to it (as `orrery::enforce_ranges` folds).
fn fold_into(x: i32, lo: i32, hi: i32) -> i32 {
    let (lo, hi) = (lo as i64, hi as i64);
    let mut y = x as i64;
    if y > hi {
        y -= 12 * (y - hi + 11).div_euclid(12);
    }
    if y < lo {
        y += 12 * (lo - y + 11).div_euclid(12);
    }
    if y > hi && y - hi > lo - (y - 12) {
        y -= 12;
    }
    y as i32
}

/// The nearest scale-pc pitch strictly above `from`.
fn next_scale_above(from: i32, scale_pcs: &[i32]) -> i32 {
    (1..=12).map(|k| from + k).find(|c| scale_pcs.contains(&c.rem_euclid(12))).unwrap_or(from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_doc::{PhraseDef, PhraseNote};

    fn phrase(notes: &[(f32, f32)]) -> PhraseDef {
        PhraseDef {
            fit: "chord".into(),
            tags: vec![],
            length: 4,
            notes: notes
                .iter()
                .map(|&(t, offset)| PhraseNote { t, offset, dur: Some(0.5), legato: false, prob: 1.0, velocity: Some(100), cents_offset: 0.0 })
                .collect(),
        }
    }

    const MAJOR: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];

    #[test]
    fn chord_fit_snaps_strong_notes_to_chord_tones_preserving_rhythm() {
        // C-major phrase against an "Am"-shaped chord (offsets 9, 0, 4): strong notes land
        // on chord pcs, weak notes on scale pcs, rhythm/gates/velocity untouched.
        let p = phrase(&[(0.0, 0.0), (0.5, 2.0), (1.0, 4.0), (2.5, 11.0)]);
        let f = refit(&p, &[9, 12, 16], &MAJOR, "chord", NoteRange::default());
        for (a, b) in f.notes.iter().zip(&p.notes) {
            assert_eq!(a.t, b.t);
            assert_eq!(a.dur, b.dur);
            assert_eq!(a.velocity, b.velocity);
        }
        let chord_pcs = [9, 0, 4];
        for n in f.notes.iter().filter(|n| n.t.fract() == 0.0) {
            assert!(chord_pcs.contains(&(n.offset as i32).rem_euclid(12)), "strong note {} on a chord tone", n.offset);
        }
        for n in f.notes.iter().filter(|n| n.t.fract() != 0.0) {
            let pc = (n.offset as i32).rem_euclid(12);
            assert!(MAJOR.contains(&pc), "weak note {} in scale", n.offset);
            assert!(!chord_pcs.iter().any(|&c| (pc - c).rem_euclid(12) == 1), "weak note {} avoids the ♭9-style clash", n.offset);
        }
        // movement is minimal: nothing snaps further than a whole step
        for (a, b) in f.notes.iter().zip(&p.notes) {
            assert!((a.offset - b.offset).abs() <= 2.0, "{} → {}", b.offset, a.offset);
        }
    }

    #[test]
    fn arp_fit_keeps_the_contour_and_range_on_the_current_chord() {
        // F-major arpeggio F3 → F5 → A3 (offsets from F), one note per step.
        let line = [-12.0, -8.0, -5.0, 0.0, 4.0, 7.0, 12.0, 7.0, 4.0, 0.0, -5.0, -8.0];
        let mut p = phrase(&line.iter().enumerate().map(|(i, &o)| (i as f32, o)).collect::<Vec<_>>());
        p.fit = "arp".into();
        let fit = |tones: &[i32]| -> Vec<f32> { refit(&p, tones, &MAJOR, "arp", NoteRange::default()).notes.iter().map(|n| n.offset).collect() };
        // Home (Fmaj7): as written.
        assert_eq!(fit(&[0, 4, 7, 11]), line.to_vec());
        // Gm7 (G B♭ D F): ties follow the line, no repeats: F3 B♭3 D4 F4 B♭4 D5 F5 B♭4 G4 F4 B♭3 G3.
        assert_eq!(fit(&[2, 5, 9, 12]), vec![-12.0, -7.0, -3.0, 0.0, 5.0, 9.0, 12.0, 5.0, 2.0, 0.0, -7.0, -10.0]);
        // Em7♭5 (E G B♭ D): F3 → E3 would leave the range, so the floor folds up to G3.
        assert_eq!(fit(&[11, 14, 17, 21]), vec![-10.0, -7.0, -3.0, -1.0, 5.0, 9.0, 11.0, 5.0, 2.0, -1.0, -7.0, -10.0]);
        // Every chord: on chord tones, inside the written range, and no repeat where the line moves.
        for tones in [[4, 7, 11, 14], [7, 11, 14, 17], [8, 12, 15, 19], [10, 14, 17, 20], [2, 6, 9, 12]] {
            let r = fit(&tones);
            let pcs: Vec<i32> = tones.iter().map(|t| t.rem_euclid(12)).collect();
            for (k, &o) in r.iter().enumerate() {
                assert!(pcs.contains(&(o as i32).rem_euclid(12)), "{tones:?}: {o} is a chord tone");
                assert!((-12.0..=12.0).contains(&o), "{tones:?}: {o} inside F3..F5");
                if k > 0 && line[k] != line[k - 1] {
                    assert_ne!(o, r[k - 1], "{tones:?}: no repeat at {k}: {r:?}");
                }
            }
        }
        // Rhythm, gates and the rest are untouched.
        let r = refit(&p, &[2, 5, 9, 12], &MAJOR, "arp", NoteRange::default());
        assert!(r.notes.iter().zip(&p.notes).all(|(a, b)| a.t == b.t && a.dur == b.dur && a.prob == b.prob));
    }

    /// The F-major up-and-down arpeggio F3 → F5 → A3 (offsets from F), one note per step.
    const ARP_LINE: [f32; 12] = [-12.0, -8.0, -5.0, 0.0, 4.0, 7.0, 12.0, 7.0, 4.0, 0.0, -5.0, -8.0];

    fn arp_fit(tones: &[i32], range: NoteRange) -> Vec<f32> {
        let mut p = phrase(&ARP_LINE.iter().enumerate().map(|(i, &o)| (i as f32, o)).collect::<Vec<_>>());
        p.fit = "arp".into();
        refit(&p, tones, &MAJOR, "arp", range).notes.iter().map(|n| n.offset).collect()
    }

    const C_MAJ: [i32; 3] = [7, 11, 14]; // C E G over F
    const A_MIN: [i32; 3] = [4, 7, 11]; // A C E over F

    #[test]
    fn arp_fit_takes_the_modulator_range_over_the_written_one() {
        // Under C the written F3..F5 holds one chord tone too few for the rise; a modulator max of 16
        // (A5) lets it climb to G5: G3 C4 E4 G4 C5 E5 G5 C5 G4 E4 C4 G3.
        let both = NoteRange { min: Some(-12.0), max: Some(16.0) };
        assert_eq!(arp_fit(&C_MAJ, both), vec![-10.0, -5.0, -1.0, 2.0, 7.0, 11.0, 14.0, 7.0, 2.0, -1.0, -5.0, -10.0]);
        // A narrower range than the line also wins: nothing above C5 (7) under C.
        let low = arp_fit(&C_MAJ, NoteRange { min: Some(-12.0), max: Some(7.0) });
        assert!(low.iter().all(|&o| (-12.0..=7.0).contains(&o)), "{low:?}");
        // One bound set: the other side is open, so the floor no longer folds up — F3 under C goes
        // to its nearest chord tone, E3, below the written bottom.
        let max_only = arp_fit(&C_MAJ, NoteRange { min: None, max: Some(16.0) });
        assert_eq!(max_only, vec![-13.0, -10.0, -5.0, -1.0, 2.0, 7.0, 11.0, 7.0, 2.0, -1.0, -5.0, -10.0], "the rise starts a tone lower");
        let min_only = arp_fit(&C_MAJ, NoteRange { min: Some(-12.0), max: None });
        assert_eq!(min_only[6], 14.0, "an open top: {min_only:?}");
        // A range with no note of the line inside: the line first moves by the octaves that fit it
        // (+108 ⇒ 96..120 inside 90..130), so the arp keeps its contour instead of piling onto the
        // edge. A one-sided ceiling under the written top does NOT drop the arp an octave: only the
        // notes past it change.
        let far = arp_fit(&[0, 4, 7, 11], NoteRange { min: Some(90.0), max: Some(130.0) });
        assert_eq!(far, ARP_LINE.iter().map(|o| o + 108.0).collect::<Vec<_>>());
        let capped = arp_fit(&[0, 4, 7, 11], NoteRange { min: None, max: Some(7.0) });
        assert_eq!(capped[..6], ARP_LINE[..6], "below the ceiling, as written: {capped:?}");
        assert!(capped[6] <= 7.0, "{capped:?}");
        // No range: the written range, exactly as before (Fmaj7 plays as written).
        assert_eq!(arp_fit(&[0, 4, 7, 11], NoteRange::default()), ARP_LINE.to_vec());
    }

    #[test]
    fn arp_fit_never_restrikes_a_step_it_takes_a_tension_instead() {
        // Written range F3..F5 under C: the rise runs out of chord tones at E5, and G5 is past the
        // top. The step takes D5 (the 9th) rather than striking E5 again; F5 (the 11th, a semitone
        // over E) is an avoid note.
        let c = arp_fit(&C_MAJ, NoteRange::default());
        assert_eq!(c, vec![-10.0, -5.0, -1.0, 2.0, 7.0, 11.0, 9.0, 7.0, 2.0, -1.0, -5.0, -10.0]);
        // Under Am the same top takes D5 (the 11th), never F (the ♭13, a semitone over E).
        let am = arp_fit(&A_MIN, NoteRange::default());
        assert_eq!(am[6], 9.0, "{am:?}");
        assert_eq!(tension_pcs(&C_MAJ, &MAJOR), vec![4, 9], "A (13th) and D (9th) over C; F avoided, B♭ a 7th");
        assert_eq!(tension_pcs(&A_MIN, &MAJOR), vec![9], "D (11th) over Am; F and B♭ avoided, G a 7th");
        // Every triad/seventh of F major, ranged and not: no re-strike wherever the line moves, and
        // every note inside the range.
        let chords: [&[i32]; 9] = [&C_MAJ, &A_MIN, &[2, 5, 9], &[9, 12, 16], &[5, 9, 12], &[11, 14, 17], &[2, 5, 9, 12], &[4, 7, 11, 14], &[7, 11, 14, 17]];
        for range in [NoteRange::default(), NoteRange { min: Some(-12.0), max: Some(16.0) }, NoteRange { min: Some(-5.0), max: Some(9.0) }] {
            let (lo, hi) = if range.is_set() { (range.min.unwrap(), range.max.unwrap()) } else { (-12.0, 12.0) };
            for tones in chords {
                let r = arp_fit(tones, range);
                for (k, &o) in r.iter().enumerate() {
                    assert!((lo..=hi).contains(&o), "{tones:?} {range:?}: {o} inside {lo}..{hi}: {r:?}");
                    if k > 0 && ARP_LINE[k] != ARP_LINE[k - 1] {
                        assert_ne!(o, r[k - 1], "{tones:?} {range:?}: no re-strike at {k}: {r:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn chord_and_root_fits_honour_the_range() {
        // chord: a strong note past the top moves to the nearest chord tone inside (C5 under C),
        // a weak one to the nearest non-avoid scale tone inside (D5). The line overlaps the range,
        // so it keeps its register — the open floor is not room to drop it an octave.
        let p = phrase(&[(0.0, 12.0), (0.5, 13.0), (1.0, 0.0)]);
        let fit = |p: &PhraseDef, range| -> Vec<f32> { refit(p, &C_MAJ, &MAJOR, "chord", range).notes.iter().map(|n| n.offset).collect() };
        assert_eq!(fit(&p, NoteRange { min: None, max: Some(10.0) }), vec![7.0, 9.0, -1.0]);
        // No range: as before (F5 → E5; the weak 13 snaps to F5, an avoid note over E, so it resolves up to G5).
        let free = fit(&p, NoteRange::default());
        assert_eq!(free, vec![11.0, 14.0, -1.0]);
        // Both edges: the strong F3 (→ E3, under the floor) goes to G3, the weak one (→ G5) to D5.
        let wide = phrase(&[(0.0, -12.0), (0.5, 13.0), (1.0, 0.0)]);
        assert_eq!(fit(&wide, NoteRange::default()), vec![-13.0, 14.0, -1.0]);
        assert_eq!(fit(&wide, NoteRange { min: Some(-12.0), max: Some(10.0) }), vec![-10.0, 9.0, -1.0]);
        // root: the centred transposition (−5) unless another octave fits better.
        let q = phrase(&[(0.0, 0.0), (1.0, 7.0), (2.0, 12.0)]);
        let root = |range| -> Vec<f32> { refit(&q, &[7, 11, 14], &MAJOR, "root", range).notes.iter().map(|n| n.offset).collect() };
        assert_eq!(root(NoteRange::default()), vec![-5.0, 2.0, 7.0]);
        assert_eq!(root(NoteRange { min: Some(0.0), max: None }), vec![7.0, 14.0, 19.0], "the octave up fits whole");
        assert_eq!(root(NoteRange { min: Some(0.0), max: Some(12.0) }), vec![7.0, 2.0, 7.0], "the best octave, the stray folded");
        // A range far from the line: root takes the octave that fits it whole, the contour kept
        // (Copilot review, PR 22), not three notes folded on their own to [103, 110, 103]; a chord
        // line with no note inside moves the same way before it fits.
        assert_eq!(root(NoteRange { min: Some(100.0), max: Some(127.0) }), vec![103.0, 110.0, 115.0]);
        let far = fit(&p, NoteRange { min: Some(100.0), max: Some(127.0) });
        assert_eq!(far, free.iter().map(|o| o + 108.0).collect::<Vec<_>>(), "chord: the line moves 9 octaves, then fits");
        // literal: as written whatever the range.
        assert_eq!(refit(&q, &[7, 11, 14], &MAJOR, "literal", NoteRange { min: Some(2.0), max: Some(3.0) }), q);
    }

    #[test]
    fn root_fit_transposes_in_parallel_and_literal_is_verbatim() {
        let p = phrase(&[(0.0, 0.0), (1.5, 7.0)]);
        let r = refit(&p, &[7, 11, 14], &MAJOR, "root", NoteRange::default()); // chord root a fifth up → −5 centered
        let d: Vec<f32> = r.notes.iter().zip(&p.notes).map(|(a, b)| a.offset - b.offset).collect();
        assert!(d.iter().all(|&x| x == d[0]), "parallel: {d:?}");
        assert_eq!(d[0], -5.0, "fifth above, centered to the nearest instance of the root pc");
        let l = refit(&p, &[7, 11, 14], &MAJOR, "literal", NoteRange::default());
        assert_eq!(l, p);
    }

    #[test]
    fn refit_static_snaps_to_the_authored_scale_with_the_triad_avoid_rule() {
        const DORIAN: [i32; 7] = [0, 2, 3, 5, 7, 9, 10];
        // The library's major-triad contour (the lib:and_lift shape) lands in-key: 16 (an
        // octave + MAJOR third — F# over a D doc) snaps to 15; diatonic notes untouched.
        let p = phrase(&[(0.5, 7.0), (1.5, 12.0), (2.5, 16.0), (3.0, 4.0), (3.5, 14.0)]);
        let f = refit_static(&p, &DORIAN);
        let offs: Vec<f32> = f.notes.iter().map(|n| n.offset).collect();
        assert_eq!(offs, vec![7.0, 12.0, 15.0, 3.0, 14.0]);
        for (a, b) in f.notes.iter().zip(&p.notes) {
            assert_eq!(a.t, b.t);
            assert_eq!(a.dur, b.dur);
            assert_eq!(a.velocity, b.velocity);
        }
        // An AUTHORED scale tone always stands — the 4th in ionian and the ♭6 in aeolian sit
        // a semitone above a triad tone, but they are the author's notes, not snap artefacts.
        const AEOLIAN: [i32; 7] = [0, 2, 3, 5, 7, 8, 10];
        assert_eq!(refit_static(&phrase(&[(0.5, 5.0)]), &MAJOR).notes[0].offset, 5.0, "authored 4th preserved");
        assert_eq!(refit_static(&phrase(&[(0.5, 8.0)]), &AEOLIAN).notes[0].offset, 8.0, "authored ♭6 preserved");
        // The avoid rule applies only to a note that had to be SNAPPED: the ♯4 (6) snaps to
        // the 4th (5), which is a semitone above the 3rd, so it resolves up to the 5th.
        assert_eq!(refit_static(&phrase(&[(0.5, 6.0)]), &MAJOR).notes[0].offset, 7.0, "snapped note runs the avoid rule");
        // A fully diatonic entry is untouched (the amendment's bit-identical guarantee).
        let dia = phrase(&[(0.0, 0.0), (1.0, 5.0), (2.0, 9.0), (3.0, 11.0)]);
        assert_eq!(refit_static(&dia, &MAJOR), dia);
    }

    #[test]
    fn develop_is_seeded_deterministic_and_length_preserving() {
        let p = phrase(&[(0.0, 0.0), (1.0, 4.0), (2.0, 7.0), (3.0, 12.0)]);
        for seed in 0..24u64 {
            let a = develop_notes(&p, seed);
            let b = develop_notes(&p, seed);
            assert_eq!(a, b, "seeded — reproducible");
            assert_eq!(a.length, p.length, "tile length never changes");
            assert!(!a.notes.is_empty());
            for n in &a.notes {
                assert!(p.notes.iter().any(|o| o.offset == n.offset), "pitches come from the source row");
            }
        }
        // all three ops occur across seeds
        let variants: std::collections::BTreeSet<usize> = (0..24u64).map(|s| develop_notes(&p, s).notes.len()).collect();
        assert!(variants.len() > 1, "fragmenting shows up across seeds");
    }
}
