//! Read-only runtime observation (ADR 0010 / ADR 0031): the latest per-loop [`Telemetry`] snapshot
//! the shell/TUI shows (operating point, constraint status, Orrery state, track audibility, song
//! position) and the [`LoopRecord`] stream — one record per rendered loop, in order. The core
//! *writes* both; nothing in the core reads them (they never affect rendering — CE-R7.3 holds).

use crate::categorical::CategoricalMove;
use crate::orrery::HarmonicMove;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The latest per-loop telemetry snapshot (only the most recent is kept — not a history).
#[derive(Clone, Debug, Default, Serialize)]
pub struct Telemetry {
    pub loop_id: u64,
    /// Active control revision (0 without a control).
    pub revision: u64,
    pub optimize: String,
    /// Param perturbed this loop (`""` if none / a categorical or constraint loop reports its path).
    pub chosen_path: String,
    /// Operating point Pₙ (native-unit doc mirror) the loop was rendered from.
    pub operating_point: Value,
    pub constraints: Vec<ConstraintStatus>,
    pub orrery: Option<OrreryTelemetry>,
    /// Per-instrument audibility this loop (S1-h5): the param table shows knob positions, not whether a
    /// voice is actually sounding — a voice can be silent (no notes, an Orrery `drop`, a euclid
    /// `pulses=0`, a returned-home rest) while its `level_db` still reads full. This surfaces the rendered
    /// dry RMS + `enabled` so the TUI can flag an inaudible track regardless of *why* it went quiet.
    #[serde(default)]
    pub tracks: Vec<TrackStatus>,
    /// Song position (ADR 0029) — `None` for an open-ended (song-less) session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub song: Option<SongTelemetry>,
}

/// Where a song (ADR 0029) is this loop: block / loop counters, seam + tail flags, and the
/// audio-time position (`elapsed_s` = this loop's start; both in rendered-audio seconds).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SongTelemetry {
    /// Current block index (the last block's index during the tail).
    pub block: usize,
    pub blocks: usize,
    pub label: String,
    pub loop_in_block: usize,
    pub block_loops: usize,
    /// This loop's index within the whole song (0-based).
    pub loop_in_song: usize,
    pub total_loops: usize,
    /// In the release tail after the last block.
    pub tail: bool,
    /// This loop renders a block-seam glide.
    pub seam: bool,
    pub elapsed_s: f64,
    pub total_s: f64,
}

/// One instrument's audibility for the latest loop (read-only telemetry; see [`Telemetry::tracks`]).
#[derive(Clone, Debug, Serialize)]
pub struct TrackStatus {
    pub name: String,
    /// Authored `enabled` flag (the authored axis — neither drift nor the Orrery toggles it).
    pub enabled: bool,
    /// Rendered dry RMS over the loop (pre-bus/width/gain); 0 ⇒ produced no signal.
    pub rms: f32,
    /// `enabled && rms > AUDIBLE_EPS` — false flags a voice that is silent in the audio despite its knobs.
    pub audible: bool,
}

/// RMS below this counts as inaudible (≈ −80 dBFS) — a voice with no notes / a drop / `pulses=0`.
/// Internal threshold (not part of the C ABI); the audibility verdict is precomputed in `TrackStatus`.
pub(crate) const AUDIBLE_EPS: f32 = 1e-4;

