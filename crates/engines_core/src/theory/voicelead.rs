//! Voice-leading: move a set of stable voice ids to a target chord with minimal total
//! motion — the smooth morph the Orrery relies on (the engine glides between offsets when a
//! voice id keeps its id and changes its `offset`).

use super::chord::Chord;
use super::pitch::Pc;
use super::scale::Scale;

pub type VoiceId = String;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Voicing {
    /// `(voice id, semitone offset from the scale root)`.
    pub voices: Vec<(VoiceId, i32)>,
}

impl Voicing {
    /// Construct from id/offset pairs.
    pub fn from_pairs<I, S>(it: I) -> Voicing
    where
        I: IntoIterator<Item = (S, i32)>,
        S: Into<String>,
    {
        Voicing { voices: it.into_iter().map(|(s, o)| (s.into(), o)).collect() }
    }

    /// Offsets, sorted ascending.
    pub fn offsets(&self) -> Vec<i32> {
        let mut v: Vec<i32> = self.voices.iter().map(|(_, o)| *o).collect();
        v.sort_unstable();
        v
    }

    /// Total semitone motion to reach `self` from `prev`, summed over shared voice ids.
    pub fn movement_from(&self, prev: &Voicing) -> i32 {
        self.voices
            .iter()
            .map(|(id, o)| {
                prev.voices
                    .iter()
                    .find(|(pid, _)| pid == id)
                    .map(|(_, po)| (o - po).abs())
                    .unwrap_or(0)
            })
            .sum()
    }
}

pub struct LeadOpts {
    /// Center the voicing around this offset (semitones from root) — register placement.
    pub register_center: i32,
    /// A voice id pinned to hold its current pitch when that pitch is a chord tone (pedal).
    pub pedal: Option<VoiceId>,
    /// Common-tone retention (Phase 5 voice-change governor): hold up to `retain` voices whose
    /// current pitch is already a chord tone at that pitch, reducing voice motion. `0` ⇒ off
    /// (byte-identical to the pre-governor lead); the pedal voice is its `retain == 1` special case.
    pub retain: usize,
}
impl Default for LeadOpts {
    fn default() -> Self {
        Self { register_center: 7, pedal: None, retain: 0 }
    }
}

/// Voice-lead `from` to `chord`, keeping the same voice ids. Each voice is assigned a target
/// offset so that the **total** semitone motion is minimal (sorted-to-sorted matching is
/// optimal for summed absolute distance in one dimension), preserving spread (no crossing).
/// Voice count is preserved: extra voices octave-double the lowest tones; if there are fewer
/// voices than chord tones, the lowest tones are used.
pub fn lead(from: &Voicing, chord: &Chord, scale: &Scale, opts: &LeadOpts) -> Voicing {
    if from.voices.is_empty() {
        return Voicing::default();
    }
    let v = from.voices.len();
    let targets = target_offsets(chord, scale, v, opts.register_center); // sorted ascending

    // Match voices (sorted by current pitch) to sorted targets in order — minimal total motion.
    let mut order: Vec<usize> = (0..v).collect();
    order.sort_by_key(|&i| from.voices[i].1);

    let mut out: Vec<(VoiceId, i32)> = from.voices.clone();
    for (slot, &i) in order.iter().enumerate() {
        out[i].1 = targets[slot];
    }

    // Pedal: if the pinned voice currently holds a chord tone, let it keep that exact pitch.
    if let Some(pid) = &opts.pedal {
        if let Some((_, cur)) = from.voices.iter().find(|(id, _)| id == pid) {
            let pc = Pc::from_offset(scale.root, *cur);
            if chord.pitch_classes(scale).contains(&pc) {
                for vx in out.iter_mut() {
                    if &vx.0 == pid {
                        vx.1 = *cur;
                    }
                }
            }
        }
    }

    // Common-tone retention (governor): hold up to `retain` voices whose *current* pitch is already
    // a chord tone, at that pitch — each held voice is one fewer voice change. Prefer the voices the
    // assignment would have moved the most (holding them saves the most motion); deterministic ties
    // by index. With `retain == 0` this is a no-op.
    if opts.retain > 0 {
        let chord_pcs = chord.pitch_classes(scale);
        let mut cands: Vec<(usize, i32)> = from
            .voices
            .iter()
            .enumerate()
            .filter_map(|(i, (_, cur))| {
                let pc = Pc::from_offset(scale.root, *cur);
                let moved = (out[i].1 - cur).abs();
                (chord_pcs.contains(&pc) && moved > 0).then_some((i, moved))
            })
            .collect();
        cands.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for &(i, _) in cands.iter().take(opts.retain) {
            out[i].1 = from.voices[i].1;
        }
    }

    Voicing { voices: out }
}

