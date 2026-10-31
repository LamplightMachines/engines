//! Engine: stateful block/loop renderer + phase snapshot (CE-R3.2, CE-R4, CE-R7).
//!
//! Synthesis runs continuously; only the control timeline loops (CE-R1.1). The `Engine`
//! owns persistent DSP state (per-voice filters, shared reverb) so tails/transients
//! cross the loop seam (CE-R4.4). Modulated destinations are smoothed per-sample
//! (zipper-free, CE-R4.5). Determinism: a fresh `Engine` + a given `(LoopDoc,
//! PhaseSnapshot)` reproduces bit-identically (CE-R7.3).

use crate::dsp::{adsr_env, osc, soft_clip, PitchBand, Smoothed, Svf, Wave};
use crate::effects::Bus;
use crate::hash::{fnv1a_bytes, hash64, hash_site, hash_to_unit};
use crate::loop_doc::{param_denorm, param_norm, resolve_dest, Dest, LoopDoc, SHIFT_RANGE};
use crate::matrix::{enforce_polyphony, SourceVoices};
use crate::midi::{
    cc_value, diff_tap, diff_notes, flush_tap, flush_notes, midi_note_number, tap_key, MidiEvent, MidiMsg, MidiOutState,
    TapEvent, TapState,
};
use crate::modulator::{evaluate_modulator, step_duration_frac, CONTROL_HOP};
use crate::synth::{
    binaural_root, binaural_sample, dual_sample, modal_sample, Aa2x, AmpPolicy, DualParams, ModalParams, SynthKind,
    SynthVoiceState,
};
use crate::voice::{apply_note_event, pitch_to_hz, root_semitone, synthetic_voice_id, Action};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Cross-loop state (serializable; CE-R7.2) ────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModulatorPhase {
    pub step_index: i64,
    pub frames_into_step: i64,
    pub lfo_phase: f32,
    pub last_output: f32,
    /// Note events whose onset spilled past the loop seam (a sub-step `t` near the loop
    /// edge of a free-run grid, ADR 0028) — emitted at the top of the next loop instead
    /// of clamped/dropped. Empty (and skipped when serializing) for pre-0028 docs, so
    /// legacy snapshots stay byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_notes: Vec<PendingNote>,
    /// Phrase-arm tile clock (ADR 0028): the absolute host-step index where the current
    /// tile started (free-run only; loop-sync re-anchors at 0) …
    #[serde(default, skip_serializing_if = "i64_is_zero")]
    pub phrase_anchor_step: i64,
    /// … and the count of tiles started so far — the seed for per-tile selection and prob
    /// rolls (loop_id-free, so a tile straddling the seam decides identically on both
    /// sides). Carried for BOTH sync modes. Zero (and skipped) for phrase-less docs.
    #[serde(default, skip_serializing_if = "i64_is_zero")]
    pub phrase_ordinal: i64,
}
fn i64_is_zero(x: &i64) -> bool {
    *x == 0
}

/// A carried-over note event: `frame` is relative to the NEXT loop's start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingNote {
    pub frame: i64,
    /// `NoteVoice.id` is never serialized in a doc (the voicing MAP key carries it), but a
    /// pending voicing is a plain list — so its ids ride an explicit `id` field instead.
    /// Without this a JSON'd snapshot resumed a carried `p{k}` strike as an anonymous voice
    /// and silently dropped its paired release (a stuck gate; CE-R7.3 breach).
    #[serde(with = "pending_voicing")]
    pub voicing: Vec<crate::loop_doc::NoteVoice>,
}

