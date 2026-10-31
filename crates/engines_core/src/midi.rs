//! MIDI-out event types + the voice-set → Note-On/Off diff (ADR 0026).
//!
//! A `type: "midi"` instrument translates its note events and effective (post-modulation)
//! param values into MIDI instead of synthesizing audio. Everything here is **pure data /
//! pure functions** — events are produced deterministically inside `Engine::render`
//! (CE-R7.3: a function of `(doc, snapshot, loop_id)` only) and *delivered* by a platform
//! shell; no I/O lives in the core.
//!
//! Hung-note safety is the central design concern: the engine's note semantics are
//! "absent ⇒ sustain, never a dropout" (design-rationale §2.5), so Note-Offs cannot be
//! derived from note *events* alone (a stolen/pruned voice produces no `Action`). Instead
//! [`diff_notes`] reconciles the instrument's **desired note set** against the
//! last-emitted set carried in the phase snapshot ([`MidiOutState`]): a voice sustained at
//! the same pitch across loops emits nothing; a vanished voice emits Note-Off; a pitch
//! change emits On(new)-before-Off(old) (legato overlap, so an external synth's portamento
//! can glide). Unison-safe: two voices on one note number are ref-counted, so releasing a
//! cents-detuned double never kills its sibling.
//!
//! The **mirror** (ADR 0048) runs the same reconcile for an *audio* instrument the shell picks
//! ([`diff_tap`]): its events carry the instrument instead of a channel, and a voice articulated
//! on a note already sounding re-strikes it, so the MIDI strikes where the audio does.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One wire-level MIDI message. `ch` is the **wire** channel `0..=15` (the loop doc authors
/// human channels `1..=16`); `note`/`cc`/`value`/`vel` are 7-bit `0..=127`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MidiMsg {
    NoteOn { ch: u8, note: u8, vel: u8 },
    NoteOff { ch: u8, note: u8 },
    Cc { ch: u8, cc: u8, value: u8 },
}

/// A MIDI message stamped with its sample offset within the rendered loop. The session
/// layer re-bases `frame` onto the absolute session sample clock for shell scheduling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiEvent {
    pub frame: usize,
    pub msg: MidiMsg,
}

/// A [`MidiEvent`] re-based onto the session's **absolute sample clock** (audio frames since
/// session start — the same origin a shell's frames-played counter uses). This is what
/// [`crate::session::Session::poll_midi`] / `engines_session_poll_midi_json` stream: the shell
/// holds each message until playback reaches `abs_frame`, then sends it to the OS port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedMidi {
    pub abs_frame: u64,
    pub msg: MidiMsg,
}

/// Per-instrument emitted-MIDI state, carried across loops in the phase snapshot
/// (CE-R7.3: part of the deterministic state, like modulator phases). `notes` is the
/// last-emitted note set (`voice_id → (note#, velocity)`); `cc_last` the last-sent 7-bit
/// value per mapped param (so CC is emit-on-change; an empty snapshot naturally yields a
/// full initial sync burst).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MidiOutState {
    /// Wire channel (`0..=15`) the state was emitted on. A live channel edit flushes
    /// Note-Offs on the old channel and re-emits on the new one.
    #[serde(default)]
    pub channel: u8,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, (u8, u8)>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cc_last: BTreeMap<String, u8>,
}