/// What a voice that moves at all costs on top of its distance in [`lead_pcs`], in semitones:
/// holding a common tone beats sliding two voices a semitone each.
const MOVE_COST: i32 = 1;

/// Voice-lead `from` onto an arbitrary pitch-class set: a chord read from a file rather than a
/// diatonic [`Chord`] (`engines-midi parts`, ADR 0053). Each voice gets an offset in `range`
/// (semitones from `root`, inclusive) whose pitch class is in `pcs`, no two voices on one pitch,
/// with every pitch class of `required` sounded (in its order of priority, when there are fewer
/// voices than it lists). Of those voicings, the least total motion wins, plus [`MOVE_COST`] per
/// voice that moves; ties go to the lower voicing. Voices keep their ids and their order
/// (sorted-to-sorted matching, so they never cross). With fewer pitches in range than voices, the
/// extra voices double the nearest one.
pub fn lead_pcs(from: &Voicing, root: Pc, pcs: &[Pc], required: &[Pc], range: (i32, i32)) -> Voicing {
    let v = from.voices.len();
    if v == 0 || pcs.is_empty() {
        return from.clone();
    }
    let cands: Vec<i32> = (range.0..=range.1).filter(|&o| pcs.contains(&Pc::from_offset(root, o))).collect();
    let mut need: Vec<Pc> = Vec::new();
    for pc in required {
        if pcs.contains(pc) && !need.contains(pc) {
            need.push(*pc);
        }
    }
    need.truncate(v);
    // Voices by current pitch (index breaks ties), matched in order to a sorted voicing.
    let mut order: Vec<usize> = (0..v).collect();
    order.sort_by_key(|&i| (from.voices[i].1, i));
    let cost = |voicing: &[i32]| -> i32 {
        order.iter().zip(voicing).map(|(&i, &o)| (o - from.voices[i].1).abs() + if o == from.voices[i].1 { 0 } else { MOVE_COST }).sum()
    };
    let mut best: Option<(i32, Vec<i32>)> = None;
    if cands.len() >= v {
        let mut idx: Vec<usize> = (0..v).collect();
        loop {
            let voicing: Vec<i32> = idx.iter().map(|&k| cands[k]).collect();
            if need.iter().all(|pc| voicing.iter().any(|&o| Pc::from_offset(root, o) == *pc)) {
                let c = cost(&voicing);
                if best.as_ref().is_none_or(|(bc, bv)| (c, &voicing) < (*bc, bv)) {
                    best = Some((c, voicing));
                }
            }
            // The next combination, in lexicographic order.
            let Some(k) = (0..v).rev().find(|&k| idx[k] < cands.len() - v + k) else { break };
            idx[k] += 1;
            for j in k + 1..v {
                idx[j] = idx[j - 1] + 1;
            }
        }
    }
    let voicing = match best {
        Some((_, voicing)) => voicing,
        // Too few pitches in range (or none): each voice takes the nearest one there is.
        None if cands.is_empty() => return from.clone(),
        None => {
            let mut vs: Vec<i32> = order.iter().map(|&i| *cands.iter().min_by_key(|&&o| ((o - from.voices[i].1).abs(), o)).unwrap()).collect();
            vs.sort_unstable();
            vs
        }
    };
    let mut out = from.voices.clone();
    for (&i, &o) in order.iter().zip(&voicing) {
        out[i].1 = o;
    }
    Voicing { voices: out }
}

/// `v` target offsets for `chord`, centered near `center`, sorted ascending.
fn target_offsets(chord: &Chord, scale: &Scale, v: usize, center: i32) -> Vec<i32> {
    let mut tones = chord.tones(scale);
    if tones.is_empty() {
        return vec![center; v];
    }
    // Octave-shift the whole stack so its mean sits near `center`.
    let mean = tones.iter().sum::<i32>() / tones.len() as i32;
    let shift = (((center - mean) as f32) / 12.0).round() as i32 * 12;
    for t in tones.iter_mut() {
        *t += shift;
    }
    // Pad up to `v` by octave-doubling from the bottom; truncate if there are fewer voices.
    let base = tones.clone();
    let mut k = 0;
    while tones.len() < v {
        let add = base[k % base.len()] + 12 * (1 + (k / base.len()) as i32);
        tones.push(add);
        k += 1;
    }
    tones.truncate(v);
    tones.sort_unstable();
    tones
}

