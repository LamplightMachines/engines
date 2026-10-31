//! Voice model helpers (CE-R4.3) and note/voice transition semantics (CE-R6).
//!
//! [`apply_note_event`] resolves one note event against a single source's current
//! voices: glide / sustain / articulate / release by `id`; unreferenced ids sustain;
//! `release` stops. Anonymous voices (no `id`, e.g. generator notes) get a deterministic
//! synthetic id so their RNG/filter/pan don't collide (an M2 quality bug). Cross-source
//! polyphony + priority stealing live in [`crate::matrix`].

use crate::engine::ActiveVoice;
use crate::loop_doc::{Harmony, NoteVoice};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Sustain,
    Glide,
    Articulate,
    Release,
}

#[derive(Clone, Debug)]
pub struct VoiceAction {
    pub voice: ActiveVoice,
    pub action: Action,
    pub glide_from: f32,
}

#[derive(Clone, Debug, Default)]
pub struct VoiceResult {
    /// This source's voices after the event (oldest-first).
    pub voices: Vec<ActiveVoice>,
    pub actions: Vec<VoiceAction>,
}

const PITCH_EPS: f32 = 0.001;

/// Apply one event's `voicing` to `current` (this source's voices). `frame` seeds
/// deterministic ids for anonymous voices.
pub fn apply_note_event(
    incoming: &[NoteVoice],
    current: &[ActiveVoice],
    source_id: &str,
    budget: usize,
    frame: usize,
) -> VoiceResult {
    let mut result = VoiceResult::default();
    let mut incoming_ids: Vec<String> = Vec::new();

    for (i, nv) in incoming.iter().enumerate() {
        if nv.release {
            if let Some(id) = &nv.id {
                incoming_ids.push(id.clone());
                if let Some(prev) = current.iter().find(|v| &v.voice_id == id) {
                    result.actions.push(VoiceAction {
                        voice: prev.clone(),
                        action: Action::Release,
                        glide_from: prev.pitch_semitones,
                    });
                    // carry the voice into its release tail (CE-R4.3); the engine stamps
                    // released_at_s and prunes it once the tail completes.
                    result.voices.push(prev.clone());
                }
            }
            continue;
        }

        if let Some(id) = &nv.id {
            incoming_ids.push(id.clone());
            // match only a HELD voice — a voice already in its release tail is re-articulated
            // fresh (envelope reset) rather than sustained/glided.
            if let Some(prev) = current.iter().find(|v| &v.voice_id == id && v.released_at_s.is_none()) {
                let same = (prev.pitch_semitones - nv.offset).abs() < PITCH_EPS
                    && (prev.cents_offset - nv.cents_offset).abs() < PITCH_EPS;
                let mut updated = prev.clone();
                updated.pitch_semitones = nv.offset;
                updated.cents_offset = nv.cents_offset;
                result.actions.push(VoiceAction {
                    voice: updated.clone(),
                    action: if same { Action::Sustain } else { Action::Glide },
                    glide_from: prev.pitch_semitones,
                });
                result.voices.push(updated);
                continue;
            }
        }

        // New id, or anonymous → fresh articulation.
        let voice_id = nv
            .id
            .clone()
            .unwrap_or_else(|| synthetic_voice_id(source_id, frame, i));
        let av = ActiveVoice {
            voice_id,
            source_id: source_id.to_string(),
            pitch_semitones: nv.offset,
            cents_offset: nv.cents_offset,
            phase_rad: 0.0,
            phase_rad_shift: 0.0,
            elapsed_decay_s: 0.0,
            released_at_s: None,
            synth_state: Default::default(),
        };
        result.actions.push(VoiceAction {
            voice: av.clone(),
            action: Action::Articulate,
            glide_from: av.pitch_semitones,
        });
        result.voices.push(av);
    }

    // Unreferenced existing id'd voices → sustain (carry as-is). (CE-R6.2)
    for v in current {
        if !v.voice_id.is_empty() && !incoming_ids.iter().any(|id| id == &v.voice_id) {
            result.voices.push(v.clone());
        }
    }

    // Cap this source to its voices budget (keep oldest/sustained).
    if result.voices.len() > budget {
        result.voices.truncate(budget);
    }
    result
}

/// Deterministic id for an anonymous voice (no authored `id`, e.g. generator/arp notes),
/// seeded by its event site. Shared by [`apply_note_event`] and the engine's MIDI
/// velocity-hint lookup (ADR 0026) so the two can't drift.
pub fn synthetic_voice_id(source_id: &str, frame: usize, i: usize) -> String {
    format!("{source_id}@{frame}#{i}")
}

