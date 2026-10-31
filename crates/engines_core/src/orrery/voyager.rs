//! The Voyager: the body that traverses the field. Each phrase it picks an orbital transfer
//! (musical prior + gravity + harmonic preference + a seeded draw), voice-leads the pad to the
//! new chord, places the bass, and regenerates the melody — all expressed as semitone offsets.

use super::field::Field;
use super::gravity::{Gravity, HarmonicPreference};
use serde::{Deserialize, Serialize};
use crate::hash::{hash_site, hash_to_unit};
use crate::theory::chord_fit::chord_fit;
use crate::theory::function::{function_of, resolution_targets, Function};
use crate::theory::melody::{next_phrase, MelodyParams, Motif};
use crate::theory::melody_fit::melody_fit;
use crate::theory::pitch::Pc;
use crate::theory::voicelead::{lead, LeadOpts, Voicing};
use crate::theory::Scale;

/// A discrete Orrery harmonic transfer (the slow-layer move), recorded per loop in the
/// [`LoopRecord`](crate::telemetry::LoopRecord) stream. `default_chord` is the gravity-default the
/// theory alone would have picked (the argmax, kept for diagnostics).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HarmonicMove {
    pub chord: String,
    pub tension: f32,
    pub default_chord: String,
}

/// Weight on the leader-fit term when a returned-home track constrains the chord (Phase 5b). Set
/// dominant over the gravity/preference terms so the chord genuinely fits the leader, while the
/// softmax + voice-leading still break ties among well-fitting chords (no bland argmax).
const LEADER_FIT_WEIGHT: f32 = 3.0;

/// How a returned-home **leader** track constrains this phrase's chord choice (Phase 5b). A pedal bass
/// (scale-degree 1 or 5) imposes *no* `LeaderFit` — the chord voyages freely above the held pedal.
pub enum LeaderFit {
    /// Harmonize a fixed melody (an arp leader) — quality-aware via `melody_fit`.
    Melody(Vec<Pc>),
    /// Bias toward chords that contain this bass pitch-class as a chord tone (a non-pedal bass leader).
    BassChordTone(Pc),
}

/// Per-phrase options for [`Voyager::advance`] (Phase 5b). The default reproduces the plain
/// chord-led voyage exactly (no leader, no forced return).
#[derive(Default)]
pub struct AdvanceOpts<'a> {
    /// A returned-home leader the chord must fit (overrides the drone-context fit term).
    pub leader: Option<&'a LeaderFit>,
    /// Cadence-aware return: when the current chord is poised to resolve, steer the transfer home.
    pub want_return: bool,
}

/// The concrete musical content for one phrase, ready to write into a loop doc.
#[derive(Clone, Debug)]
pub struct PhrasePlan {
    /// Pad voicing: stable voice ids → semitone offsets (the engine glides between phrases).
    pub pad: Voicing,
    /// Bass offset (semitones from root), placed in a low register.
    pub bass: i32,
    /// Melody pool (semitone offsets) for the arp's `pitch_walk`.
    pub melody: Vec<i32>,
    /// Current chord tones (semitone offsets from root) — the palette for one-shot accents (WS4.3).
    pub chord_tones: Vec<i32>,
    /// Upper-structure **color tones** (the 9th/11th/13th, semitone offsets) — the palette for
    /// *differentiated* accents (Phase 6b): a high sparkle distinct from the arp's chord-tone runs.
    pub color_tones: Vec<i32>,
}

pub struct Voyager {
    field: Field,
    current: usize,
    pad: Voicing,
    bass: i32,
    melody: Vec<i32>,
    motif: Motif,
    pedal_id: Option<String>,
    mel_len: usize,
    loops_since_home: u32,
    /// Common-tone retention level fed to `lead` (set by the Phase 5 governor; 0 ⇒ off).
    retain: usize,
    /// Pad voices that changed pitch on the last `advance` (the governor's measured signal).
    last_changes: u32,
}

impl Voyager {
    /// Start at home (the tonic) with `pad_ids` voicing it, `mel_len` melody notes.
    pub fn new(scale: Scale, pad_ids: &[String], pref: &HarmonicPreference, mel_len: usize, seed: u64) -> Voyager {
        let field = Field::build(scale);
        let current = field.tonic_index();
        let chord = field.nodes[current].chord;
        let pedal_id = pad_ids.last().cloned();

        let seed_pad = Voicing::from_pairs(pad_ids.iter().map(|id| (id.clone(), 0)));
        let pad = lead(&seed_pad, &chord, &scale, &LeadOpts { register_center: pref.pad_center, pedal: None, retain: 0 });
        let bass = place_low(chord.tones(&scale)[0], -17);
        let (melody, motif) = next_phrase(&scale, &chord, &Motif::default(), &mel_params(pref), seed, mel_len.max(1));

        Voyager { field, current, pad, bass, melody, motif, pedal_id, mel_len: mel_len.max(1), loops_since_home: 0, retain: 0, last_changes: 0 }
    }