#[cfg(test)]
mod tests {
    use super::super::chord::Extension;
    use super::*;

    fn dorian() -> Scale {
        Scale::from_harmony("D", "dorian")
    }

    #[test]
    fn common_tones_barely_move() {
        // Dm7 (0 3 7 10) → its own re-voicing should be (near) motionless.
        let s = dorian();
        let dm7 = Chord::diatonic(0, Extension::Seventh);
        let start = Voicing::from_pairs(vec![("v1", 0), ("v2", 3), ("v3", 7), ("v4", 10)]);
        let led = lead(&start, &dm7, &s, &LeadOpts { register_center: 5, pedal: None, retain: 0 });
        assert!(led.movement_from(&start) <= 4, "tiny move, got {}", led.movement_from(&start));
        assert_eq!(led.voices.len(), 4, "voice count preserved");
    }

    #[test]
    fn chord_change_uses_small_steps_and_keeps_ids() {
        let s = dorian();
        let start = Voicing::from_pairs(vec![("v1", 0), ("v2", 3), ("v3", 7), ("v4", 10)]); // Dm7
        // → the bright dorian IV (Gmaj7-ish, degree 3 diatonic 7th)
        let g = Chord::diatonic(3, Extension::Seventh);
        let led = lead(&start, &g, &s, &LeadOpts { register_center: 7, pedal: None, retain: 0 });
        // ids are stable and the move is gentle (no big leaps).
        let ids: Vec<&str> = led.voices.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["v1", "v2", "v3", "v4"]);
        let max_jump = led
            .voices
            .iter()
            .map(|(id, o)| (o - start.voices.iter().find(|(p, _)| p == id).unwrap().1).abs())
            .max()
            .unwrap();
        assert!(max_jump <= 7, "each voice moves ≤ a fifth, got {max_jump}");
        // every voice lands on a chord tone
        for (_, o) in &led.voices {
            let pc = Pc::from_offset(s.root, *o);
            assert!(g.pitch_classes(&s).contains(&pc), "voice {o} is a chord tone");
        }
    }

    #[test]
    fn pedal_voice_is_held() {
        let s = dorian();
        // hold E5 (offset 14) on v4 across a change where E is a chord tone.
        let start = Voicing::from_pairs(vec![("v1", 0), ("v2", 7), ("v3", 10), ("v4", 14)]);
        // Cmaj7 in D dorian = bVII (degree 6 diatonic 7th): C E G B — contains E (pc 4).
        let c = Chord::diatonic(6, Extension::Seventh);
        let led = lead(&start, &c, &s, &LeadOpts { register_center: 7, pedal: Some("v4".into()), retain: 0 });
        let v4 = led.voices.iter().find(|(id, _)| id == "v4").unwrap().1;
        assert_eq!(v4, 14, "pedal voice holds E5");
    }

    #[test]
    fn retention_never_increases_motion_and_is_deterministic() {
        // Common-tone retention (the governor's lever) can only reduce or equal voice motion, and
        // `retain == 0` is the pre-governor behavior.
        let s = dorian();
        let start = Voicing::from_pairs(vec![("v1", 0), ("v2", 7), ("v3", 10), ("v4", 14)]);
        let target = Chord::diatonic(3, Extension::Seventh); // the bright dorian IV
        let none = lead(&start, &target, &s, &LeadOpts { register_center: 7, pedal: None, retain: 0 });
        let held = lead(&start, &target, &s, &LeadOpts { register_center: 7, pedal: None, retain: 4 });
        assert!(
            held.movement_from(&start) <= none.movement_from(&start),
            "retention never increases motion: held={} none={}",
            held.movement_from(&start),
            none.movement_from(&start)
        );
        assert_eq!(held.voices.len(), 4, "voice count preserved under retention");
        let held2 = lead(&start, &target, &s, &LeadOpts { register_center: 7, pedal: None, retain: 4 });
        assert_eq!(held.voices, held2.voices, "deterministic");
    }

    fn pcs(names: &[&str]) -> Vec<Pc> {
        names.iter().map(|n| crate::theory::pc_of_root(n)).collect()
    }

    #[test]
    fn lead_pcs_holds_common_tones_and_drops_the_optional_fifth() {
        // From the owner's MIDI sketch, in D: Am(add9) as written (A3 C4 E4 B4) → G6 → D.
        let d = crate::theory::pc_of_root("D");
        let am = Voicing::from_pairs(vec![("v1", -5), ("v2", -2), ("v3", 2), ("v4", 9)]);
        // G6 = G B D E, its fifth (D) optional: E4 and B4 hold, A→G and C→B step down.
        let g6 = lead_pcs(&am, d, &pcs(&["G", "B", "E", "D"]), &pcs(&["G", "B", "E"]), (-12, 14));
        assert_eq!(g6.offsets(), vec![-7, -3, 2, 9], "{g6:?}");
        assert_eq!(g6.voices.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["v1", "v2", "v3", "v4"], "ids keep their order");
        // D = D F# A, the fifth optional: every voice lands on D or F# or A, by small steps.
        let dd = lead_pcs(&g6, d, &pcs(&["D", "F#", "A"]), &pcs(&["D", "F#"]), (-12, 14));
        for (_, o) in &dd.voices {
            assert!(pcs(&["D", "F#", "A"]).contains(&Pc::from_offset(d, *o)), "{dd:?}");
        }
        assert!(dd.movement_from(&g6) <= 8, "{dd:?}");
        // Deterministic.
        assert_eq!(dd, lead_pcs(&g6, d, &pcs(&["D", "F#", "A"]), &pcs(&["D", "F#"]), (-12, 14)));
    }

    #[test]
    fn lead_pcs_sounds_what_is_required_and_stays_in_range() {
        let d = crate::theory::pc_of_root("D");
        let from = Voicing::from_pairs(vec![("v1", 0), ("v2", 4), ("v3", 7), ("v4", 12)]);
        // Am(add9) with every tone but the fifth required: A, C and B all sound.
        let led = lead_pcs(&from, d, &pcs(&["A", "C", "E", "B"]), &pcs(&["A", "C", "B"]), (-5, 12));
        let got: Vec<Pc> = led.voices.iter().map(|(_, o)| Pc::from_offset(d, *o)).collect();
        for pc in pcs(&["A", "C", "B"]) {
            assert!(got.contains(&pc), "{led:?}");
        }
        assert!(led.voices.iter().all(|(_, o)| (-5..=12).contains(o)), "{led:?}");
        let mut offs = led.offsets();
        offs.dedup();
        assert_eq!(offs.len(), 4, "no two voices on one pitch: {led:?}");
        // Fewer voices than required tones: the first ones in priority order.
        let one = lead_pcs(&Voicing::from_pairs(vec![("b", -12)]), d, &pcs(&["G", "B", "D"]), &pcs(&["G", "B"]), (-24, -1));
        assert_eq!(Pc::from_offset(d, one.voices[0].1), crate::theory::pc_of_root("G"), "{one:?}");
        // No voices, or nothing in range: unchanged.
        assert_eq!(lead_pcs(&Voicing::default(), d, &pcs(&["A"]), &[], (0, 12)), Voicing::default());
        let none = lead_pcs(&from, d, &pcs(&["A"]), &[], (0, 3));
        assert_eq!(none, from);
        // Too few pitches in range for every voice: the nearest, doubled.
        let tight = lead_pcs(&from, d, &pcs(&["A"]), &[], (0, 8));
        assert!(tight.voices.iter().all(|(_, o)| *o == 7), "{tight:?}");
    }

    #[test]
    fn fewer_voices_than_tones_still_voices_a_chord() {
        let s = dorian();
        let start = Voicing::from_pairs(vec![("b", -12)]); // a single bass voice
        let dm7 = Chord::diatonic(0, Extension::Seventh);
        let led = lead(&start, &dm7, &s, &LeadOpts { register_center: -12, pedal: None, retain: 0 });
        assert_eq!(led.voices.len(), 1);
        let pc = Pc::from_offset(s.root, led.voices[0].1);
        assert!(dm7.pitch_classes(&s).contains(&pc));
    }
}