mod pending_voicing {
    use crate::loop_doc::NoteVoice;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct PendingVoice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(flatten)]
        voice: NoteVoice,
    }

    pub fn serialize<S: Serializer>(v: &[NoteVoice], s: S) -> Result<S::Ok, S::Error> {
        let wrapped: Vec<PendingVoice> = v.iter().map(|nv| PendingVoice { id: nv.id.clone(), voice: nv.clone() }).collect();
        wrapped.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<NoteVoice>, D::Error> {
        let wrapped: Vec<PendingVoice> = Vec::deserialize(d)?;
        Ok(wrapped
            .into_iter()
            .map(|w| {
                let mut nv = w.voice;
                nv.id = w.id;
                nv
            })
            .collect())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActiveVoice {
    pub voice_id: String,
    pub source_id: String,
    pub pitch_semitones: f32,
    pub cents_offset: f32,
    pub phase_rad: f32,
    /// The shifted (octave-pedal) oscillator's own phase; advances at `pitch + shift_st` and is
    /// mixed against `phase_rad` by `shift_mix` (CE-R7.3 determinism — carried across loops).
    #[serde(default)]
    pub phase_rad_shift: f32,
    /// Seconds since articulation (the envelope clock; accumulates across loops).
    pub elapsed_decay_s: f32,
    /// `Some(t)` = note-off at `t` seconds-since-articulation → in the ADSR release tail
    /// (CE-R4.3); `None` = held. Carried across loops until the tail completes.
    #[serde(default)]
    pub released_at_s: Option<f32>,
    /// Per-synth-type DSP state (ADR 0009): dual-osc modulator phase/feedback, modal resonator
    /// memories. Carried across loops for determinism (CE-R7.3). Subtractive ⇒ `None`, which is
    /// skipped when serializing so legacy snapshots stay byte-identical.
    #[serde(default, skip_serializing_if = "SynthVoiceState::is_none")]
    pub synth_state: SynthVoiceState,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PhaseSnapshot {
    pub prev_loop_id: u64,
    pub modulator_phases: BTreeMap<String, ModulatorPhase>,
    /// Keyed by instrument note-target (e.g. "poly.notes").
    pub active_voices: BTreeMap<String, Vec<ActiveVoice>>,
    /// Last-rendered depth per matrix entry id (depth slew across loops, CE-R5.4).
    pub connection_depths: BTreeMap<String, f32>,
    /// Per-`type:"midi"`-instrument emitted state (ADR 0026): the last-sent note set +
    /// CC values, keyed by instrument id — what Note-On/Off diffing and CC on-change
    /// reconcile against across the loop seam (part of the deterministic state, CE-R7.3).
    /// Empty (and skipped when serializing) unless the doc has midi instruments, so
    /// legacy snapshots stay byte-identical.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub midi_out: BTreeMap<String, MidiOutState>,
    /// Per-mirrored-instrument emitted notes (ADR 0048), keyed by instrument id. Kept apart from
    /// `midi_out` (whose stale entries are flushed every loop). Empty (and skipped when
    /// serializing) unless the engine mirrors, so every other snapshot stays byte-identical.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub note_tap: BTreeMap<String, TapState>,
    /// A song block seam in progress (ADR 0029): the glide from the outgoing block's operating
    /// point into the new block, consumed by the next render(s). Part of the deterministic state
    /// (CE-R7.3). `None` (and skipped when serializing) outside a seam, so every non-song render and
    /// snapshot stays byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seam: Option<Seam>,
}

impl PhaseSnapshot {
    pub fn empty() -> Self {
        Self::default()
    }
}

/// A block-to-block glide (ADR 0029), carried on the [`PhaseSnapshot`]. Everything decays by the
/// same raised-cosine gain `g` (1 → 0 over `total_frames`, measured from the seam's first frame):
/// - `param_offsets`: `dest → old − new` (normalized) — added to the new base as a synthetic
///   modulation contribution, so the param starts where the old block left it and eases home;
/// - `depth_from`: `route → old depth` (0 for a brand-new or re-targeted route) — the route's depth
///   glides from there to its new value;
/// - `fading_routes`: mod routes the new block dropped (or re-targeted) — still rendered, at
///   `depth · g`, until they reach zero;
/// - `release_orphans`: at the seam's first frame, release every carried voice whose
///   `(instrument, source)` note route no longer exists (otherwise it would sustain forever).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Seam {
    pub total_frames: u64,
    #[serde(default)]
    pub elapsed_frames: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub param_offsets: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub depth_from: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fading_routes: BTreeMap<String, FadingRoute>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub release_orphans: bool,
}

/// A mod route the new block removed, faded out over the seam (ADR 0029).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FadingRoute {
    pub source: String,
    pub dest: String,
    pub depth: f32,
}

impl Seam {
    /// A hard cut that still releases notes whose route disappeared (`transition_s: 0`, the tail).
    pub fn release_only() -> Self {
        Self { release_orphans: true, ..Default::default() }
    }

    /// The glide gain at `frame` of the current render: a raised cosine from 1 (seam start) to 0
    /// (after `total_frames`), smooth at both ends. 0 for a hard cut.
    pub fn gain_at(&self, frame: usize) -> f32 {
        let t = self.elapsed_frames + frame as u64;
        if self.total_frames == 0 || t >= self.total_frames {
            return 0.0;
        }
        let x = t as f64 / self.total_frames as f64;
        (0.5 * (1.0 + (std::f64::consts::PI * x).cos())) as f32
    }

    /// Still gliding at this render's first frame?
    fn active(&self) -> bool {
        self.elapsed_frames < self.total_frames
    }
}

/// The instrument params the per-voice render reads through a modulation slot (`InstBase.idx`) —
/// i.e. the matrix-modulatable (and seam-glideable) set.
pub const MODULATABLE_INST_PARAMS: &[&str] = &[
    "level_db", "cutoff_hz", "resonance", "drive_db", "attack_s", "decay_s", "sustain", "release_s",
    "band_lo_hz", "band_hi_hz", "shift_st", "shift_mix",
    // Dual-osc (ADR 0009)
    "osc_a_level_db", "osc_b_level_db", "osc_b_ratio", "osc_b_detune_cents", "fm_index", "fm_feedback",
    // Modal (ADR 0009)
    "t60_s", "ring_ratio", "brightness", "strike_pos", "stretch",
    // Binaural (ADR 0047) — native Hz, read as authored unless a route/seam moves them
    "root_hz", "spread_hz",
];

/// Can a song seam glide this dest (ADR 0029)? True where the engine reads the param through a
/// modulation slot: the whitelisted instrument params, a midi instrument's CC-mapped params,
/// `global.master_gain_db`/`stereo_width`, bus effect params (except the build-time `width`), and bus
/// return gains. Everything else (categoricals, `voices_detune_cents`, `glide_ms`) steps.
pub(crate) fn is_glideable_dest(dest: &str, doc: &LoopDoc) -> bool {
    match resolve_dest(dest) {
        Dest::InstParam { inst, param } => {
            MODULATABLE_INST_PARAMS.contains(&param.as_str())
                || doc.instruments.get(&inst).is_some_and(|i| {
                    i.is_midi() && i.routing.as_ref().is_some_and(|r| r.cc.contains_key(&param))
                })
        }
        Dest::Global { param } => matches!(param.as_str(), "master_gain_db" | "stereo_width"),
        Dest::BusParam { param, .. } => param != "width",
        Dest::BusReturn { .. } => true,
        _ => false,
    }
}

// ── Render result ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct RenderResult {
    /// Interleaved stereo: `[L0, R0, L1, R1, …]`.
    pub audio: Vec<f32>,
    pub channels: u32,
    pub num_frames: usize,
    pub next_snapshot: PhaseSnapshot,
    /// Effective (post-modulation) values at loop end.
    pub effective_state: BTreeMap<String, f32>,
    /// Per-instrument dry RMS over the loop (pre-bus/width/gain), keyed by instrument id. Observation
    /// only (CE-R7.3 unaffected) — lets the shell/TUI flag a voice that is inaudible even though its
    /// `level_db` knob reads full (S1-h5). Absent ⇒ the instrument produced no dry signal this loop.
    pub per_inst_rms: BTreeMap<String, f32>,
    /// Live-viz observation (LiveEdit/instrument screen, ADR 0024): per-hop (25 Hz / 40 ms) raw mono
    /// master audio for the spectrogram. Empty unless [`Engine::set_viz`] is on, so non-TUI renders
    /// are byte-identical and pay nothing (CE-R7.3 unaffected — off the determinism path).
    pub viz_master_hops: Vec<Vec<f32>>,
    /// Per-hop raw mono **dry** audio of the [`Engine::set_viz_instrument`]-selected instrument
    /// (empty if none selected / viz off). Parallel-indexed with `viz_master_hops`.
    pub viz_inst_dry_hops: Vec<Vec<f32>>,
    /// Per-hop **normalized** effective (post-mod) param values keyed by JSONPath (the modulated set,
    /// like `effective_state`), for the instrument-screen oscilloscope. Parallel to the hop vecs.
    pub viz_eff_hops: Vec<BTreeMap<String, f32>>,
    /// Time-stamped MIDI events from `type:"midi"` instruments (ADR 0026), frame-ordered
    /// (CC before Note diffs within a frame). Deterministic — a pure function of
    /// `(doc, snapshot, loop_id)`, like the audio (CE-R7.3); the session re-bases `frame`
    /// onto the absolute sample clock and the shell delivers at playback time. Empty when
    /// the doc has no midi instruments.
    pub midi_events: Vec<MidiEvent>,
    /// The mirror's notes (ADR 0048): each [`Engine::set_note_tap`] instrument's held voices as
    /// Note-On/Off, frame-ordered, tagged with the instrument. Deterministic like `midi_events`;
    /// empty unless the engine mirrors.
    pub note_tap: Vec<TapEvent>,
    /// This loop's note-source structure for the mirror's offline log (ADR 0048), when
    /// [`Engine::set_note_log`] is on: `None` otherwise.
    pub note_log: Option<LoopNoteLog>,
    /// Multichannel stem split (ADR 0027): filled only when [`Engine::set_stem_capture`] is on
    /// AND the doc routes at least one instrument via `out_channel`. `audio` (the scored master
    /// mix) is byte-identical whether or not this is filled — stems are an *additional*,
    /// observation-only decomposition the session composes into an N-channel device stream.
    pub stems: Option<StemMix>,
    /// Per-source stem split (ADR 0030): filled only when [`Engine::set_source_capture`] is on.
    /// Like `stems`, observation-only — `audio` is byte-identical whether or not this is filled.
    pub sources: Option<SourceStems>,
}

