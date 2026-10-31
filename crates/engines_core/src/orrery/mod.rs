//! The Orrery — generative harmonic-morphing layer (the slow, discrete half of the CE-R8
//! hybrid). A [`Voyager`] traverses a [`Field`] of chords orbiting the tonic; [`write_phrase`]
//! realizes each phrase into a loop doc's note modulators (so the engine's existing glide does
//! the morph). Voice presence is an Orrery concern too, but it is expressed by writing the bound
//! note modulator (a blank / release-only sequence), never by toggling `instruments.enabled`
//! (the axis seam, ADR 0017) — the narrative that drives it lands in Phase 6c.
//!
//! See `doc/decisions/orrery-generative-harmony-design.md`. Pure of I/O; all randomness is
//! seeded via [`crate::hash`] so CE-R7.3 bit-reproducibility holds.

pub mod arranger;
pub mod field;
pub mod gravity;
pub mod voyager;

pub use arranger::{ArrangePlan, Arranger};
pub use field::{Field, FieldNode};
pub use gravity::{pad_center_for, Gravity, HarmonicPreference, PAD_CENTER};
pub use voyager::{AdvanceOpts, HarmonicMove, LeaderFit, PhrasePlan, Voyager};

use crate::hash::{hash_site, hash_to_unit};
use crate::loop_doc::{LoopDoc, NoteVoice, PhraseDef, Step};
use crate::theory::phrase_fit::NoteRange;

/// Which modulators/instruments the Orrery drives in a loop doc. Defaults match the demo
/// "orrery seed" docs (and the v0.7 fixture's ids).
#[derive(Clone, Debug)]
pub struct OrreryConfig {
    /// Primary note step_seq id that voices the pad chord.
    pub chord_mod: String,
    /// Note step_seq id that voices the bass.
    pub bass_mod: String,
    /// Primary note step_seq id (euclid generator + pitch_walk) that plays the melody/arp.
    pub arp_mod: String,
    /// Additional `chord`-role tracks beyond the primary (Phase 6c multi-track) — each is voiced with
    /// the same chord, octave-staggered by index, so two pads/keys layer the harmony in distinct
    /// registers/timbres.
    pub extra_chord_mods: Vec<String>,
    /// Additional `arp`-role tracks beyond the primary (Phase 6c) — each gets the melody pool,
    /// octave-staggered by index (interlocking arps in different registers).
    pub extra_arp_mods: Vec<String>,
    /// Stable pad voice ids (kept across phrases so the engine glides).
    pub pad_ids: Vec<String>,
    /// Bass voice id.
    pub bass_id: String,
    /// One-shot note modulators that receive seeded chord-tone accents (Phase 4; `oneshot` role
    /// or `bind.oneshot`). Empty ⇒ no accent surface.
    pub oneshot_mods: Vec<String>,
    /// Read-only fixed-pitch context: note modulators flagged `orrery_disable` or `orrery_role:
    /// context`. Never rewritten; their sounding pitches bias the voyage (Phase 4 harmonic floor).
    pub context_mods: Vec<String>,
    /// Melody pool length.
    pub mel_len: usize,
    /// Loops per phrase (how often a chord change happens).
    pub phrase_loops: u64,
}

impl Default for OrreryConfig {
    fn default() -> Self {
        Self {
            chord_mod: "chords".into(),
            bass_mod: "bass_line".into(),
            arp_mod: "arp_shimmer".into(),
            extra_chord_mods: Vec::new(),
            extra_arp_mods: Vec::new(),
            pad_ids: vec!["v1".into(), "v2".into(), "v3".into(), "v4".into()],
            bass_id: "b".into(),
            oneshot_mods: Vec::new(),
            context_mods: Vec::new(),
            mel_len: 6,
            phrase_loops: 1,
        }
    }
}

impl OrreryConfig {
    /// All `chord`-role track ids (primary + extras) — Phase 6c multi-track.
    pub fn chord_mods(&self) -> Vec<String> {
        std::iter::once(self.chord_mod.clone()).chain(self.extra_chord_mods.iter().cloned()).collect()
    }
    /// All `arp`-role track ids (primary + extras).
    pub fn arp_mods(&self) -> Vec<String> {
        std::iter::once(self.arp_mod.clone()).chain(self.extra_arp_mods.iter().cloned()).collect()
    }
    /// Every modulator the Orrery writes into (chord, bass, arp and one-shot tracks).
    pub fn written_modulators(&self) -> Vec<String> {
        ["chord", "bass", "arp", "oneshot"].into_iter().flat_map(|r| self.track_modulators(r)).collect()
    }
    /// The modulator id(s) realizing a track role (`chord`/`bass`/`arp`/`oneshot`) — Phase 5b uses
    /// this to rest a returned-home track, map per-track clocks, and (Phase 6c) drop a whole role.
    pub fn track_modulators(&self, role: &str) -> Vec<String> {
        match role {
            "chord" => self.chord_mods(),
            "bass" => vec![self.bass_mod.clone()],
            "arp" => self.arp_mods(),
            "oneshot" => self.oneshot_mods.clone(),
            _ => Vec::new(),
        }
    }
}

/// A single held step that fires the given id-keyed voicing at the loop start.
fn single_step(voicing: impl IntoIterator<Item = (String, f32)>) -> Vec<Step> {
    let voicing = voicing
        .into_iter()
        .map(|(id, off)| {
            (id.clone(), NoteVoice { id: Some(id), offset: off, cents_offset: 0.0, prob: 1.0, release: false, velocity: None, t: 0.0 })
        })
        .collect();
    vec![Step { step: 0, value: None, voicing }]
}