/// Reconcile the desired note set against the previously-emitted one, appending the
/// Note-On/Off events (at `frame`, on wire channel `ch`) to `out`, then adopt `desired`
/// as the new emitted set.
///
/// Note numbers are **ref-counted** across voices: an On fires only on a note's 0→1
/// transition, an Off only on 1→0 — so unison voicings (two `voice_id`s on one note#,
/// e.g. a cents-detuned double, which plain MIDI can't distinguish) never half-die. All
/// Ons are emitted before all Offs at the same frame so a pitch change overlaps legato
/// (external portamento glides instead of retriggering). A note held by any voice across
/// the diff emits nothing — the external synth just keeps sustaining (design-rationale
/// §2.5 "absent = sustain", per ADR 0026). Velocity for a newly-on note comes from the
/// first (sorted-`voice_id`) desired voice holding it — deterministic by BTreeMap order.
pub fn diff_notes(
    prev: &mut BTreeMap<String, (u8, u8)>,
    desired: &BTreeMap<String, (u8, u8)>,
    ch: u8,
    frame: usize,
    out: &mut Vec<MidiEvent>,
) {
    if *prev == *desired {
        return; // fast path: the steady drone (sustained voices) emits nothing
    }
    let mut old_count = [0u16; 128];
    for &(note, _) in prev.values() {
        old_count[note as usize] += 1;
    }
    let mut new_count = [0u16; 128];
    let mut on_vel = [0u8; 128];
    for &(note, vel) in desired.values() {
        if new_count[note as usize] == 0 {
            on_vel[note as usize] = vel; // first sorted voice_id wins (deterministic)
        }
        new_count[note as usize] += 1;
    }
    for note in 0..128usize {
        if new_count[note] > 0 && old_count[note] == 0 {
            out.push(MidiEvent { frame, msg: MidiMsg::NoteOn { ch, note: note as u8, vel: on_vel[note] } });
        }
    }
    for note in 0..128usize {
        if old_count[note] > 0 && new_count[note] == 0 {
            out.push(MidiEvent { frame, msg: MidiMsg::NoteOff { ch, note: note as u8 } });
        }
    }
    *prev = desired.clone();
}

/// Flush Note-Offs for every note still held in `state` (session stop, instrument
/// disabled/removed, or a channel edit), clearing the held set. Ref-count aware: one Off
/// per distinct note number.
pub fn flush_notes(state: &mut MidiOutState, frame: usize, out: &mut Vec<MidiEvent>) {
    let mut seen = [false; 128];
    for &(note, _) in state.notes.values() {
        if !seen[note as usize] {
            seen[note as usize] = true;
            out.push(MidiEvent { frame, msg: MidiMsg::NoteOff { ch: state.channel, note } });
        }
    }
    state.notes.clear();
}

// ── The mirror (ADR 0048): an audio instrument's notes as MIDI ─────────────

/// A mirrored note (ADR 0048). No channel: the shell maps the instrument to a port and channel
/// (a Live set's input pair), and an offline log keys by instrument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TapMsg {
    NoteOn { note: u8, vel: u8 },
    NoteOff { note: u8 },
}

/// A [`TapMsg`] from instrument `inst`, at `frame` within the rendered loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapEvent {
    pub frame: usize,
    pub inst: String,
    pub msg: TapMsg,
}

/// A [`TapEvent`] re-based onto the session's absolute sample clock (like [`TimedMidi`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedTap {
    pub abs_frame: u64,
    pub inst: String,
    pub msg: TapMsg,
}

/// One mirrored instrument's emitted notes, carried across loops in the phase snapshot:
/// `"<source>::<voice_id>" → (note#, velocity)`. Keyed by source too, since two note sources into
/// one instrument may author the same voice ids.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TapState {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, (u8, u8)>,
}

/// The mirror's key for a voice: its note source and voice id.
pub fn tap_key(source: &str, voice_id: &str) -> String {
    format!("{source}::{voice_id}")
}

/// [`diff_notes`] for the mirror: the same ref-counted reconcile, plus **re-strikes** — a key in
/// `fresh` (a voice articulated by this event) that lands on a note already sounding sends Off
/// then On, so the MIDI strikes where the audio does (the translator keeps holding instead).
/// Within the frame: re-strike Offs, then Ons, then plain Offs (a pitch change still overlaps).
pub fn diff_tap(
    prev: &mut BTreeMap<String, (u8, u8)>,
    desired: &BTreeMap<String, (u8, u8)>,
    fresh: &BTreeSet<String>,
    inst: &str,
    frame: usize,
    out: &mut Vec<TapEvent>,
) {
    if *prev == *desired && fresh.is_empty() {
        return;
    }
    let mut old_count = [0u16; 128];
    for &(note, _) in prev.values() {
        old_count[note as usize] += 1;
    }
    let mut new_count = [0u16; 128];
    let mut on_vel = [0u8; 128];
    for &(note, vel) in desired.values() {
        if new_count[note as usize] == 0 {
            on_vel[note as usize] = vel;
        }
        new_count[note as usize] += 1;
    }
    let mut restrike = [false; 128];
    for key in fresh {
        if let Some(&(note, vel)) = desired.get(key) {
            if old_count[note as usize] > 0 && !prev.contains_key(key) && !restrike[note as usize] {
                restrike[note as usize] = true;
                on_vel[note as usize] = vel;
            }
        }
    }
    let ev = |msg| TapEvent { frame, inst: inst.to_string(), msg };
    for note in 0..128usize {
        if restrike[note] {
            out.push(ev(TapMsg::NoteOff { note: note as u8 }));
        }
    }
    for note in 0..128usize {
        if new_count[note] > 0 && (old_count[note] == 0 || restrike[note]) {
            out.push(ev(TapMsg::NoteOn { note: note as u8, vel: on_vel[note] }));
        }
    }
    for note in 0..128usize {
        if old_count[note] > 0 && new_count[note] == 0 {
            out.push(ev(TapMsg::NoteOff { note: note as u8 }));
        }
    }
    *prev = desired.clone();
}