/// One loop's note-source structure (ADR 0048): which sources fed each mirrored instrument, and
/// where those sources' passes started — what a MIDI log needs to cut its notes into one clip per
/// phrase pass. Observation only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoopNoteLog {
    /// Source (note modulator) id → its pass starts this loop.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiles: BTreeMap<String, Vec<crate::modulator::TileMark>>,
    /// Mirrored instrument → the note sources routed into it this loop (sorted).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub feeds: BTreeMap<String, Vec<String>>,
}

/// Per-`out_channel` stereo stems + the rest-of-mix (ADR 0027). Every buffer is interleaved
/// stereo with the master's per-sample stereo-width + master-gain chain applied, so summing
/// `rest + Σ stems` reconstructs `audio` (up to float association). `rest` carries every
/// un-routed instrument **plus all bus/reverb wet** (sends are a shared field; a routed
/// instrument's send still feeds its bus, whose return stays here).
#[derive(Clone, Debug, Default)]
pub struct StemMix {
    /// Interleaved stereo rest-of-mix — what the device's main 1/2 pair carries.
    pub rest: Vec<f32>,
    /// `out_channel` (odd, 1-based pair start: 3 ⇒ device channels 3/4) → interleaved stereo.
    /// Instruments sharing an `out_channel` are pre-summed.
    pub stems: BTreeMap<u16, Vec<f32>>,
}