// ── Pitch (CE-R3.3 harmony: offsets are semitones from root) ────────────────

/// Semitone of a note name relative to C (C=0 … B=11).
pub fn root_semitone(root: &str) -> f32 {
    try_root_semitone(root).unwrap_or(0.0)
}

/// [`root_semitone`] without the silent fallback to C — `None` for a spelling the engine doesn't
/// accept (the doc checker reports it, ADR 0033).
pub fn try_root_semitone(root: &str) -> Option<f32> {
    Some(match root {
        "C" | "c" => 0.0,
        "C#" | "Db" => 1.0,
        "D" | "d" => 2.0,
        "D#" | "Eb" => 3.0,
        "E" | "e" => 4.0,
        "F" | "f" => 5.0,
        "F#" | "Gb" => 6.0,
        "G" | "g" => 7.0,
        "G#" | "Ab" => 8.0,
        "A" | "a" => 9.0,
        "A#" | "Bb" => 10.0,
        "B" | "b" => 11.0,
        // The schema's spelling pattern admits these too; each is its pitch class (as `B` / `E` /
        // `F` / `C`), the octave the root's other spellings play in.
        "Cb" => 11.0,
        "Fb" => 4.0,
        "E#" => 5.0,
        "B#" => 0.0,
        _ => return None,
    })
}

/// Frequency (Hz) for a semitone `offset` from `harmony.root` (octave 4) + `cents`.
pub fn pitch_to_hz(harmony: &Harmony, offset_st: f32, cents: f32) -> f32 {
    let midi = 60.0 + root_semitone(&harmony.root) + offset_st;
    let st_from_a4 = (midi - 69.0) + cents / 100.0;
    440.0 * 2.0_f32.powf(st_from_a4 / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn av(id: &str, pitch: f32) -> ActiveVoice {
        ActiveVoice { voice_id: id.into(), source_id: "chords".into(), pitch_semitones: pitch, ..Default::default() }
    }
    fn nv(id: Option<&str>, offset: f32) -> NoteVoice {
        NoteVoice { id: id.map(|s| s.into()), offset, cents_offset: 0.0, prob: 1.0, release: false, velocity: None, t: 0.0 }
    }

    #[test]
    fn sustain_then_glide() {
        let cur = vec![av("v1", 7.0)];
        let r = apply_note_event(&[nv(Some("v1"), 7.0)], &cur, "chords", 4, 0);
        assert_eq!(r.actions[0].action, Action::Sustain);
        let r = apply_note_event(&[nv(Some("v1"), 9.0)], &cur, "chords", 4, 0);
        assert_eq!(r.actions[0].action, Action::Glide);
        assert_eq!(r.actions[0].glide_from, 7.0);
        assert_eq!(r.voices[0].pitch_semitones, 9.0);
    }

    #[test]
    fn absent_sustains_release_stops() {
        let cur = vec![av("v1", 0.0), av("v4", 14.0)];
        // only v1 mentioned → v4 sustains
        let r = apply_note_event(&[nv(Some("v1"), 0.0)], &cur, "chords", 4, 0);
        assert!(r.voices.iter().any(|v| v.voice_id == "v4"));
        // explicit release of v4: emits a Release action and CARRIES v4 into its tail
        // (the engine stamps released_at_s and prunes it when the tail finishes).
        let rel = NoteVoice { id: Some("v4".into()), release: true, prob: 1.0, ..nv(None, 0.0) };
        let r = apply_note_event(&[rel], &cur, "chords", 4, 0);
        assert!(r.actions.iter().any(|a| a.action == Action::Release && a.voice.voice_id == "v4"));
        assert!(r.voices.iter().any(|v| v.voice_id == "v4"), "released voice is carried for the tail");
    }

    #[test]
    fn anonymous_voices_get_distinct_ids() {
        let r = apply_note_event(&[nv(None, 5.0), nv(None, 7.0)], &[], "arp", 4, 100);
        assert_eq!(r.voices.len(), 2);
        assert_ne!(r.voices[0].voice_id, r.voices[1].voice_id);
        assert!(r.voices[0].voice_id.starts_with("arp@100#"));
    }

    #[test]
    fn pitch_d_root_offset_zero_is_d4() {
        let h = Harmony { root: "D".into(), mode: "dorian".into() };
        // D4 = MIDI 62 → 293.66 Hz
        assert!((pitch_to_hz(&h, 0.0, 0.0) - 293.66).abs() < 0.5);
    }
}