/// Note-Offs for every note `state` still holds (one per distinct note), clearing it.
pub fn flush_tap(state: &mut TapState, inst: &str, frame: usize, out: &mut Vec<TapEvent>) {
    let mut seen = [false; 128];
    for &(note, _) in state.notes.values() {
        if !seen[note as usize] {
            seen[note as usize] = true;
            out.push(TapEvent { frame, inst: inst.to_string(), msg: TapMsg::NoteOff { note } });
        }
    }
    state.notes.clear();
}

/// Absolute MIDI note number for a semitone `offset` from `harmony.root` — the same
/// `60 + root_semitone + offset` anchor as [`crate::voice::pitch_to_hz`] (root octave 4),
/// rounded to the nearest semitone and clamped to `0..=127`. `cents_offset` is dropped in
/// plain-MIDI mode (documented in ADR 0026; an MPE mode would restore it as pitch bend).
pub fn midi_note_number(root_semitone: f32, offset_st: f32) -> u8 {
    (60.0 + root_semitone + offset_st).round().clamp(0.0, 127.0) as u8
}

/// Quantize a normalized `0..1` effective param value to a 7-bit CC value.
pub fn cc_value(norm: f32) -> u8 {
    (norm.clamp(0.0, 1.0) * 127.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(entries: &[(&str, u8, u8)]) -> BTreeMap<String, (u8, u8)> {
        entries.iter().map(|&(id, n, v)| (id.to_string(), (n, v))).collect()
    }
    fn msgs(out: &[MidiEvent]) -> Vec<MidiMsg> {
        out.iter().map(|e| e.msg).collect()
    }

    #[test]
    fn appearing_voice_emits_note_on_with_its_velocity() {
        let mut prev = BTreeMap::new();
        let mut out = Vec::new();
        diff_notes(&mut prev, &set(&[("v1", 62, 96)]), 3, 480, &mut out);
        assert_eq!(msgs(&out), vec![MidiMsg::NoteOn { ch: 3, note: 62, vel: 96 }]);
        assert_eq!(out[0].frame, 480);
        assert_eq!(prev, set(&[("v1", 62, 96)]), "emitted set adopted");
    }

    #[test]
    fn sustained_voice_emits_nothing() {
        let mut prev = set(&[("v1", 62, 96)]);
        let mut out = Vec::new();
        diff_notes(&mut prev, &set(&[("v1", 62, 96)]), 0, 0, &mut out);
        assert!(out.is_empty(), "same voice, same note ⇒ external synth keeps holding");
    }

    #[test]
    fn pitch_change_emits_on_before_off_for_legato_overlap() {
        let mut prev = set(&[("v1", 62, 96)]);
        let mut out = Vec::new();
        diff_notes(&mut prev, &set(&[("v1", 64, 96)]), 0, 100, &mut out);
        assert_eq!(
            msgs(&out),
            vec![MidiMsg::NoteOn { ch: 0, note: 64, vel: 96 }, MidiMsg::NoteOff { ch: 0, note: 62 }],
            "On(new) before Off(old) so external portamento glides"
        );
    }

    #[test]
    fn vanished_voice_emits_note_off() {
        let mut prev = set(&[("v1", 62, 96), ("v2", 69, 96)]);
        let mut out = Vec::new();
        diff_notes(&mut prev, &set(&[("v1", 62, 96)]), 0, 0, &mut out);
        assert_eq!(msgs(&out), vec![MidiMsg::NoteOff { ch: 0, note: 69 }]);
    }

    #[test]
    fn unison_is_ref_counted() {
        // two voices on one note# (a cents-detuned double): one On when it appears…
        let mut prev = BTreeMap::new();
        let mut out = Vec::new();
        diff_notes(&mut prev, &set(&[("v1", 62, 96), ("v2", 62, 80)]), 0, 0, &mut out);
        assert_eq!(msgs(&out), vec![MidiMsg::NoteOn { ch: 0, note: 62, vel: 96 }], "one On; first sorted voice's velocity");
        // …no Off while its sibling still holds it…
        out.clear();
        diff_notes(&mut prev, &set(&[("v1", 62, 96)]), 0, 10, &mut out);
        assert!(out.is_empty(), "sibling still holds the note");
        // …and the Off fires only when the last holder vanishes.
        out.clear();
        diff_notes(&mut prev, &BTreeMap::new(), 0, 20, &mut out);
        assert_eq!(msgs(&out), vec![MidiMsg::NoteOff { ch: 0, note: 62 }]);
    }

    #[test]
    fn voice_swap_on_one_note_is_silent() {
        // v1 hands the same note# to v2 (steal + re-articulation) — externally nothing changed.
        let mut prev = set(&[("v1", 62, 96)]);
        let mut out = Vec::new();
        diff_notes(&mut prev, &set(&[("v2", 62, 100)]), 0, 0, &mut out);
        assert!(out.is_empty(), "note count unchanged ⇒ no retrigger");
    }

    #[test]
    fn flush_emits_one_off_per_distinct_note() {
        let mut st = MidiOutState { channel: 2, notes: set(&[("v1", 62, 96), ("v2", 62, 80), ("v3", 69, 96)]), cc_last: BTreeMap::new() };
        let mut out = Vec::new();
        flush_notes(&mut st, 0, &mut out);
        assert_eq!(
            msgs(&out),
            vec![MidiMsg::NoteOff { ch: 2, note: 62 }, MidiMsg::NoteOff { ch: 2, note: 69 }],
            "unison flushed once; sorted by voice_id → note order deterministic"
        );
        assert!(st.notes.is_empty());
    }

    #[test]
    fn note_number_math() {
        // D root (2), offset 0 ⇒ D4 = 62 (pitch_to_hz's anchor: 60 + root + offset).
        assert_eq!(midi_note_number(2.0, 0.0), 62);
        assert_eq!(midi_note_number(2.0, 7.0), 69);
        // fractional offsets round to the nearest semitone (cents dropped in plain MIDI)
        assert_eq!(midi_note_number(0.0, 3.4), 63);
        assert_eq!(midi_note_number(0.0, 3.6), 64);
        // clamped to the 7-bit range
        assert_eq!(midi_note_number(0.0, 90.0), 127);
        assert_eq!(midi_note_number(0.0, -90.0), 0);
    }

    #[test]
    fn cc_quantization() {
        assert_eq!(cc_value(0.0), 0);
        assert_eq!(cc_value(1.0), 127);
        assert_eq!(cc_value(0.5), 64); // 63.5 rounds half-up
        assert_eq!(cc_value(-0.2), 0);
        assert_eq!(cc_value(1.7), 127);
    }

    fn mirror_msgs(out: &[TapEvent]) -> Vec<TapMsg> {
        out.iter().map(|e| e.msg).collect()
    }
    fn fresh(keys: &[&str]) -> BTreeSet<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn mirror_diff_matches_the_translator_without_fresh_strikes() {
        let mut prev = set(&[("s::v1", 62, 96)]);
        let mut out = Vec::new();
        diff_tap(&mut prev, &set(&[("s::v1", 64, 96), ("s::v2", 69, 80)]), &BTreeSet::new(), "pad", 7, &mut out);
        assert_eq!(
            mirror_msgs(&out),
            vec![TapMsg::NoteOn { note: 64, vel: 96 }, TapMsg::NoteOn { note: 69, vel: 80 }, TapMsg::NoteOff { note: 62 }]
        );
        assert!(out.iter().all(|e| e.frame == 7 && e.inst == "pad"));
        out.clear();
        diff_tap(&mut prev, &set(&[("s::v1", 64, 96), ("s::v2", 69, 80)]), &BTreeSet::new(), "pad", 9, &mut out);
        assert!(out.is_empty(), "a held set sends nothing");
    }

    #[test]
    fn a_fresh_strike_on_a_sounding_note_re_strikes() {
        // A generator strikes D4 again while its last D4 still holds (anonymous voices, new key):
        // the audio re-attacks, so the MIDI sends Off then On.
        let mut prev = set(&[("g::g@0#0", 62, 90)]);
        let mut out = Vec::new();
        let want = set(&[("g::g@0#0", 62, 90), ("g::g@400#0", 62, 70)]);
        diff_tap(&mut prev, &want, &fresh(&["g::g@400#0"]), "pluck", 400, &mut out);
        assert_eq!(mirror_msgs(&out), vec![TapMsg::NoteOff { note: 62 }, TapMsg::NoteOn { note: 62, vel: 70 }]);
        // A steal that hands the note to a fresh voice re-strikes too (the translator stays silent).
        let mut prev = set(&[("s::v1", 62, 96)]);
        out.clear();
        diff_tap(&mut prev, &set(&[("s::v2", 62, 100)]), &fresh(&["s::v2"]), "pad", 0, &mut out);
        assert_eq!(mirror_msgs(&out), vec![TapMsg::NoteOff { note: 62 }, TapMsg::NoteOn { note: 62, vel: 100 }]);
    }

    #[test]
    fn re_strikes_come_first_then_ons_then_offs() {
        let mut prev = set(&[("s::a", 60, 100), ("s::b", 62, 100)]);
        let mut out = Vec::new();
        // a re-strikes 60, b moves 62 → 64.
        let want = set(&[("s::a2", 60, 90), ("s::b", 64, 100)]);
        diff_tap(&mut prev, &want, &fresh(&["s::a2"]), "x", 0, &mut out);
        assert_eq!(
            mirror_msgs(&out),
            vec![
                TapMsg::NoteOff { note: 60 },
                TapMsg::NoteOn { note: 60, vel: 90 },
                TapMsg::NoteOn { note: 64, vel: 100 },
                TapMsg::NoteOff { note: 62 },
            ]
        );
    }

    #[test]
    fn a_fresh_key_that_was_stolen_or_already_held_is_not_re_struck() {
        let mut prev = set(&[("s::v1", 62, 96)]);
        let mut out = Vec::new();
        // the fresh voice lost the steal (absent from `desired`) …
        diff_tap(&mut prev, &set(&[("s::v1", 62, 96)]), &fresh(&["s::v9"]), "x", 0, &mut out);
        assert!(out.is_empty());
        // … and a key already sounding isn't a new strike.
        diff_tap(&mut prev, &set(&[("s::v1", 62, 96)]), &fresh(&["s::v1"]), "x", 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn unison_mirror_is_ref_counted_and_flushes_once() {
        let mut prev = BTreeMap::new();
        let mut out = Vec::new();
        diff_tap(&mut prev, &set(&[("s::v1", 62, 96), ("s::v2", 62, 80)]), &fresh(&["s::v1", "s::v2"]), "x", 0, &mut out);
        assert_eq!(mirror_msgs(&out), vec![TapMsg::NoteOn { note: 62, vel: 96 }], "one On for a new note");
        let mut st = TapState { notes: prev };
        out.clear();
        flush_tap(&mut st, "x", 5, &mut out);
        assert_eq!(mirror_msgs(&out), vec![TapMsg::NoteOff { note: 62 }]);
        assert!(st.notes.is_empty());
    }

    #[test]
    fn midi_msg_serde_roundtrip() {
        // The wire shape the session/FFI JSONL stream uses.
        let e = MidiEvent { frame: 42, msg: MidiMsg::NoteOn { ch: 0, note: 62, vel: 100 } };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"note_on\""), "tagged enum: {s}");
        let back: MidiEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back, e);
    }
}