/// One per-source stem's identity (ADR 0030): an audio instrument's dry, or a bus's wet return.
/// Instruments and buses are separate id maps (an instrument and a bus may share an id), so the
/// kind is part of the key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKey {
    Inst(String),
    Bus(String),
}

impl SourceKey {
    pub fn id(&self) -> &str {
        match self {
            SourceKey::Inst(id) | SourceKey::Bus(id) => id,
        }
    }
    /// `"instrument"` | `"bus"`.
    pub fn kind(&self) -> &'static str {
        match self {
            SourceKey::Inst(_) => "instrument",
            SourceKey::Bus(_) => "bus",
        }
    }
}

impl std::fmt::Display for SourceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceKey::Inst(id) => write!(f, "inst:{id}"),
            SourceKey::Bus(id) => write!(f, "bus:{id}"),
        }
    }
}

/// Per-source stems (ADR 0030): each audio instrument's dry (post level/pan, pre-bus) and each
/// internal bus's wet return (post `return_gain_db`), every buffer interleaved stereo with the master's
/// per-sample stereo-width + master-gain chain applied. The master is exactly Σ instrument dry +
/// Σ bus wet through that linear chain, so `Σ inst + Σ bus` reconstructs `audio` (up to float
/// association). An instrument's reverb lives in its bus's stem — the usual DAW stems + send
/// returns split.
#[derive(Clone, Debug, Default)]
pub struct SourceStems {
    /// Instrument id → interleaved stereo. Only instruments this render actually played (enabled,
    /// non-midi) have a buffer; a session composes absent ones as silence.
    pub inst: BTreeMap<String, Vec<f32>>,
    /// Internal bus id → interleaved stereo wet return.
    pub bus: BTreeMap<String, Vec<f32>>,
    /// True once a retiring bus's tail (ADR 0035) reached the master during this render — audio no
    /// source buffer carries, so `Σ inst + Σ bus` falls short of `audio` from then on. A routed
    /// session (ADR 0044) carries the difference on its overflow pair.
    pub uncaptured: bool,
}