    /// Current common-tone retention level (Phase 5 governor).
    pub fn retain(&self) -> usize {
        self.retain
    }
    /// Set the common-tone retention level applied on the next `advance` (governor feedback).
    pub fn set_retain(&mut self, n: usize) {
        self.retain = n;
    }
    /// Pad voices that changed pitch on the last `advance` — the governor's measured signal.
    pub fn last_voice_changes(&self) -> u32 {
        self.last_changes
    }

    pub fn plan(&self) -> PhrasePlan {
        let node = &self.field.nodes[self.current];
        let scale = &self.field.scale;
        let chord = node.chord;
        PhrasePlan {
            pad: self.pad.clone(),
            bass: self.bass,
            melody: self.melody.clone(),
            chord_tones: chord.tones(scale),
            // Upper structure: the 9th/11th/13th are diatonic thirds stacked two octaves up
            // (degree + 8/10/12) — in-scale by construction (Lydian's #11 falls out for free).
            color_tones: [8, 10, 12].iter().map(|&d| scale.degree_semitone(chord.degree + d)).collect(),
        }
    }

    pub fn loops_since_home(&self) -> u32 {
        self.loops_since_home
    }

    /// Current chord label (e.g. `"Gmaj7"`) — for telemetry/UI.
    pub fn current_chord(&self) -> String {
        let scale = self.field.scale;
        self.field.nodes[self.current].chord.label(&scale)
    }

    /// Current chord tension (orbital radius, 0..1) — for telemetry/UI.
    pub fn current_tension(&self) -> f32 {
        self.field.nodes[self.current].tension
    }

    /// Consonance of the current chord with a fixed-pitch context (WS4.2) — for tests/telemetry.
    pub fn current_fit(&self, fixed: &[Pc]) -> f32 {
        chord_fit(&self.field.nodes[self.current].chord, &self.field.scale, fixed)
    }

    /// Quality-aware harmonization of the current chord for a fixed melody (Phase 5b) — for tests.
    pub fn current_melody_fit(&self, fixed: &[Pc]) -> f32 {
        melody_fit(&self.field.nodes[self.current].chord, &self.field.scale, fixed)
    }