/// Realize a phrase plan into the loop doc's note modulators: rewrite the chord voicing (stable
/// ids ⇒ the engine glides), the bass, and the arp's `pitch_walk` pool. Missing modulators are
/// skipped gracefully, as are any flagged `orrery_disable` (a constant "underdrone" stays put).
pub fn write_phrase(doc: &mut LoopDoc, plan: &PhrasePlan, cfg: &OrreryConfig) {
    // Every chord-role track gets the voicing (octave-staggered later by `apply_octaves`).
    for id in cfg.chord_mods() {
        if let Some(m) = doc.modulators.get_mut(&id) {
            if !m.orrery_disable {
                m.steps = single_step(plan.pad.voices.iter().map(|(id, o)| (id.clone(), *o as f32)));
            }
        }
    }
    if let Some(m) = doc.modulators.get_mut(&cfg.bass_mod) {
        if !m.orrery_disable {
            m.steps = single_step([(cfg.bass_id.clone(), plan.bass as f32)]);
        }
    }
    for id in cfg.arp_mods() {
        if let Some(m) = doc.modulators.get_mut(&id) {
            if !m.orrery_disable {
                if let Some(pw) = m.pitch_walk.as_mut() {
                    if !plan.melody.is_empty() {
                        pw.pool = plan.melody.iter().map(|&o| o as f32).collect();
                    }
                }
            }
        }
    }
}

/// A single one-voice note step at `step` firing `id` at `offset` semitones.
fn note_step(step: u32, id: &str, offset: f32) -> Step {
    let nv = NoteVoice { id: Some(id.to_string()), offset, cents_offset: 0.0, prob: 1.0, release: false, velocity: None, t: 0.0 };
    Step { step, value: None, voicing: [(id.to_string(), nv)].into_iter().collect() }
}

/// Octave-place a pitch-class offset near `center` semitones (the sparkle register for accents).
fn place_octave(off: i32, center: i32) -> i32 {
    let mut o = off.rem_euclid(12);
    while o < center - 6 {
        o += 12;
    }
    while o > center + 6 {
        o -= 12;
    }
    o
}

/// The voice id to fire a one-shot modulator with: reuse its first authored voice id (so any matrix
/// `voices`/routing still resolves), else a default.
fn oneshot_voice_id(m: &crate::loop_doc::Modulator) -> String {
    m.steps.iter().flat_map(|s| s.voicing.keys()).next().cloned().unwrap_or_else(|| "h".to_string())
}

/// Place seeded **accents** into each one-shot modulator's steps (Phase 4 derived surface; the role
/// differentiation of Phase 6b). Density (1..=3 hits) and step positions are seeded. The accent
/// **palette** is differentiated from the arp: when `accent_color` (a sparse/calm texture) it draws
/// from the chord's **upper-structure color tones** (9/#11/13), else from the chord tones; either way
/// register-placed near `accent_center` (a high sparkle above the arp). `orrery_disable` modulators
/// are skipped (a constant accent stays put).
pub fn write_oneshots(doc: &mut LoopDoc, plan: &PhrasePlan, cfg: &OrreryConfig, accent_center: i32, accent_color: bool, seed: u64) {
    let palette: &[i32] = if accent_color && !plan.color_tones.is_empty() {
        &plan.color_tones
    } else {
        &plan.chord_tones
    };
    if palette.is_empty() {
        return;
    }
    for (i, id) in cfg.oneshot_mods.iter().enumerate() {
        let Some(m) = doc.modulators.get_mut(id) else { continue };
        if m.orrery_disable || m.phrase.is_some() {
            // A phrase-armed one-shot gets its content from `select_and_refit_phrases`
            // (writing steps here would violate the steps ⊕ generator ⊕ phrase XOR).
            continue;
        }
        let len = m.length.max(1);
        let vid = oneshot_voice_id(m);
        let k = 1 + (hash_to_unit(hash_site(seed, "os_k", i as u64, id)) * 3.0) as u32; // 1..=3
        let mut steps: Vec<Step> = (0..k)
            .map(|h| {
                let sub = ((i as u64) << 16) | h as u64;
                let pos = (hash_to_unit(hash_site(seed, "os_pos", sub, id)) * len as f32) as u32 % len;
                let ti = ((hash_to_unit(hash_site(seed, "os_pitch", sub, id)) * palette.len() as f32) as usize)
                    .min(palette.len() - 1);
                note_step(pos, &vid, place_octave(palette[ti], accent_center) as f32)
            })
            .collect();
        steps.sort_by_key(|s| s.step);
        m.steps = steps;
    }
}

/// Keep what the Orrery wrote into each ranged modulator inside its `orrery_note_min..=max`
/// (ADR 0042). The whole surface (step voicings, `pitch_walk` pool, the `realized` phrase) first
/// moves by the octave shift that leaves the fewest notes outside, preferring no shift, then the
/// smallest. So an Arranger lift or a pad placement that won't fit is undone with its shape kept,
/// and one that fits stays. Any note still outside then folds by octaves to its nearest instance
/// inside. `orrery_disable` modulators are skipped.
pub fn enforce_ranges(doc: &mut LoopDoc, ids: &[String]) {
    for id in ids {
        let Some(m) = doc.modulators.get_mut(id) else { continue };
        if m.orrery_disable || (m.orrery_note_min.is_none() && m.orrery_note_max.is_none()) {
            continue;
        }
        let lo = m.orrery_note_min.unwrap_or(f32::NEG_INFINITY);
        let hi = m.orrery_note_max.unwrap_or(f32::INFINITY);
        let mut pitches: Vec<&mut f32> = Vec::new();
        for step in m.steps.iter_mut() {
            pitches.extend(step.voicing.values_mut().filter(|nv| !nv.release).map(|nv| &mut nv.offset));
        }
        if let Some(pw) = m.pitch_walk.as_mut() {
            pitches.extend(pw.pool.iter_mut());
        }
        if let Some(r) = m.phrase.as_mut().and_then(|ph| ph.realized.as_mut()) {
            pitches.extend(r.notes.iter_mut().map(|n| &mut n.offset));
        }
        if pitches.is_empty() {
            continue;
        }
        let outside = |shift: f32| pitches.iter().filter(|p| !(lo..=hi).contains(&(***p + shift))).count();
        let shift = [0.0, -12.0, 12.0, -24.0, 24.0, -36.0, 36.0].into_iter().min_by_key(|&s| outside(s)).unwrap_or(0.0);
        for p in pitches {
            *p = fold_into(*p + shift, lo, hi);
        }
    }
}