impl SourceStems {
    pub fn get(&self, key: &SourceKey) -> Option<&[f32]> {
        match key {
            SourceKey::Inst(id) => self.inst.get(id),
            SourceKey::Bus(id) => self.bus.get(id),
        }
        .map(Vec::as_slice)
    }
}

/// The per-source stem layout for `doc` (ADR 0030): every non-midi instrument in id order —
/// **including** authored-disabled ones, which an objectives `overrides` entry can enable
/// mid-session — then every internal (non-`external_return`) bus in id order. A session fixes this
/// at start, so a source's channel pair never moves.
pub fn source_layout(doc: &LoopDoc) -> Vec<SourceKey> {
    let insts = doc.instruments.iter().filter(|(_, i)| !i.is_midi()).map(|(id, _)| SourceKey::Inst(id.clone()));
    let buses = doc.buses.iter().filter(|(_, b)| !b.is_external_return()).map(|(id, _)| SourceKey::Bus(id.clone()));
    insts.chain(buses).collect()
}

/// The master gain alone, applied in place to one interleaved frame of a stem buffer — a binaural
/// pair's stem (ADR 0047), whose dry skips the width stage on the master too.
#[inline]
fn gain_in_place(buf: &mut [f32], frame: usize, gain: f32) {
    buf[2 * frame] *= gain;
    buf[2 * frame + 1] *= gain;
}

