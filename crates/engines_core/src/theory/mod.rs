//! Music-theory core — the "theory in code" for the Orrery generative layer (ADR pending;
//! see `doc/decisions/orrery-generative-harmony-design.md`).
//!
//! Pure, deterministic, **I/O-free**. Everything works in **semitone offsets from the
//! harmony root** (the engine's note unit — see [`crate::voice::pitch_to_hz`]), so output
//! drops straight into [`crate::loop_doc::NoteVoice::offset`] with no conversion. This module
//! owns scales/modes, diatonic + borrowed chord construction, functional harmony + resolution
//! tendencies (the gravity *direction*), a tension metric (the orbital *radius*), voice-leading
//! (the smooth morph), and seeded melodic generation. It is deliberately isolated from the
//! DSP/controller code so the music knowledge lives in one place.

pub mod chord;
pub mod chord_fit;
pub mod function;
pub mod melody;
pub mod melody_fit;
pub mod phrase_fit;
pub mod pitch;
pub mod scale;
pub mod tension;
pub mod voicelead;

pub use chord::{identify, Chord, ChordName, Extension, Quality, Source};
pub use chord_fit::chord_fit;
pub use melody_fit::melody_fit;
pub use function::{function_of, resolution_targets, roman, roman_of, Function};
pub use melody::{next_phrase, MelodyParams, Motif};
pub use pitch::{interval_class, midi_of_offset, note_name, pc_name, pc_of_root, Key, Pc};
pub use scale::{Mode, Scale};
pub use tension::tension;
pub use voicelead::{lead, lead_pcs, LeadOpts, VoiceId, Voicing};