/// Fold `x` by octaves into `lo..=hi`; a range narrower than an octave that holds no instance of
/// `x`'s pitch class keeps the instance nearest to it.
fn fold_into(x: f32, lo: f32, hi: f32) -> f32 {
    // Jump by whole octaves (no loop over a far-off bound), then settle the last step exactly; the
    // `y ± 12 != y` guards stop at magnitudes where an octave no longer changes an `f32`.
    let mut y = x;
    if y > hi {
        y -= 12.0 * ((y - hi) / 12.0).ceil();
    }
    while y > hi && y - 12.0 != y {
        y -= 12.0;
    }
    if y < lo {
        y += 12.0 * ((lo - y) / 12.0).ceil();
    }
    while y < lo && y + 12.0 != y {
        y += 12.0;
    }
    if y > hi && (y - hi) > (lo - (y - 12.0)) {
        y -= 12.0;
    }
    y
}

/// Transpose the Orrery-written surfaces by whole octaves at write time (Phase 6b) — the Arranger's
/// collision-aware register motion. Voice-leading is preserved (a voicing moves as a unit) and the
/// engine glides the octave change. `orrery_disable` modulators are left put.
pub fn apply_octaves(doc: &mut LoopDoc, cfg: &OrreryConfig, pad_oct: i32, bass_oct: i32, arp_oct: i32) {
    let shift_voices = |m: &mut crate::loop_doc::Modulator, semis: f32| {
        for s in m.steps.iter_mut() {
            for nv in s.voicing.values_mut() {
                nv.offset += semis;
            }
        }
    };
    // Chord tracks: octave-staggered by index so layered pads/keys occupy distinct registers.
    for (i, id) in cfg.chord_mods().iter().enumerate() {
        let oct = pad_oct + i as i32;
        if oct == 0 {
            continue;
        }
        if let Some(m) = doc.modulators.get_mut(id) {
            if !m.orrery_disable {
                shift_voices(m, (12 * oct) as f32);
            }
        }
    }
    if bass_oct != 0 {
        if let Some(m) = doc.modulators.get_mut(&cfg.bass_mod) {
            if !m.orrery_disable {
                shift_voices(m, (12 * bass_oct) as f32);
            }
        }
    }
    // Arp tracks: octave-staggered by index (interlocking arps in different registers).
    for (i, id) in cfg.arp_mods().iter().enumerate() {
        let oct = arp_oct + i as i32;
        if oct == 0 {
            continue;
        }
        if let Some(m) = doc.modulators.get_mut(id) {
            if !m.orrery_disable {
                if let Some(pw) = m.pitch_walk.as_mut() {
                    for p in pw.pool.iter_mut() {
                        *p += (12 * oct) as f32;
                    }
                }
                // Phrase-armed arps register-move via their materialized copy (ADR 0028) —
                // the pristine pool entries are shared and never mutated in place.
                if let Some(r) = m.phrase.as_mut().and_then(|ph| ph.realized.as_mut()) {
                    for n in r.notes.iter_mut() {
                        n.offset += (12 * oct) as f32;
                    }
                }
            }
        }
    }
}

/// Set every arp track's euclid `generator.pulses` — the rhythmic **density** the Arranger rides
/// (Phase 6b) — clamped to each arp's `[1, length-1]` band. No-op for arps without a euclid generator
/// or flagged `orrery_disable`.
pub fn write_arp_density(doc: &mut LoopDoc, cfg: &OrreryConfig, pulses: u32) {
    for id in cfg.arp_mods() {
        let Some(m) = doc.modulators.get_mut(&id) else { continue };
        if m.orrery_disable {
            continue;
        }
        let len = m.length.max(1);
        if let Some(g) = m.generator.as_mut() {
            g.pulses = pulses.clamp(1, len.saturating_sub(1).max(1));
        }
    }
}

/// Phase 6c **narrative drop**: silence the given surfaces *musically* for one phrase — release the
/// held voices of a step-voiced track (chord/bass) so they tail out via the instrument's release
/// envelope, or zero a euclid arp's `pulses`. Never touches `instruments.enabled` (the seam) and skips
/// `orrery_disable`. The surface re-enters on the next phrase's normal write (the resolution).
pub fn drop_surface(doc: &mut LoopDoc, ids: &[String]) {
    for id in ids {
        let Some(m) = doc.modulators.get_mut(id) else { continue };
        if m.orrery_disable {
            continue;
        }
        if let Some(ph) = m.phrase.as_mut() {
            // Phrase arm (ADR 0028): materialize an empty tile — no new onsets this phrase
            // (gated tails release on their own; a dur-less held voice riding through the
            // drop is the documented v1 gap). Length keeps the tile clock steady.
            let len = ph.realized.as_ref().map(|r| r.length).unwrap_or(1).max(1);
            ph.realized = Some(Box::new(PhraseDef { fit: "literal".into(), tags: Vec::new(), length: len, notes: Vec::new() }));
        } else if let Some(g) = m.generator.as_mut() {
            g.pulses = 0; // euclid: no pulses fire ⇒ the arp goes silent (tails out)
        } else {
            // step-voiced: release every currently-held voice id (note-off, envelope tail).
            let vids: Vec<String> = m.steps.iter().flat_map(|s| s.voicing.keys().cloned()).collect();
            let voicing = vids
                .into_iter()
                .map(|vid| (vid.clone(), NoteVoice { id: Some(vid), offset: 0.0, cents_offset: 0.0, prob: 1.0, release: true, velocity: None, t: 0.0 }))
                .collect();
            m.steps = vec![Step { step: 0, value: None, voicing }];
        }
    }
}