/// The master's stereo-width (mid/side) + gain stage, applied in place to one interleaved frame of
/// a stem buffer — the identical expression the master uses, so stems sum back to it.
#[inline]
fn width_gain_in_place(buf: &mut [f32], frame: usize, side_gain: f32, gain: f32) {
    let (bl, br) = (buf[2 * frame], buf[2 * frame + 1]);
    let m = (bl + br) * 0.5;
    let sd = (br - bl) * 0.5;
    buf[2 * frame] = (m - sd * side_gain) * gain;
    buf[2 * frame + 1] = (m + sd * side_gain) * gain;
}

// ── Engine ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Engine {
    sample_rate: f32,
    /// Internal send buses (effects chains), keyed by bus id. Built lazily on first sight and
    /// **reused** across loops so reverb tails cross the loop seam (CE-R4.4); rebuilt only when
    /// the doc edits the bus's effect chain, dropped when the bus is removed or retyped to
    /// `external_return`.
    buses: BTreeMap<String, Bus>,
    /// Persistent per-voice filters keyed "inst::voice_id": (lowpass, optional second filter —
    /// the noise bandpass's highpass, or a `binaural` voice's right-channel lowpass, ADR 0047;
    /// a binaural voice is never noise, so the slot is never double-booked). Survive across loops;
    /// reset on ARTICULATE.
    filters: BTreeMap<String, (Svf, Option<Svf>)>,
    /// Persistent per-voice 2× anti-alias decimators for `dual` voices, keyed "inst::voice_id"
    /// (ADR 0009). Like `filters`: survive across loops, reset on ARTICULATE, pruned with voices.
    aa_filters: BTreeMap<String, Aa2x>,
    /// Persistent per-voice note bands for pitched `filtered_noise` voices, keyed "inst::voice_id"
    /// (ADR 0046). Like `aa_filters`: created on the first sample, reset on ARTICULATE, pruned with
    /// voices.
    pitch_bands: BTreeMap<String, PitchBand>,
    /// Live-viz tap (ADR 0024): when `true`, `render` fills the `RenderResult.viz_*` hop buffers.
    /// Off (default) ⇒ byte-identical, zero-cost. Set only by the TUI/Session for the instrument
    /// screen. Carried in `#[derive(Clone)]` so a standby/crossfade engine inherits the setting.
    viz_enabled: bool,
    /// Which instrument's raw dry hops to capture (bandwidth: only the selected one flows). `None` ⇒
    /// master hops + effective snapshots only.
    viz_inst: Option<String>,
    /// Stem-split tap (ADR 0027): when `true` and the doc has `out_channel` routes, `render` also
    /// fills `RenderResult.stems`. Off (default) ⇒ byte-identical, zero-cost — like `viz_enabled`,
    /// strictly off the determinism path. Set by the session when the output device is multichannel.
    stem_capture: bool,
    /// Per-source tap (ADR 0030): when `true`, `render` also fills `RenderResult.sources`. Off
    /// (default) ⇒ byte-identical, zero-cost. Set by the session for a stem export.
    source_capture: bool,
    /// Buses taken out of service (removed, or their effect chain edited) ringing out their tails
    /// with no new sends (ADR 0035). Empty in any render whose buses don't change.
    retiring: Vec<loop_render::Retiring>,
    /// Live gains (mute/solo, ADR 0035): per-instrument target and current ramp position. Empty ⇒
    /// every gain is exactly 1 and the render is byte-identical.
    live_gain_target: BTreeMap<String, f32>,
    live_gain_cur: BTreeMap<String, f32>,
    /// Per-instrument block peaks for live meters (observation only).
    metering: bool,
    /// Instruments whose notes are mirrored as MIDI (ADR 0048). A shell setting, never doc content;
    /// it only reads voice state, so `audio` is byte-identical whatever it holds.
    note_tap: std::collections::BTreeSet<String>,
    /// Report each loop's [`LoopNoteLog`] (ADR 0048; a MIDI log for offline renders).
    note_log: bool,
}