#[derive(Clone, Debug, Serialize)]
pub struct ConstraintStatus {
    pub path: String,
    pub value: f32,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub in_range: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrreryTelemetry {
    pub chord: String,
    pub tension: f32,
    pub loops_since_home: u32,
    /// Moving average of voice-changes-per-phrase (the Phase 5 governor's signal); 0 until measured.
    pub voice_change_avg: f32,
    /// The active arrangement leader this phrase (`bass`/`arp`), if any (Phase 5b leader-led chains).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    /// The Arranger's slow energy arc (0..1) — the producer "intensity" knob (Phase 6b).
    #[serde(default)]
    pub energy: f32,
    /// The arp's current rhythmic density (euclid pulses) the Arranger is riding (Phase 6b).
    #[serde(default)]
    pub arp_pulses: u32,
    /// The role silenced by a narrative drop this phrase, if any (Phase 6c).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<String>,
    /// Whether a homeward return fired this phrase (breach snap / chord cadence / track rest) — Phase 6d.
    #[serde(default)]
    pub returning_home: bool,
    /// The sequence-pool entry the primary arp's phrase arm is playing, if phrase-armed (ADR 0028).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phrase_active: Option<String>,
    /// The pad voicing (semitone offsets from the root, one per pad voice), the bass offset and
    /// the chord's tones — what the editor panel's keyboard shows (ADR 0038).
    #[serde(default)]
    pub pad: Vec<i32>,
    #[serde(default)]
    pub bass: i32,
    #[serde(default)]
    pub tones: Vec<i32>,
}

/// A loop's place in a song (ADR 0029): the block, and how far into it (ADR 0040 — the panel's
/// structure-bar playhead).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SongStamp {
    pub block: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seam: bool,
    /// This loop's index within its block, 0-based (within the tail during the tail).
    #[serde(default)]
    pub loop_in_block: u32,
    /// Loops in the block (`tail_loops` during the tail).
    #[serde(default)]
    pub block_loops: u32,
    /// In the release tail after the last block.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tail: bool,
}

impl From<&SongTelemetry> for SongStamp {
    fn from(t: &SongTelemetry) -> Self {
        SongStamp { block: t.block as u32, seam: t.seam, loop_in_block: t.loop_in_block as u32, block_loops: t.block_loops as u32, tail: t.tail }
    }
}

/// One rendered loop, in render order (ADR 0031) — the slim per-loop observation stream that
/// replaced the learner's sample corpus: what the controller did, where in a song, how loud.
/// Drained via `Session::poll_records`; `engines-song --loop-log` writes them as JSON lines.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRecord {
    pub loop_id: u64,
    /// Control revision in force (0 without a control).
    pub revision: u64,
    pub optimize: String,
    /// Absolute session frame of the loop's first sample (the shell's frames-played origin).
    pub start_frame: u64,
    pub num_frames: usize,
    /// The param the controller stepped this loop (`""` = none) and its signed native step.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default)]
    pub delta: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harmonic_move: Option<HarmonicMove>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categorical_move: Option<CategoricalMove>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song: Option<SongStamp>,
    /// Sample peak |x| of the master mix over the loop.
    pub peak: f32,
    /// The shell's revision number of the doc this loop played (`engines-live`'s history numbering,
    /// ADR 0034) — set via `SessionConfig::doc_rev` / `Session::apply_doc_rev`; 0 = untracked.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub doc_rev: u64,
    /// The Orrery's state after this loop's pick (chord, tension, voicing) — carried per record so
    /// a reader draining several loops at once labels each with its own chord. `None` without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orrery: Option<OrreryTelemetry>,
    /// This loop's note-source structure (ADR 0048) when the session logs it for a MIDI log;
    /// `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_log: Option<crate::engine::LoopNoteLog>,
}

fn is_zero_u64(x: &u64) -> bool {
    *x == 0
}

/// A mid-loop edit took effect (live mode, ADR 0035): from `abs_frame` on, the audio crossfades
/// (over `xfade_frames`) into revision `doc_rev`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Applied {
    /// The shell's revision number (`Session::apply_doc_rev`; 0 = untracked).
    pub doc_rev: u64,
    pub loop_id: u64,
    /// Absolute session frame where the new doc starts sounding.
    pub abs_frame: u64,
    /// Frame within the loop (after any loop-length remap).
    pub pos: usize,
    pub kind: crate::engine::EditKind,
    pub xfade_frames: usize,
}

/// One rendered block (live mode): where playback is heading, its master peak and per-instrument
/// peaks (live meters; post live-gain, pre bus/width/master).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BlockTelemetry {
    pub loop_id: u64,
    /// Absolute session frame of the block's first sample.
    pub abs_frame: u64,
    /// Frame within the loop of the block's first sample.
    pub pos: usize,
    /// Frames in the loop.
    pub total: usize,
    pub frames: usize,
    /// Master sample peak, `max(left, right)` (post width/gain, pre any device limiter).
    pub peak: f32,
    /// The master peak per channel, `[left, right]` (ADR 0040).
    pub peak_lr: [f32; 2],
    /// Per-instrument `[left, right]` peaks (ADR 0040; was one `max` value).
    pub meters: Vec<(String, [f32; 2])>,
    /// The song position of this block's loop (`None` without a song).
    pub song: Option<SongStamp>,
}