/// Phase-window phrase selection for phrase-armed tracks (ADR 0028): for each bound arp /
/// one-shot modulator carrying a `phrase` arm, pick a pool entry — hard-filtered by the
/// ref's `tags`, weighted by the Arranger's energy (`busy` entries ∝ energy, `calm`/`sparse`
/// ∝ 1−energy), seeded draw — optionally develop it (rotate/fragment, a seeded minority of
/// phrase windows; never a `fit: "arp"` entry), refit it to the current chord per the entry's
/// `fit` inside the modulator's `orrery_note_min..=max` (ADR 0042), and materialize the
/// result as `phrase.active` + `phrase.realized`. The doc stays the deterministic ground
/// truth for plain renders; the pristine pool entries are never mutated. `orrery_disable`
/// is respected like every writer.
pub fn select_and_refit_phrases(
    doc: &mut LoopDoc,
    cfg: &OrreryConfig,
    plan: &PhrasePlan,
    scale_degrees: &[i32],
    energy: f32,
    seed: u64,
) {
    let ids: Vec<String> = cfg.arp_mods().into_iter().chain(cfg.oneshot_mods.iter().cloned()).collect();
    for (i, id) in ids.iter().enumerate() {
        let Some(m) = doc.modulators.get(id) else { continue };
        if m.orrery_disable {
            continue;
        }
        let Some(ph) = m.phrase.clone() else { continue };
        let cands: Vec<(String, PhraseDef)> = ph
            .pool
            .iter()
            .filter_map(|pid| crate::phrase::resolve(pid, doc).map(|d| (pid.clone(), d.clone())))
            .filter(|(_, d)| ph.tags.iter().all(|t| d.tags.contains(t)))
            .collect();
        if cands.is_empty() {
            continue;
        }
        let weight = |d: &PhraseDef| -> f32 {
            let mut w = 1.0f32;
            if d.tags.iter().any(|t| t == "busy") {
                w *= 0.25 + 1.5 * energy;
            }
            if d.tags.iter().any(|t| t == "calm" || t == "sparse") {
                w *= 0.25 + 1.5 * (1.0 - energy);
            }
            w.max(0.01)
        };
        let total: f32 = cands.iter().map(|(_, d)| weight(d)).sum();
        let mut u = hash_to_unit(hash_site(seed, "ph_sel", i as u64, id)) * total;
        let mut pick = cands.len() - 1;
        for (ci, (_, d)) in cands.iter().enumerate() {
            u -= weight(d);
            if u <= 0.0 {
                pick = ci;
                break;
            }
        }
        let range = NoteRange { min: m.orrery_note_min, max: m.orrery_note_max };
        let (pid, def) = &cands[pick];
        // An arpeggio keeps its line: rotating it scrambles the contour and fragmenting it
        // silences the back half of the tile.
        let dev = def.fit != "arp" && hash_to_unit(hash_site(seed, "ph_dev", i as u64, id)) < 0.4;
        let base = if dev {
            crate::theory::phrase_fit::develop_notes(def, hash_site(seed, "ph_dev_op", i as u64, id))
        } else {
            def.clone()
        };
        let realized = crate::theory::phrase_fit::refit(&base, &plan.chord_tones, scale_degrees, &def.fit, range);
        let (pid, realized) = (pid.clone(), realized);
        if let Some(m) = doc.modulators.get_mut(id) {
            if let Some(phm) = m.phrase.as_mut() {
                phm.active = Some(pid);
                phm.realized = Some(Box::new(realized));
            }
        }
    }
}