#[inline]
fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// Map a normalized 0..1 cutoff to Hz (≈80 Hz .. 8 kHz, log). `pub` so the TUI units registry
/// (ADR 0024) reports the same Hz the engine renders — one source of truth, no drift.
#[inline]
pub fn cutoff_to_hz(c: f32, nyq: f32) -> f32 {
    crate::params::CUTOFF.eval(c).min(nyq)
}
/// Map a normalized 0..1 bandpass edge to Hz (20 Hz .. 4 kHz, log). `pub` for the TUI registry.
#[inline]
pub fn band_to_hz(c: f32, nyq: f32) -> f32 {
    crate::params::BAND.eval(c).min(nyq)
}
/// Inverse of [`cutoff_to_hz`]: Hz → normalized 0..1 (clamped), so the TUI can accept a Hz constraint
/// in the units it displays and store it in the engine's normalized space (ADR 0024). Co-located with
/// the forward map so the curve stays one source of truth.
#[inline]
pub fn hz_to_cutoff(hz: f32) -> f32 {
    crate::params::CUTOFF.inverse(hz)
}
/// Inverse of [`band_to_hz`]: Hz → normalized 0..1 (clamped).
#[inline]
pub fn hz_to_band(hz: f32) -> f32 {
    crate::params::BAND.inverse(hz)
}
/// `voices_detune_cents`, normalized (0..1 of 100 ¢) → voice `vi`'s detune in cents: the instrument's `n_total` sounding voices
/// spread evenly across ±`detune`·100 cents, the first lowest (one voice ⇒ none). `pub` for the
/// capability catalog's unit map.
#[inline]
pub fn voice_spread_cents(detune: f32, vi: usize, n_total: usize) -> f32 {
    if n_total > 1 {
        detune * VOICES_DETUNE_CENTS_MAX * (vi as f32 / (n_total - 1) as f32 - 0.5) * 2.0
    } else {
        0.0
    }
}

#[derive(Clone)]
struct InstBase {
    /// Synth type (ADR 0009): selects the per-voice source stage. `Subtractive` is the legacy path.
    kind: SynthKind,
    /// Release-tail length (s) used to prune released voices in step 7: ADSR release for
    /// subtractive/dual, 0 for the legacy `event` bell, the modal ring length for modal.
    release_tail_s: f32,
    wave: Wave,
    is_noise: bool,
    /// A `pitched` subtractive `filtered_noise` instrument (ADR 0046): its source band goes on
    /// through a band at each voice's note. `false` ⇒ the unpitched path, bit-identical to before.
    pitched_noise: bool,
    /// `event` slot keeps its bell `decay_s` envelope; all others use full ADSR (CE-R4.3).
    is_event: bool,
    level: f32,
    drive: f32,
    cutoff: f32,
    resonance: f32,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    band_lo: f32,
    band_hi: f32,
    detune: f32,
    /// Octave-pedal: static interval in semitones (`shift_st`) and wet/dry crossfade (`shift_mix`),
    /// used when not matrix-modulated. Defaults 0/0 ⇒ pure dry ⇒ bit-identical to no-shift.
    shift_st: f32,
    shift_mix: f32,
    glide_frames: usize,
    /// param name -> smoothed index (only modulated params present).
    idx: BTreeMap<String, usize>,
}

/// The envelope times' ranges in seconds (CE-R4.3; their normalized `0..1` is the seam's); sustain
/// is a 0..1 level. `DECAY_MAX_S` is the ADSR's decay and the legacy `event` bell's ring alike
/// (ADR 0054 P7). `pub` so the TUI units registry (ADR 0024) reports the same seconds the engine
/// renders.
pub const ATTACK_MAX_S: f32 = 3.0;
pub const DECAY_MAX_S: f32 = 4.0;
pub const RELEASE_MAX_S: f32 = 4.0;
/// `voices_detune_cents` at its top (normalized 1) spreads the outermost voices ± this many cents.
pub(crate) const VOICES_DETUNE_CENTS_MAX: f32 = 100.0;