    /// Advance one phrase: choose + apply the next transfer. `fixed` is the read-only fixed-pitch
    /// context (held drones) the voyage should stay consonant with (WS4.2; empty ⇒ unconstrained).
    /// `seed` should be unique per phrase (the controller mixes it from the session seed + phrase
    /// index). Returns the move record.
    pub fn advance(&mut self, pref: &HarmonicPreference, gravity: &Gravity, fixed: &[Pc], adv: &AdvanceOpts, seed: u64) -> HarmonicMove {
        let scale = self.field.scale;
        let from = self.current;
        let from_chord = self.field.nodes[from].chord;
        let targets = resolution_targets(&from_chord, &scale);
        let opts = LeadOpts { register_center: pref.pad_center, pedal: self.pedal_id.clone(), retain: self.retain };

        // Score every node. Track the gravity-default (prior + pull only) for diagnostics.
        let gamma = gravity.gamma(self.loops_since_home);
        let mut scores = Vec::with_capacity(self.field.nodes.len());
        let mut default_idx = from;
        let mut default_best = f32::MIN;
        for (i, node) in self.field.nodes.iter().enumerate() {
            let mv = lead(&self.pad, &node.chord, &scale, &opts).movement_from(&self.pad) as f32;
            let smooth = -mv / 24.0;
            let is_res = targets.iter().any(|t| t.pitch_classes(&scale) == node.chord.pitch_classes(&scale));
            let prior = gravity.mu * (smooth + if is_res { 0.5 } else { 0.0 });
            let pull = -gamma * node.tension;
            // Fit term. With a returned-home **leader** (Phase 5b), the chord must fit the leader's
            // authored notes — quality-aware `melody_fit` for an arp leader, `chord_fit` containment
            // for a bass-chord-tone leader — at a dominant weight. Otherwise it's the WS4.2 harmonic
            // floor: consonance with held drones. Folded into `base` so the gravity-default
            // respects the same constraint (D1).
            let fit = match adv.leader {
                Some(LeaderFit::Melody(pcs)) => LEADER_FIT_WEIGHT * melody_fit(&node.chord, &scale, pcs),
                Some(LeaderFit::BassChordTone(b)) => LEADER_FIT_WEIGHT * chord_fit(&node.chord, &scale, &[*b]),
                None => gravity.w_fit * chord_fit(&node.chord, &scale, fixed),
            };
            let base = prior + pull + fit; // theory + gravity + (leader or drone) consonance
            if base > default_best {
                default_best = base;
                default_idx = i;
            }
            let pref_term = -gravity.pref_weight * (node.tension - pref.tension_target).abs();
            let rep = if i == from { -gravity.repetition } else { 0.0 };
            scores.push(base + pref_term + rep);
        }

        // Cadence-aware return (Phase 5b): when asked to return *and* the current chord is poised to
        // resolve home (the tonic is a resolution target, or we're sitting on a dominant), boost the
        // tonic so the softmax lands home — a **prepared** return through the cadence, not a teleport.
        // If not poised, the return waits (gravity keeps pulling) until a later, cadential phrase.
        if adv.want_return {
            let home = self.field.tonic_index();
            let home_pcs = self.field.nodes[home].chord.pitch_classes(&scale);
            let poised = targets.iter().any(|t| t.pitch_classes(&scale) == home_pcs)
                || function_of(&from_chord, &scale) == Function::Dominant;
            if poised {
                scores[home] += 10.0;
            }
        }

        let u = hash_to_unit(hash_site(seed, "transfer", 0, ""));
        let chosen = softmax_pick(&scores, pref.adventure, u);

        // Apply the chosen transfer.
        self.current = chosen;
        self.loops_since_home = if self.field.is_tonic(chosen) { 0 } else { self.loops_since_home + 1 };
        let chord = self.field.nodes[chosen].chord;
        let prev_pad = self.pad.clone();
        self.pad = lead(&self.pad, &chord, &scale, &opts);
        self.last_changes = count_changes(&prev_pad, &self.pad);
        self.bass = place_low(chord.tones(&scale)[0], -17);
        let (melody, motif) = next_phrase(&scale, &chord, &self.motif, &mel_params(pref), seed, self.mel_len);
        self.melody = melody;
        self.motif = motif;

        HarmonicMove {
            chord: chord.label(&scale),
            tension: self.field.nodes[chosen].tension,
            default_chord: self.field.nodes[default_idx].chord.label(&scale),
        }
    }

    /// Force a **global return home** (a hard `tension_max` response, WS5.4): voice-lead the pad to
    /// the tonic chord, place the bass, reset the time-away counter, and — when `regen_melody` — also
    /// regenerate the melody. `home == per_modulator` passes `regen_melody == false` so the harmony
    /// resolves while the melodic surface keeps roaming its current pool (a single sequence resolves
    /// while others roam). Returns the move record (chosen == default == the tonic).
    pub fn return_home(&mut self, pref: &HarmonicPreference, regen_melody: bool, seed: u64) -> HarmonicMove {
        let scale = self.field.scale;
        let home = self.field.tonic_index();
        let chord = self.field.nodes[home].chord;
        let opts = LeadOpts { register_center: pref.pad_center, pedal: self.pedal_id.clone(), retain: self.retain };
        let prev_pad = self.pad.clone();
        self.current = home;
        self.pad = lead(&self.pad, &chord, &scale, &opts);
        self.last_changes = count_changes(&prev_pad, &self.pad);
        self.bass = place_low(chord.tones(&scale)[0], -17);
        if regen_melody {
            let (melody, motif) = next_phrase(&scale, &chord, &self.motif, &mel_params(pref), seed, self.mel_len);
            self.melody = melody;
            self.motif = motif;
        }
        self.loops_since_home = 0;
        let tension = self.field.nodes[home].tension;
        HarmonicMove { chord: chord.label(&scale), tension, default_chord: chord.label(&scale) }
    }
}

/// Count pad voices whose offset changed between two voicings (shared ids; a new id counts as a change).
fn count_changes(prev: &Voicing, cur: &Voicing) -> u32 {
    cur.voices
        .iter()
        .filter(|(id, off)| prev.voices.iter().find(|(pid, _)| pid == id).map(|(_, po)| po != off).unwrap_or(true))
        .count() as u32
}

fn mel_params(pref: &HarmonicPreference) -> MelodyParams {
    MelodyParams { range: 6, consonance: 0.6, register: pref.mel_register, direction: 0.0 }
}