/// Vary the arp `pitch_walk` **pool length** per phrase (Phase 4): a seeded count of distinct pitches
/// within an internal band derived from the pool size, so the shimmer's pitch variety breathes phrase
/// to phrase. Pitch classes (scale membership) are preserved; the arp's *register* (octave) and
/// *rhythmic density* (pulses) are the Arranger's job (Phase 6b — `apply_octaves`/`write_arp_density`),
/// not a random flip here. `orrery_disable` is skipped.
pub fn vary_arp_pool(doc: &mut LoopDoc, cfg: &OrreryConfig, seed: u64) {
    for (i, id) in cfg.arp_mods().iter().enumerate() {
        let Some(m) = doc.modulators.get_mut(id) else { continue };
        if m.orrery_disable {
            continue;
        }
        let Some(pw) = m.pitch_walk.as_mut() else { continue };
        let full = pw.pool.len();
        if full == 0 {
            continue;
        }
        let lo = ((full + 1) / 2).max(1); // density floor ≈ half the pool
        let span = full - lo + 1;
        let len = (lo + (hash_to_unit(hash_site(seed, "arp_len", i as u64, id)) * span as f32) as usize).clamp(1, full);
        pw.pool.truncate(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::{Scale, Voicing};

    fn seed_doc() -> LoopDoc {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_orrery_dorian.json");
        LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn write_phrase_rewrites_chord_bass_and_arp() {
        let mut doc = seed_doc();
        let cfg = OrreryConfig::default();
        let s = Scale::from_harmony(&doc.harmony.root, &doc.harmony.mode);
        let mut v = Voyager::new(s, &cfg.pad_ids, &HarmonicPreference::default(), cfg.mel_len, 1);
        v.advance(&HarmonicPreference::default(), &Gravity::default(), &[], &AdvanceOpts::default(), 2);
        let plan = v.plan();
        write_phrase(&mut doc, &plan, &cfg);

        let chords = &doc.modulators[&cfg.chord_mod];
        assert_eq!(chords.steps.len(), 1, "one held chord step");
        assert_eq!(chords.steps[0].voicing.len(), cfg.pad_ids.len());
        // ids preserved (so the engine glides between phrases)
        for id in &cfg.pad_ids {
            assert!(chords.steps[0].voicing.contains_key(id), "voice {id} present");
        }
        // arp pool was rewritten to the generated melody
        let pool = &doc.modulators[&cfg.arp_mod].pitch_walk.as_ref().unwrap().pool;
        assert_eq!(pool.len(), cfg.mel_len);
    }

    #[test]
    fn orrery_disable_marks_a_constant_underdrone() {
        // Lock the chord modulator, round-trip through JSON (it's a loop-doc param), and confirm
        // Orrery leaves its voicing constant while still moving the other managed modulators.
        let mut doc = seed_doc();
        doc.modulators.get_mut("chords").unwrap().orrery_disable = true;
        let doc = LoopDoc::from_json_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert!(doc.modulators["chords"].orrery_disable, "flag survives JSON round-trip");

        let mut doc = doc;
        let before = serde_json::to_value(&doc.modulators["chords"].steps).unwrap();
        let cfg = OrreryConfig::default();
        let s = Scale::from_harmony(&doc.harmony.root, &doc.harmony.mode);
        let mut v = Voyager::new(s, &cfg.pad_ids, &HarmonicPreference::default(), cfg.mel_len, 1);
        v.advance(&HarmonicPreference::default(), &Gravity::default(), &[], &AdvanceOpts::default(), 2);
        write_phrase(&mut doc, &v.plan(), &cfg);

        let after = serde_json::to_value(&doc.modulators["chords"].steps).unwrap();
        assert_eq!(before, after, "locked chord modulator stays constant");
        // a non-locked managed modulator still moves (arp pool rewritten to the melody)
        assert_eq!(doc.modulators[&cfg.arp_mod].pitch_walk.as_ref().unwrap().pool.len(), cfg.mel_len);
    }

    // ── WS4.3: one-shot accents + variable pools ─────────────────────────────

    #[test]
    fn write_oneshots_places_chord_tone_accents() {
        let mut doc = seed_doc();
        let cfg = OrreryConfig { oneshot_mods: vec!["spark_events".into()], ..OrreryConfig::default() };
        // a known chord-tone palette (Cmaj7-shape offsets): pcs {0,4,7,11}
        let plan = PhrasePlan { pad: Voicing::default(), bass: -17, melody: vec![], chord_tones: vec![0, 4, 7, 11], color_tones: vec![] };
        write_oneshots(&mut doc, &plan, &cfg, 19, false, 12345); // accent_color=false ⇒ chord tones

        let m = &doc.modulators["spark_events"];
        assert!(!m.steps.is_empty(), "accents written");
        assert!(m.steps.len() <= 3, "density is 1..=3, got {}", m.steps.len());
        let chord_pcs = [0, 4, 7, 11];
        for s in &m.steps {
            assert!(s.step < m.length, "accent within the modulator length");
            for nv in s.voicing.values() {
                let pc = (nv.offset.round() as i32).rem_euclid(12);
                assert!(chord_pcs.contains(&pc), "accent pc {pc} is a chord tone");
            }
        }
        // deterministic for a fixed seed
        let mut b = seed_doc();
        write_oneshots(&mut b, &plan, &cfg, 19, false, 12345);
        assert_eq!(
            serde_json::to_value(&doc.modulators["spark_events"].steps).unwrap(),
            serde_json::to_value(&b.modulators["spark_events"].steps).unwrap(),
        );
    }

    #[test]
    fn write_oneshots_uses_color_tones_when_sparse() {
        // With `accent_color`, accents draw from the upper-structure color palette (here {2,9}) and
        // sit high near the accent center — a distinct sparkle from the chord-tone arp (Phase 6b).
        let mut doc = seed_doc();
        let cfg = OrreryConfig { oneshot_mods: vec!["spark_events".into()], ..OrreryConfig::default() };
        let plan = PhrasePlan { pad: Voicing::default(), bass: -17, melody: vec![], chord_tones: vec![0, 4, 7, 11], color_tones: vec![14, 21] };
        write_oneshots(&mut doc, &plan, &cfg, 24, true, 12345); // high accent center, color tones
        let m = &doc.modulators["spark_events"];
        assert!(!m.steps.is_empty(), "accents written");
        for s in &m.steps {
            for nv in s.voicing.values() {
                let pc = (nv.offset.round() as i32).rem_euclid(12);
                assert!([2, 9].contains(&pc), "accent pc {pc} is a color tone (9th/13th)");
                assert!(nv.offset >= 18.0, "accents sit high near the accent center, got {}", nv.offset);
            }
        }
    }

    #[test]
    fn write_oneshots_skips_orrery_disabled() {
        let mut doc = seed_doc();
        doc.modulators.get_mut("spark_events").unwrap().orrery_disable = true;
        let before = serde_json::to_value(&doc.modulators["spark_events"].steps).unwrap();
        let cfg = OrreryConfig { oneshot_mods: vec!["spark_events".into()], ..OrreryConfig::default() };
        let plan = PhrasePlan { pad: Voicing::default(), bass: 0, melody: vec![], chord_tones: vec![0, 4, 7], color_tones: vec![] };
        write_oneshots(&mut doc, &plan, &cfg, 19, false, 7);
        assert_eq!(serde_json::to_value(&doc.modulators["spark_events"].steps).unwrap(), before, "disabled one-shot is untouched");
    }

    #[test]
    fn apply_octaves_transposes_surfaces_by_whole_octaves() {
        let mut doc = seed_doc();
        let cfg = OrreryConfig::default(); // chords / bass_line / arp_shimmer
        let pad0: Vec<f32> = doc.modulators["chords"].steps[0].voicing.values().map(|nv| nv.offset).collect();
        let arp0 = doc.modulators["arp_shimmer"].pitch_walk.as_ref().unwrap().pool.clone();
        apply_octaves(&mut doc, &cfg, 1, 0, -1); // pad up an octave, arp down an octave
        let pad1: Vec<f32> = doc.modulators["chords"].steps[0].voicing.values().map(|nv| nv.offset).collect();
        let arp1 = doc.modulators["arp_shimmer"].pitch_walk.as_ref().unwrap().pool.clone();
        assert!(pad1.iter().zip(&pad0).all(|(a, b)| (a - b - 12.0).abs() < 1e-6), "pad +12");
        assert!(arp1.iter().zip(&arp0).all(|(a, b)| (a - b + 12.0).abs() < 1e-6), "arp −12");
    }

    #[test]
    fn write_arp_density_sets_clamped_pulses() {
        let mut doc = seed_doc();
        let cfg = OrreryConfig::default();
        let len = doc.modulators["arp_shimmer"].length;
        write_arp_density(&mut doc, &cfg, 3);
        assert_eq!(doc.modulators["arp_shimmer"].generator.as_ref().unwrap().pulses, 3);
        write_arp_density(&mut doc, &cfg, 9999); // clamps to length-1
        assert_eq!(doc.modulators["arp_shimmer"].generator.as_ref().unwrap().pulses, len - 1);
    }

    // ── Phase 6c: multi-track binding + the narrative drop ───────────────────

    #[test]
    fn writers_drive_all_chord_and_arp_tracks_octave_staggered() {
        // Two chord-role + two arp-role tracks: each gets the voicing/pool, and the 2nd of each is
        // octave-staggered (+12) by index so the layers occupy distinct registers (Phase 6c).
        let mut doc = seed_doc();
        doc.modulators.insert("chords2".into(), doc.modulators["chords"].clone());
        doc.modulators.insert("arp2".into(), doc.modulators["arp_shimmer"].clone());
        let cfg = OrreryConfig {
            extra_chord_mods: vec!["chords2".into()],
            extra_arp_mods: vec!["arp2".into()],
            ..OrreryConfig::default()
        };
        let s = Scale::from_harmony(&doc.harmony.root, &doc.harmony.mode);
        let mut v = Voyager::new(s, &cfg.pad_ids, &HarmonicPreference::default(), cfg.mel_len, 1);
        v.advance(&HarmonicPreference::default(), &Gravity::default(), &[], &AdvanceOpts::default(), 2);
        write_phrase(&mut doc, &v.plan(), &cfg);
        apply_octaves(&mut doc, &cfg, 0, 0, 0); // index stagger only

        assert_eq!(doc.modulators["chords2"].steps[0].voicing.len(), cfg.pad_ids.len(), "2nd chord voiced");
        let pick = |id: &str| -> Vec<f32> {
            let mut v: Vec<f32> = doc.modulators[id].steps[0].voicing.values().map(|nv| nv.offset).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v
        };
        let (c1, c2) = (pick("chords"), pick("chords2"));
        assert!(c2.iter().zip(&c1).all(|(a, b)| (a - b - 12.0).abs() < 1e-6), "2nd chord an octave up: {c1:?} {c2:?}");
        let arp1 = doc.modulators["arp_shimmer"].pitch_walk.as_ref().unwrap().pool.clone();
        let arp2 = doc.modulators["arp2"].pitch_walk.as_ref().unwrap().pool.clone();
        assert_eq!(arp2.len(), cfg.mel_len, "2nd arp got the pool");
        assert!(arp2.iter().zip(&arp1).all(|(a, b)| (a - b - 12.0).abs() < 1e-6), "2nd arp an octave up");
    }

    #[test]
    fn drop_surface_releases_chords_and_silences_arps() {
        let mut doc = seed_doc();
        let cfg = OrreryConfig::default(); // chords (step-voiced) + arp_shimmer (euclid)
        drop_surface(&mut doc, &cfg.track_modulators("chord"));
        drop_surface(&mut doc, &cfg.track_modulators("arp"));
        let cs = &doc.modulators["chords"].steps;
        assert_eq!(cs.len(), 1, "chord becomes one release step");
        assert!(cs[0].voicing.values().all(|nv| nv.release), "chord voices are released (tail out)");
        assert!(!cs[0].voicing.is_empty(), "the release names the held voices");
        assert_eq!(doc.modulators["arp_shimmer"].generator.as_ref().unwrap().pulses, 0, "arp silenced");
    }

    // ── ADR 0028: phrase-armed tracks ────────────────────────────────────────

    fn phrase_seed_doc() -> LoopDoc {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../lab/tests/fixtures/loop_orrery_phrase.json");
        LoopDoc::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }
    const DORIAN: [i32; 7] = [0, 2, 3, 5, 7, 9, 10];
    const DM7: [i32; 4] = [0, 3, 7, 10];

    fn plan_dm7() -> PhrasePlan {
        PhrasePlan { pad: crate::theory::Voicing::default(), bass: -17, melody: vec![], chord_tones: DM7.to_vec(), color_tones: vec![14, 21] }
    }

    #[test]
    fn select_and_refit_materializes_active_and_realized_with_energy_gating() {
        // High energy prefers `busy` entries (lib:glass_up); low energy prefers `calm`
        // (lib:pedal_pulse / turn). Every selection also writes a chord-refit `realized`
        // whose strong (whole-step t) notes land on chord pitch classes.
        let cfg = OrreryConfig::default(); // arp_mod = "arp_shimmer" — matches the fixture
        let busy_picks = |energy: f32| -> usize {
            (0..40u64)
                .filter(|&s| {
                    let mut doc = phrase_seed_doc();
                    select_and_refit_phrases(&mut doc, &cfg, &plan_dm7(), &DORIAN, energy, s);
                    let ph = doc.modulators["arp_shimmer"].phrase.as_ref().unwrap();
                    let r = ph.realized.as_ref().expect("realized materialized");
                    if !r.notes.is_empty() {
                        for n in r.notes.iter().filter(|n| n.t.fract() == 0.0) {
                            let pc = (n.offset as i32).rem_euclid(12);
                            assert!(DM7.contains(&pc), "strong note {} refit to a chord tone", n.offset);
                        }
                    }
                    ph.active.as_deref() == Some("lib:glass_up")
                })
                .count()
        };
        let (hi, lo) = (busy_picks(0.95), busy_picks(0.05));
        assert!(hi > lo, "energy gates busy-vs-calm selection (busy picks: {hi} @0.95 vs {lo} @0.05)");
        // deterministic per seed
        let mut a = phrase_seed_doc();
        let mut b = phrase_seed_doc();
        select_and_refit_phrases(&mut a, &cfg, &plan_dm7(), &DORIAN, 0.5, 7);
        select_and_refit_phrases(&mut b, &cfg, &plan_dm7(), &DORIAN, 0.5, 7);
        assert_eq!(
            serde_json::to_value(&a.modulators["arp_shimmer"].phrase).unwrap(),
            serde_json::to_value(&b.modulators["arp_shimmer"].phrase).unwrap(),
        );
    }

    #[test]
    fn enforce_ranges_skips_a_lift_that_wont_fit_and_folds_strays() {
        let cfg = OrreryConfig::default(); // arp_mod = "arp_shimmer"
        let realized = |doc: &LoopDoc| -> Vec<f32> {
            doc.modulators["arp_shimmer"].phrase.as_ref().unwrap().realized.as_ref().unwrap().notes.iter().map(|n| n.offset).collect()
        };
        let mut doc = phrase_seed_doc();
        select_and_refit_phrases(&mut doc, &cfg, &plan_dm7(), &DORIAN, 0.5, 7);
        let fitted = realized(&doc);
        let (lo, hi) = fitted.iter().fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        {
            let m = doc.modulators.get_mut("arp_shimmer").unwrap();
            m.orrery_note_min = Some(lo);
            m.orrery_note_max = Some(hi);
        }
        // An Arranger lift that won't fit is undone whole (the shape is kept).
        apply_octaves(&mut doc, &cfg, 0, 0, 1);
        enforce_ranges(&mut doc, &cfg.written_modulators());
        assert_eq!(realized(&doc), fitted, "the lift is skipped");
        // One that fits stays.
        doc.modulators.get_mut("arp_shimmer").unwrap().orrery_note_max = Some(hi + 12.0);
        apply_octaves(&mut doc, &cfg, 0, 0, 1);
        enforce_ranges(&mut doc, &cfg.written_modulators());
        assert!(realized(&doc).iter().zip(&fitted).all(|(a, b)| a - b == 12.0), "a lift that fits stays");
        // A stray note folds by octaves; the rest stay put.
        let mut doc2 = phrase_seed_doc();
        select_and_refit_phrases(&mut doc2, &cfg, &plan_dm7(), &DORIAN, 0.5, 7);
        {
            let m = doc2.modulators.get_mut("arp_shimmer").unwrap();
            m.orrery_note_min = Some(lo);
            m.orrery_note_max = Some(hi);
            m.phrase.as_mut().unwrap().realized.as_mut().unwrap().notes[0].offset = hi + 12.0;
        }
        enforce_ranges(&mut doc2, &cfg.written_modulators());
        let r = realized(&doc2);
        assert_eq!(r[0], hi, "the stray folds an octave down into range");
        assert_eq!(r[1..], fitted[1..]);
        // No range ⇒ untouched.
        let mut doc3 = phrase_seed_doc();
        select_and_refit_phrases(&mut doc3, &cfg, &plan_dm7(), &DORIAN, 0.5, 7);
        apply_octaves(&mut doc3, &cfg, 0, 0, 1);
        let lifted = realized(&doc3);
        enforce_ranges(&mut doc3, &cfg.written_modulators());
        assert_eq!(realized(&doc3), lifted);
    }

    #[test]
    fn an_arp_entry_is_never_developed_and_climbs_to_the_modulator_range() {
        // A D3 → D5 → F3 arpeggio (fit "arp") under C major (C E G over D): the written range holds
        // one chord tone too few for the rise; the modulator's range (max 16) lets it reach E5.
        let line = [-12.0, -9.0, -5.0, 0.0, 3.0, 7.0, 12.0, 7.0, 3.0, 0.0, -5.0, -9.0];
        let c_major = PhrasePlan { chord_tones: vec![10, 14, 17], ..plan_dm7() };
        let run = |min: Option<f32>, max: Option<f32>, seed: u64| -> PhraseDef {
            let mut doc = phrase_seed_doc();
            let mut entry = doc.sequence_pools["turn"].clone();
            entry.fit = "arp".into();
            entry.length = 12;
            entry.notes = line.iter().enumerate().map(|(i, &o)| crate::loop_doc::PhraseNote { t: i as f32, offset: o, dur: Some(1.0), ..entry.notes[0].clone() }).collect();
            doc.sequence_pools.insert("updown".into(), entry);
            let m = doc.modulators.get_mut("arp_shimmer").unwrap();
            m.phrase.as_mut().unwrap().pool = vec!["updown".into()];
            m.length = 12;
            (m.orrery_note_min, m.orrery_note_max) = (min, max);
            select_and_refit_phrases(&mut doc, &OrreryConfig::default(), &c_major, &DORIAN, 0.5, seed);
            *doc.modulators["arp_shimmer"].phrase.as_ref().unwrap().realized.clone().unwrap()
        };
        let top = |d: &PhraseDef| d.notes.iter().map(|n| n.offset).fold(f32::MIN, f32::max);
        let first = run(Some(-12.0), Some(16.0), 0);
        assert_eq!(top(&first), 14.0, "climbs past the written D5 to E5");
        for seed in 0..24u64 {
            // Every phrase window plays the whole line in order — no rotate, no fragment.
            let r = run(Some(-12.0), Some(16.0), seed);
            assert_eq!(r, first, "seed {seed}: an arp entry is never developed");
            assert_eq!(r.notes.len(), line.len());
            // Without the range, the written top still caps it.
            assert!(top(&run(None, None, seed)) <= 12.0);
        }
    }

    #[test]
    fn fold_into_handles_open_and_narrow_ranges() {
        assert_eq!(fold_into(26.0, -12.0, 12.0), 2.0);
        assert_eq!(fold_into(-20.0, -12.0, 12.0), -8.0);
        assert_eq!(fold_into(5.0, f32::NEG_INFINITY, f32::INFINITY), 5.0);
        // A 4-semitone window holding no instance of the pitch class: the nearest one (12, 1 above it).
        assert_eq!(fold_into(0.0, 7.0, 11.0), 12.0);
        // Exact on the bounds, and a far-off bound is reached in one jump (no octave-by-octave loop).
        assert_eq!(fold_into(24.0, -12.0, 12.0), 12.0);
        assert_eq!(fold_into(-24.0, -12.0, 12.0), -12.0);
        assert_eq!(fold_into(0.5, 100.0, 112.0), 108.5);
        assert!(fold_into(0.0, 1e9, f32::INFINITY) >= 1e9 - 64.0, "terminates where an octave is below f32 precision");
    }

    #[test]
    fn select_and_refit_respects_orrery_disable() {
        let cfg = OrreryConfig::default();
        let mut doc = phrase_seed_doc();
        doc.modulators.get_mut("arp_shimmer").unwrap().orrery_disable = true;
        select_and_refit_phrases(&mut doc, &cfg, &plan_dm7(), &DORIAN, 0.5, 7);
        let ph = doc.modulators["arp_shimmer"].phrase.as_ref().unwrap();
        assert!(ph.active.is_none() && ph.realized.is_none(), "disabled track keeps its authored phrase arm");
    }

    #[test]
    fn apply_octaves_shifts_the_realized_phrase_copy() {
        let cfg = OrreryConfig::default();
        let mut doc = phrase_seed_doc();
        select_and_refit_phrases(&mut doc, &cfg, &plan_dm7(), &DORIAN, 0.5, 7);
        let before: Vec<f32> =
            doc.modulators["arp_shimmer"].phrase.as_ref().unwrap().realized.as_ref().unwrap().notes.iter().map(|n| n.offset).collect();
        apply_octaves(&mut doc, &cfg, 0, 0, 1); // arp up an octave
        let after: Vec<f32> =
            doc.modulators["arp_shimmer"].phrase.as_ref().unwrap().realized.as_ref().unwrap().notes.iter().map(|n| n.offset).collect();
        assert!(!before.is_empty());
        assert!(after.iter().zip(&before).all(|(a, b)| (a - b - 12.0).abs() < 1e-6), "realized moved +12: {before:?} → {after:?}");
    }

    #[test]
    fn write_oneshots_skips_phrase_armed_mods() {
        // Writing accent STEPS into a phrase-armed mod would violate the note-source XOR.
        let mut doc = phrase_seed_doc();
        let cfg = OrreryConfig { oneshot_mods: vec!["arp_shimmer".into()], ..OrreryConfig::default() };
        write_oneshots(&mut doc, &plan_dm7(), &cfg, 19, false, 12345);
        assert!(doc.modulators["arp_shimmer"].steps.is_empty(), "phrase-armed mod gets no steps");
        assert!(doc.modulators["arp_shimmer"].phrase.is_some());
    }

    #[test]
    fn drop_surface_materializes_a_silent_tile_for_phrase_mods() {
        let cfg = OrreryConfig::default();
        let mut doc = phrase_seed_doc();
        select_and_refit_phrases(&mut doc, &cfg, &plan_dm7(), &DORIAN, 0.5, 7);
        drop_surface(&mut doc, &cfg.track_modulators("arp"));
        let ph = doc.modulators["arp_shimmer"].phrase.as_ref().unwrap();
        let r = ph.realized.as_ref().unwrap();
        assert!(r.notes.is_empty(), "the drop tile has no onsets");
        assert!(r.length >= 1, "tile clock stays sane");
        // …and the engine renders it as silence for this track
        let out = crate::modulator::evaluate_modulator(
            &doc.modulators["arp_shimmer"],
            &crate::engine::ModulatorPhase::default(),
            48_000,
            48_000.0,
            &doc,
        );
        assert!(out.note_events.is_empty(), "no onsets while dropped");
        // the doc still validates (empty realized is legal machine-written silence)
        let json = serde_json::to_string(&doc).unwrap();
        assert!(LoopDoc::from_json_str(&json).is_ok());
    }

    #[test]
    fn vary_arp_pool_changes_length_within_band() {
        let cfg = OrreryConfig::default(); // arp_mod = "arp_shimmer"
        let lens: Vec<usize> = (0..8u64)
            .map(|ph| {
                let mut doc = seed_doc();
                doc.modulators.get_mut("arp_shimmer").unwrap().pitch_walk.as_mut().unwrap().pool =
                    vec![12.0, 14.0, 15.0, 17.0, 19.0, 21.0]; // a known pool of 6
                vary_arp_pool(&mut doc, &cfg, crate::hash::hash64(7, ph, 0, 0));
                doc.modulators["arp_shimmer"].pitch_walk.as_ref().unwrap().pool.len()
            })
            .collect();
        // full=6 ⇒ density floor 3 ⇒ lengths land in [3,6] and actually vary across phrases.
        assert!(lens.iter().all(|&l| (3..=6).contains(&l)), "lengths within band: {lens:?}");
        assert!(lens.iter().collect::<std::collections::HashSet<_>>().len() > 1, "length varies: {lens:?}");
    }

}