impl Engine {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            buses: BTreeMap::new(),
            filters: BTreeMap::new(),
            aa_filters: BTreeMap::new(),
            pitch_bands: BTreeMap::new(),
            viz_enabled: false,
            viz_inst: None,
            stem_capture: false,
            source_capture: false,
            retiring: Vec::new(),
            live_gain_target: BTreeMap::new(),
            live_gain_cur: BTreeMap::new(),
            metering: false,
            note_tap: std::collections::BTreeSet::new(),
            note_log: false,
        }
    }

    /// Report each loop's note-source structure in `RenderResult.note_log` (ADR 0048): where the
    /// passes of every source feeding a mirrored instrument start. Observation only.
    pub fn set_note_log(&mut self, on: bool) {
        self.note_log = on;
    }

    /// Mirror these instruments' notes as MIDI (ADR 0048): each render reports their held voices
    /// as Note-On/Off in `RenderResult.note_tap`. Instruments that can't be mirrored
    /// ([`Instrument::can_tap_notes`](crate::loop_doc::Instrument::can_tap_notes)) or aren't in
    /// the doc are skipped. Observation only — `audio` is byte-identical.
    pub fn set_note_tap(&mut self, instruments: std::collections::BTreeSet<String>) {
        self.note_tap = instruments;
    }

    /// Per-instrument block peaks for a live meter ([`LoopRender::take_meters`]). Observation only.
    pub fn set_metering(&mut self, on: bool) {
        self.metering = on;
    }

    /// Live gain targets (mute/solo, ADR 0035), applied from the next loop (a running
    /// [`LoopRender`] takes them via [`LoopRender::set_gain_targets`]). Instruments not named ⇒ 1.
    pub fn set_live_gains(&mut self, targets: BTreeMap<String, f32>) {
        self.live_gain_target = targets;
    }

    /// The live gains as they stand (targets + where each ramp is) — for a standby engine that
    /// takes over mid-session ([`adopt_live_gains`](Self::adopt_live_gains)).
    pub(crate) fn live_gain_state(&self) -> (BTreeMap<String, f32>, BTreeMap<String, f32>) {
        (self.live_gain_target.clone(), self.live_gain_cur.clone())
    }

    /// Continue another engine's live gains (a structural doc swap's fresh engine): a muted voice
    /// stays muted instead of snapping back to unity.
    pub(crate) fn adopt_live_gains(&mut self, (target, cur): (BTreeMap<String, f32>, BTreeMap<String, f32>)) {
        self.live_gain_target = target;
        self.live_gain_cur = cur;
    }

    /// Enable the multichannel stem tap (ADR 0027): `render` fills `RenderResult.stems` for docs
    /// with `out_channel` routes. Observation-only — the master `audio` is byte-identical either way.
    pub fn set_stem_capture(&mut self, on: bool) {
        self.stem_capture = on;
    }

    /// Enable the per-source stem tap (ADR 0030): `render` fills `RenderResult.sources` with one
    /// buffer per audio instrument + internal bus. Observation-only — `audio` is byte-identical.
    pub fn set_source_capture(&mut self, on: bool) {
        self.source_capture = on;
    }

    /// Enable/disable the live-viz tap (ADR 0024). When off (default) renders are byte-identical and
    /// the `RenderResult.viz_*` buffers stay empty. The TUI turns it on for the instrument screen.
    pub fn set_viz(&mut self, on: bool) {
        self.viz_enabled = on;
    }

    /// Select which instrument's raw dry hops the viz tap captures (bandwidth control). `None` ⇒
    /// master + effective only. No effect unless [`set_viz`](Engine::set_viz) is on.
    pub fn set_viz_instrument(&mut self, id: Option<String>) {
        self.viz_inst = id;
    }

}

mod loop_render;
pub use loop_render::{EditKind, LoopRender};

#[cfg(test)]
mod tests;