/// Place a pitch-class offset in a low register near `target` semitones.
fn place_low(off: i32, target: i32) -> i32 {
    let mut o = off.rem_euclid(12);
    while o > target + 6 {
        o -= 12;
    }
    while o < target - 6 {
        o += 12;
    }
    o
}

/// Seeded softmax sample over `scores` with temperature `temp` and uniform draw `u` ∈ [0,1).
fn softmax_pick(scores: &[f32], temp: f32, u: f32) -> usize {
    if scores.is_empty() {
        return 0;
    }
    let t = temp.max(0.05);
    let max = scores.iter().cloned().fold(f32::MIN, f32::max);
    let exps: Vec<f32> = scores.iter().map(|s| ((s - max) / t).exp()).collect();
    let sum: f32 = exps.iter().sum();
    let target = u * sum;
    let mut acc = 0.0;
    for (i, e) in exps.iter().enumerate() {
        acc += e;
        if acc >= target {
            return i;
        }
    }
    scores.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> Vec<String> {
        vec!["v1".into(), "v2".into(), "v3".into(), "v4".into()]
    }

    #[test]
    fn starts_at_home() {
        let s = Scale::from_harmony("D", "dorian");
        let v = Voyager::new(s, &ids(), &HarmonicPreference::default(), 6, 1);
        assert_eq!(v.loops_since_home(), 0);
        assert_eq!(v.plan().pad.voices.len(), 4);
        assert_eq!(v.plan().melody.len(), 6);
    }

    #[test]
    fn is_deterministic_for_a_fixed_seed() {
        let s = Scale::from_harmony("A", "aeolian");
        let mk = || {
            let mut v = Voyager::new(s, &ids(), &HarmonicPreference::default(), 6, 99);
            let mut moves = Vec::new();
            for ph in 0..12 {
                moves.push(v.advance(&HarmonicPreference::default(), &Gravity::default(), &[], &AdvanceOpts::default(), 99 + ph));
            }
            moves.iter().map(|m| m.chord.clone()).collect::<Vec<_>>()
        };
        assert_eq!(mk(), mk(), "same seed → identical voyage");
    }

    #[test]
    fn gravity_returns_home_and_bounds_tension() {
        // A long neutral voyage must keep returning home (gravity) rather than drifting away.
        let s = Scale::from_harmony("D", "dorian");
        let mut v = Voyager::new(s, &ids(), &HarmonicPreference::default(), 6, 7);
        let mut max_away = 0u32;
        let mut visited_home = 0;
        for ph in 0..40u64 {
            v.advance(&HarmonicPreference::default(), &Gravity::default(), &[], &AdvanceOpts::default(), 7 + ph);
            max_away = max_away.max(v.loops_since_home());
            if v.loops_since_home() == 0 {
                visited_home += 1;
            }
        }
        assert!(visited_home >= 4, "should resolve home repeatedly, got {visited_home} returns");
        assert!(max_away <= 8, "gravity should cap how far we wander, max_away={max_away}");
    }

    #[test]
    fn fixed_pitch_context_biases_toward_consonant_chords() {
        // A held drone should pull the voyage toward chords consonant with it. Score every visited
        // chord against the drone; the drone-aware voyage must average a higher fit than one that
        // ignored it (same seed / scale).
        let scale = Scale::from_harmony("D", "dorian");
        let drone = [Pc(9)]; // a held A (the 5th)
        let mean_fit = |gravity: &Gravity, fixed: &[Pc]| {
            let mut v = Voyager::new(scale, &ids(), &HarmonicPreference::default(), 6, 5);
            let (mut sum, n) = (0.0f32, 40u64);
            for ph in 0..n {
                v.advance(&HarmonicPreference::default(), gravity, fixed, &AdvanceOpts::default(), 5 + ph);
                sum += v.current_fit(&drone); // always scored against the drone
            }
            sum / n as f32
        };
        let aware = mean_fit(&Gravity { w_fit: 3.0, ..Default::default() }, &drone);
        let blind = mean_fit(&Gravity::default(), &[]); // empty fixed ⇒ fit term inert
        assert!(aware > blind, "drone-aware voyage is more consonant: aware={aware} blind={blind}");
    }

    // ── Phase 5b: leader-constrained chord choice + cadence-aware return ──────

    #[test]
    fn melody_leader_chord_harmonizes_the_melody() {
        let scale = Scale::from_harmony("D", "dorian");
        let pref = HarmonicPreference::default();
        let mel = vec![Pc(4), Pc(9)]; // E and A
        let leader = LeaderFit::Melody(mel.clone());
        let mean_fit = |lead: Option<&LeaderFit>| {
            let mut v = Voyager::new(scale, &ids(), &pref, 6, 5);
            let (mut sum, n) = (0.0f32, 10u64);
            for ph in 0..n {
                v.advance(&pref, &Gravity::default(), &[], &AdvanceOpts { leader: lead, want_return: false }, 5 + ph);
                sum += v.current_melody_fit(&mel);
            }
            sum / n as f32
        };
        let with_leader = mean_fit(Some(&leader));
        let without = mean_fit(None);
        assert!(with_leader > without, "melody-led chords harmonize the melody better: {with_leader} vs {without}");
    }

    #[test]
    fn bass_chord_tone_leader_biases_chords_containing_the_bass() {
        let scale = Scale::from_harmony("D", "dorian");
        let pref = HarmonicPreference::default();
        let b = Pc(5); // F
        let leader = LeaderFit::BassChordTone(b);
        let contains_frac = |lead: Option<&LeaderFit>| {
            let mut v = Voyager::new(scale, &ids(), &pref, 6, 9);
            let (mut hits, n) = (0u32, 12u64);
            for ph in 0..n {
                v.advance(&pref, &Gravity::default(), &[], &AdvanceOpts { leader: lead, want_return: false }, 9 + ph);
                if v.current_fit(&[b]) > 0.9 {
                    hits += 1; // chord_fit ≈ 1.0 iff the bass pc is a chord tone
                }
            }
            hits as f32 / n as f32
        };
        let with_leader = contains_frac(Some(&leader));
        let without = contains_frac(None);
        assert!(with_leader > without, "bass-chord-tone leader favors chords containing the bass: {with_leader} vs {without}");
    }

    #[test]
    fn cadence_aware_return_pulls_home() {
        let scale = Scale::from_harmony("D", "dorian");
        let pref = HarmonicPreference::default();
        let mean_lsh = |want_return: bool| {
            let mut v = Voyager::new(scale, &ids(), &pref, 6, 7);
            let (mut sum, n) = (0u32, 40u64);
            for ph in 0..n {
                v.advance(&pref, &Gravity::default(), &[], &AdvanceOpts { leader: None, want_return }, 7 + ph);
                sum += v.loops_since_home();
            }
            sum as f32 / n as f32
        };
        let with_return = mean_lsh(true);
        let without = mean_lsh(false);
        assert!(with_return < without, "cadence-aware return spends less time away from home: {with_return} vs {without}");
    }

    #[test]
    fn return_home_resolves_to_the_tonic_and_home_scope_differs() {
        // A forced return lands on the tonic (loops_since_home == 0). `per_modulator` keeps the
        // roaming melody; `global` regenerates it.
        let scale = Scale::from_harmony("D", "dorian");
        let pref = HarmonicPreference::default();
        let weak = Gravity { gamma0: 0.0, gamma_growth: 0.0, ..Default::default() };
        let away = || {
            let mut v = Voyager::new(scale, &ids(), &pref, 6, 5);
            for ph in 0..4u64 {
                v.advance(&pref, &weak, &[], &AdvanceOpts::default(), 5 + ph);
            }
            v
        };

        let mut pm = away();
        let mel = pm.plan().melody.clone();
        pm.return_home(&pref, false, 77); // per_modulator
        assert_eq!(pm.loops_since_home(), 0, "harmony resolved home");
        assert_eq!(pm.plan().melody, mel, "per_modulator keeps the roaming melody");

        let mut gl = away();
        let gmel = gl.plan().melody.clone();
        gl.return_home(&pref, true, 77); // global
        assert_eq!(gl.loops_since_home(), 0);
        assert_ne!(gl.plan().melody, gmel, "global return regenerates the melody from home");
    }

    #[test]
    fn calm_preference_holds_lower_tension_than_restless() {
        let s = Scale::from_harmony("D", "dorian");
        let mean_tension = |pref: HarmonicPreference| {
            let mut v = Voyager::new(s, &ids(), &pref, 6, 3);
            let mut sum = 0.0;
            let n = 30;
            for ph in 0..n {
                sum += v.advance(&pref, &Gravity::default(), &[], &AdvanceOpts::default(), 3 + ph as u64).tension;
            }
            sum / n as f32
        };
        let calm = mean_tension(HarmonicPreference { tension_target: 0.1, ..Default::default() });
        let restless = mean_tension(HarmonicPreference { tension_target: 0.7, adventure: 0.6, ..Default::default() });
        assert!(calm < restless, "calm {calm} should sit lower than restless {restless}");
    }
}
