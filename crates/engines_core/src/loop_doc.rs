//! Loop document v0.7 — parse target (CE-R5.1).
//!
//! v0.7 turns `modulators`, `matrix`, and per-step `voicing` from arrays into
//! **id-keyed objects** (like `instruments`); `steps` and `pitch_walk.pool` stay arrays.
//! Iteration is by sorted key (deterministic; "declared order" = sorted-id order).
//! Canonical paths are full JSONPath (`instruments.poly.params.cutoff_hz`, `matrix.m_x.depth_oct`).
//!
//! Heterogeneous `params` (string `wave`/`source` + numeric knobs) stay as a [`DocValue`] for
//! runtime extensibility (CE-R4.1) — `serde_json::Value`'s shape, but a `-Infinity` survives
//! (ADR 0054 P3). The struct re-serializes cleanly (ids dropped, `None`/empty skipped) so it can
//! mirror the effective-state doc.

use serde::{Deserialize, Serialize};
use crate::doc_value::{to_doc_value, DocValue};
use crate::params::DepthUnit;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoopDoc {
    pub schema_version: String,
    #[serde(default)]
    pub loop_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<DocValue>,
    pub transport: Transport,
    pub harmony: Harmony,
    #[serde(default)]
    pub global: GlobalParams,
    #[serde(default)]
    pub instruments: BTreeMap<String, Instrument>,
    #[serde(default)]
    pub modulators: BTreeMap<String, Modulator>,
    #[serde(default)]
    pub matrix: BTreeMap<String, MatrixEntry>,
    /// Send-bus / effects framework (v0.7). Instruments route a `sends_db` level into each bus;
    /// a bus runs an ordered (sorted-id) chain of effects via a fundsp graph and returns its wet
    /// signal to the master mix at `return_gain_db`. A doc without buses plays dry (schema v0.9
    /// removed the reverb a v0.8 doc got from `global.reverb_mix`; `engines-migrate` writes it out).
    #[serde(default)]
    pub buses: BTreeMap<String, Bus>,
    /// Predefined melodic phrases (ADR 0028), id-keyed — the doc-local sequence pool. A note
    /// modulator plays them via its `phrase` arm; `lib:<name>` pool refs resolve to the built-in
    /// library instead (same shape — entries are copy-pasteable between the two).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sequence_pools: BTreeMap<String, PhraseDef>,
    /// Tier-2 **safe bounds** (v0.8): per-param `{min,max}` (native units, JSONPath-keyed) that the
    /// controller must never exceed. Overrides the canonical default table ([`safe_range_default`]);
    /// absent params fall back to it. See ADR 0010.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub safe_bounds: BTreeMap<String, ConstraintBand>,
    /// Authored per-loop step cap `|δ|` in normalized `0..1` (the same units the controller reasons
    /// in). Optional default the runtime `--max-change` overrides; absent ⇒ the controller's built-in
    /// `DEFAULT_MAX_CHANGE`. See ADR 0025.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_change: Option<f32>,
    /// Authored **cumulative** drift cap: the half-width, in normalized `0..1`, that drift may
    /// move a param from its authored value over the whole session (`max_change` bounds one loop;
    /// `max_drift` bounds the total). Absent ⇒ `DEFAULT_MAX_DRIFT` (0.15). An objectives `constraints`
    /// band or `override` on a param takes precedence over this. See ADR 0025.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_drift: Option<f32>,
    /// The piece's own control settings (ADR 0033) — what used to live only in a separate objectives
    /// doc, so a piece is self-contained: the timbre axis (`hold` | `drift` | `live_edit`), the Orrery
    /// movement config, and Tier-3 constraint bands (native units). A session with no explicit
    /// control falls back to these ([`crate::contract::SessionControl::from_doc`]); CLI flags and an
    /// `--objectives` file override them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimize: Option<crate::contract::OptimizeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub movement: Option<crate::contract::Movement>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub constraints: BTreeMap<String, ConstraintBand>,
    /// Song form (ADR 0029): a "loop of loops" — an ordered sequence of matrix configurations
    /// (blocks), each lasting a set number of loops; the song ends when the sequence completes.
    /// Consumed by the session's song driver (`song::SongDriver`), which hands the engine and
    /// controller plain per-block docs with this stripped — nothing below the session sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song: Option<crate::song::SongSpec>,
}

/// A `[min,max]` bound band (native units; either end optional). Shared by the loop doc's Tier-2
/// `safe_bounds` and the objectives doc's Tier-3 `constraints` (ADR 0010).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConstraintBand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bus {
    /// Backfilled from the map key; not serialized (the key is authoritative).
    #[serde(default, skip_serializing)]
    pub id: String,
    /// Bus kind (ADR 0026 phase D): absent ⇒ an internal **effects** bus (the only kind until
    /// now). `"external_return"` ⇒ a DAW-audio-return: the shell captures a loopback input
    /// device (e.g. BlackHole carrying the DAW that our MIDI drives) and monitor-mixes it into
    /// the output at `return_gain_db`. The core render itself never sees external audio (I/O-free,
    /// CE-R1.2) — this bus is *config* for the shell, carrying no effects.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Wet return level into the master mix in dB (`-Infinity`, the default, is silent; the
    /// engine applies it through [`crate::params::GAIN`]: [`Bus::return_amp`]).
    #[serde(default = "silence")]
    pub return_gain_db: f32,
    /// External-return only: shift applied when aligning the captured return stream to a
    /// loop's playback window for scoring (the MIDI→DAW→loopback round-trip latency, ms).
    /// Monitor mixing is unaffected (it's live pass-through). Static per session.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub latency_ms: f32,
    /// Ordered (sorted-id) effect chain.
    #[serde(default)]
    pub effects: BTreeMap<String, BusEffect>,
}

impl Bus {
    /// The wet return as the amplitude the engine multiplies by.
    pub fn return_amp(&self) -> f32 {
        crate::params::db_to_amp(self.return_gain_db)
    }
    /// The return in the engine's normalized gain space (what routes into it add to).
    pub fn return_norm(&self) -> f32 {
        crate::params::GAIN.inverse(self.return_gain_db)
    }
    /// DAW-audio-return bus (ADR 0026 phase D)?
    pub fn is_external_return(&self) -> bool {
        self.kind.as_deref() == Some("external_return")
    }

    /// The effect chain's build-time shape: `(effect id, effect type)` in chain (sorted-id) order.
    /// The engine builds a bus's DSP from this once and keeps it across loops (tails cross the
    /// seam); effect *params* and `return_gain_db` are re-applied in place every loop.
    pub fn effect_chain(&self) -> impl Iterator<Item = (&str, &str)> {
        self.effects.iter().map(|(id, fx)| (id.as_str(), fx.kind.as_str()))
    }

    /// Same bus `type` and same [`effect_chain`](Self::effect_chain) shape — i.e. an edit from
    /// `self` to `other` is param-only and the built bus (and its tail) can carry. A shape change
    /// needs a rebuild: the engine rebuilds the bus, the LiveEdit swap crossfades (ADR 0024).
    pub fn same_shape(&self, other: &Bus) -> bool {
        self.kind == other.kind && self.effect_chain().eq(other.effect_chain())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BusEffect {
    /// Backfilled from the map key; not serialized.
    #[serde(default, skip_serializing)]
    pub id: String,
    /// Effect discriminator (e.g. `"reverb"`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Heterogeneous knobs (all numeric, native units at the doc boundary; normalized internally).
    #[serde(default)]
    pub params: BTreeMap<String, DocValue>,
}
impl BusEffect {
    /// A param's number as the doc wrote it — the authoring tools' view. The engine reads
    /// [`norm`](Self::norm) instead.
    pub fn authored_num(&self, key: &str) -> Option<f32> {
        self.params.get(key).and_then(|v| v.as_f64()).map(|f| f as f32)
    }
    /// A param in the engine's normalized `0..1`, clamped (the ADR 0005 seam, through the param
    /// registry).
    pub fn norm(&self, leaf: &str) -> Option<f32> {
        self.authored_num(leaf).map(|v| param_norm(leaf, v))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transport {
    pub bpm: f32,
    pub master_steps: u32,
    #[serde(default = "default_step_unit")]
    pub step_unit: String,
    #[serde(default)]
    pub swing: f32,
    #[serde(default)]
    pub timing_jitter_ms: f32,
}
fn default_step_unit() -> String {
    "beat".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Harmony {
    pub root: String,
    pub mode: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GlobalParams {
    /// The master out's gain in dB (0 = unity; no limiter follows, ADR 0021).
    #[serde(default)]
    pub master_gain_db: f32,
    #[serde(default = "half")]
    pub stereo_width: f32,
}
impl Default for GlobalParams {
    fn default() -> Self {
        Self { master_gain_db: 0.0, stereo_width: 0.5 }
    }
}
impl GlobalParams {
    /// The master gain as the amplitude the engine multiplies by.
    pub fn master_amp(&self) -> f32 {
        crate::params::db_to_amp(self.master_gain_db)
    }
}
fn silence() -> f32 {
    f32::NEG_INFINITY
}
fn one() -> f32 {
    1.0
}
fn half() -> f32 {
    0.5
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instrument {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// `pitched` as authored (unset ⇒ `None`). Only an explicit `true` on a `filtered_noise`
    /// instrument does anything: it turns on the note's band ([`Instrument::pitched_noise`], ADR 0046).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitched: Option<bool>,
    /// Controller opt-out (CE-R8): when `true`, the controller never dithers this instrument —
    /// neither its numeric params (drift) nor its categorical levers (`wave`/`material`/…).
    /// Use it to pin a constant voice (e.g. a fixed "bass drone"). Default `false`. Matrix
    /// modulation still applies; this only excludes it from controller perturbation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dither_disable: bool,
    /// Synth type (ADR 0009): `"subtractive"` (default), `"dual"`, `"modal"`, `"midi"`
    /// (ADR 0026: translates notes/params to external MIDI instead of synthesizing audio), or
    /// `"binaural"` (ADR 0047: two subtractive voices hard L/R at root ∓ `spread_hz`/2).
    /// Absent ⇒ subtractive, so existing docs render unchanged. Type-specific params live
    /// alongside the base ones in `params`; the controller never dithers `type` (it changes
    /// per-voice state shape).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub note_engine: NoteEngine,
    #[serde(default)]
    pub params: BTreeMap<String, DocValue>,
    /// Send levels in dB into named buses, keyed by bus id (`-Infinity` = no send). Empty = dry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sends_db: BTreeMap<String, f32>,
    /// MIDI-note velocity (`1..=127`) for voices that don't author their own `velocity`
    /// (ADR 0026; only meaningful on `type: "midi"` instruments). Absent ⇒ 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_default: Option<u8>,
    /// External-MIDI routing (required when `type: "midi"`, ADR 0026): the channel this
    /// instrument's notes/CC go out on, and which of its params map to which CC numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<MidiRouting>,
    /// Multichannel stem routing (ADR 0027): first channel of the **stereo pair** on the output
    /// device this instrument's dry signal is sent to instead of the main 1/2 pair — 1-based and
    /// odd (`3` ⇒ channels 3/4), so a DAW taking the device can mix each instrument on its own
    /// track. Channels 1/2 always carry the rest of the mix (un-routed instruments + bus/reverb
    /// returns). Shell-degradable: if the opened output device has too few channels the stem
    /// folds back into 1/2. The internal scored/captured mix is unaffected (the stems are an
    /// *additional* observation-only output). Not valid on `type: "midi"` (no audio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_channel: Option<u16>,
}

/// Where a `type: "midi"` instrument's output goes (ADR 0026). The *musical* mapping lives
/// here in the doc; binding to an actual OS port/device is a shell concern (CLI flag), so
/// the core stays I/O-free.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MidiRouting {
    /// Human MIDI channel `1..=16` (wire messages use `channel - 1`).
    #[serde(default = "one_u8")]
    pub channel: u8,
    /// `param name → CC number (0..=127)`. Each mapped param's **effective** value (base +
    /// all matrix modulation, incl. free-running LFOs) is quantized to 7 bits and emitted
    /// on change. Param names are free-form (e.g. `macro1`) — they only need to exist in
    /// `params` (validated) and default to the `0..1` range.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cc: BTreeMap<String, u8>,
}
fn one_u8() -> u8 {
    1
}
fn yes() -> bool {
    true
}

impl Instrument {
    /// A subtractive `filtered_noise` instrument with an explicit `pitched: true` (ADR 0046): its
    /// source band goes on through a band at each voice's note. Opt-in — a noise instrument that
    /// leaves `pitched` out stays plain band noise, as every noise voice was before.
    pub fn pitched_noise(&self) -> bool {
        self.pitched == Some(true) && self.wave() == Some("filtered_noise") && self.synth_type() == "subtractive"
    }

    /// A param's number as the doc wrote it (v0.9: native under a unit-suffixed key, `0..1` for a
    /// unitless one) — the authoring tools' view. The engine reads [`norm`](Self::norm) or
    /// [`native`](Self::native).
    pub fn authored_num(&self, key: &str) -> Option<f32> {
        self.params.get(key).and_then(|v| v.as_f64()).map(|f| f as f32)
    }
    /// A param in the engine's normalized `0..1` (the ADR 0005 seam, through the param registry's
    /// curve; a gain's `-Infinity` stays `-inf`); a unitless param as written (not clamped, as the
    /// render always read it).
    pub fn norm(&self, leaf: &str) -> Option<f32> {
        self.authored_num(leaf).map(|v| crate::params::norm_of_authored(leaf, v))
    }
    /// A param's physical value — Hz, s, st, ×, dB, a count — what the DSP computes with (a
    /// unitless one with a DSP unit through its curve: `resonance` → Q).
    pub fn native(&self, leaf: &str) -> Option<f32> {
        self.authored_num(leaf).map(|v| crate::params::native_of_authored(leaf, v))
    }
    /// [`norm`](Self::norm), or the param's default ([`crate::params::DEFAULTS`]; 0 without one)
    /// when the doc leaves it out.
    pub fn norm_or_default(&self, leaf: &str) -> f32 {
        self.norm(leaf).or_else(|| crate::params::default_norm(leaf)).unwrap_or(0.0)
    }
    /// [`native`](Self::native), or the param's default when the doc leaves it out.
    pub fn native_or_default(&self, leaf: &str) -> f32 {
        self.native(leaf).or_else(|| crate::params::default_num(leaf).map(|v| crate::params::native_of_authored(leaf, v))).unwrap_or(0.0)
    }
    pub fn string(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(|v| v.as_str())
    }
    pub fn wave(&self) -> Option<&str> {
        self.string("wave").or_else(|| self.string("source"))
    }
    /// Synth type discriminator (ADR 0009); absent ⇒ `"subtractive"`.
    pub fn synth_type(&self) -> &str {
        self.kind.as_deref().unwrap_or("subtractive")
    }
    /// External-MIDI translator instrument (ADR 0026)?
    pub fn is_midi(&self) -> bool {
        self.synth_type() == "midi"
    }
    /// Can this instrument's notes be mirrored as MIDI (ADR 0048)? Any instrument that plays the
    /// notes it is sent — unpitched ones mirror them as triggers — except a `midi` translator
    /// (already MIDI) and a binaural pair pinned to `root_hz` (its notes only gate a fixed pitch).
    pub fn can_tap_notes(&self) -> bool {
        match self.synth_type() {
            "midi" => false,
            "binaural" => !self.params.contains_key("root_hz"),
            _ => true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NoteEngine {
    #[serde(default = "one_u32")]
    pub polyphony: u32,
    #[serde(default)]
    pub glide_ms: f32,
}
fn one_u32() -> u32 {
    1
}

/// The furthest a modulator's `orrery_note_min` / `orrery_note_max` may sit from the harmony root
/// (semitones, ±; ADR 0042) — a MIDI note's span either way, and a bound that keeps the octave fold
/// exact in `f32`.
pub const ORRERY_NOTE_LIMIT: f32 = 127.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Modulator {
    /// Backfilled from the map key; not serialized (the key is authoritative).
    #[serde(default, skip_serializing)]
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    /// Orrery opt-out (CE-R8 hybrid): when `true`, Orrery mode leaves this note modulator's
    /// notes/voices untouched — mark a constant "underdrone" voicing so it never morphs.
    /// Default `false`. Ignored by non-note modulators and outside Orrery mode.
    #[serde(default, skip_serializing_if = "is_false")]
    pub orrery_disable: bool,
    /// Orrery role binding (v0.8): `chord|bass|arp|oneshot|context`. When set, Orrery binds this
    /// modulator by role instead of by id (Phase 4 consumes it). `context` = read-only harmonic
    /// input fed to the theory model (like `orrery_disable`, but explicit). Parsed in Phase 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orrery_role: Option<String>,
    /// The lowest note the Orrery may write into this modulator (ADR 0042): a semitone offset from
    /// the harmony root, like a `voicing` offset. Absent ⇒ no floor. Ignored outside Orrery mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orrery_note_min: Option<f32>,
    /// The highest note the Orrery may write into this modulator (ADR 0042). Absent ⇒ no ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orrery_note_max: Option<f32>,
    #[serde(default = "default_sync")]
    pub sync: String,
    #[serde(default = "sixteen")]
    pub length: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_duration: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub humanize_ms: f32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_hz: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_value: Option<f32>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<Generator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch_walk: Option<PitchWalk>,
    /// The phrase arm (ADR 0028): the third exclusive note-source shape
    /// (`steps ⊕ generator ⊕ phrase`). Requires an explicit `step_duration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phrase: Option<PhraseRef>,
}
fn default_sync() -> String {
    "loop".to_string()
}
fn sixteen() -> u32 {
    16
}
fn is_zero(x: &f32) -> bool {
    *x == 0.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub step: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f32>,
    /// id-keyed voices (v0.7). Empty for control steps.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub voicing: BTreeMap<String, NoteVoice>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteVoice {
    /// Backfilled from the voicing map key; not serialized.
    #[serde(default, skip_serializing)]
    pub id: Option<String>,
    #[serde(default)]
    pub offset: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cents_offset: f32,
    #[serde(default = "one")]
    pub prob: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub release: bool,
    /// MIDI-note velocity `1..=127` (ADR 0026). Only consumed when the voice lands on a
    /// `type: "midi"` instrument; absent ⇒ the instrument's `velocity_default` (⇒ 100).
    /// Native MIDI units at the doc boundary, per the ADR 0005 param-range seam rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    /// Sub-step onset offset within the host step, in steps (`0.0 ≤ t < 1.0`, ADR 0028):
    /// `0.5` = the "and", `1/3 ≈ 0.333` = second triplet. Default `0` = on the beat.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub t: f32,
}
fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Generator {
    pub kind: String,
    #[serde(default)]
    pub pulses: u32,
    #[serde(default)]
    pub rotation: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PitchWalk {
    #[serde(default)]
    pub pool: Vec<f32>,
    #[serde(default = "up")]
    pub order: String,
}
fn up() -> String {
    "up".to_string()
}

/// A predefined melodic phrase (ADR 0028): pitch + rhythm + articulation, tiled over its host
/// modulator's step grid. Lives id-keyed in the doc's top-level `sequence_pools` or the built-in
/// library (same shape — a `lib:` entry pins by copy-pasting it into the doc verbatim); played
/// via a modulator's `phrase` arm.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhraseDef {
    /// How the phrase tracks the harmony: `"chord"` (under the Orrery, re-voice pitches by the
    /// melody-fit rules, rhythm preserved; under a plain render, snap every note to the doc's
    /// authored `harmony.mode` scale — `theory::phrase_fit::refit_static`, ADR 0028 amendment)
    /// | `"arp"` (like `"chord"`, but every note to a chord tone along the written line: ties follow
    /// its direction, no re-strike where it moves (a 9th/11th/13th tension when no new chord tone
    /// fits), nothing outside the modulator's `orrery_note_min..=max` if set, else its written range;
    /// never developed)
    /// | `"root"` (parallel transpose with the chord root; verbatim without one) | `"literal"`
    /// (always as written — the escape hatch for exact offsets).
    #[serde(default = "fit_chord")]
    pub fit: String,
    /// Free-form selection tags (`"busy"`, `"calm"`, `"sparse"`, `"lift"`, …): the seeded
    /// selector and the Orrery's energy gate filter/weight the pool by these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Tile length in HOST-modulator steps: the phrase repeats every `length` steps.
    pub length: u32,
    /// The notes, ascending by `t` (a lint warns otherwise; evaluation sorts).
    pub notes: Vec<PhraseNote>,
}
fn fit_chord() -> String {
    "chord".to_string()
}

/// One note of a [`PhraseDef`]. Voice semantics (ADR 0028): every phrase note owns the
/// deterministic voice id `p{k}` (`k` = index of its legato-chain head), so `dur` decides the
/// articulation family — **present = gated** (note-off at `t + dur`, re-strikes next tile),
/// **absent = holds** (same pitch next tile sustains; a changed pitch glides — the pad idiom).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhraseNote {
    /// Onset in host steps from the tile start (`0 ≤ t < length`; fractional = sub-step:
    /// `1.5` = the "and" of tile step 1, thirds give triplets).
    pub t: f32,
    /// Semitones from `harmony.root` (octave 4), like every note voice.
    #[serde(default)]
    pub offset: f32,
    /// Gate length in host steps (`0 < dur < length`): emits the paired note-off at `t + dur`.
    /// Absent ⇒ the note HOLDS across tiles (write a `dur` for arp/re-striking notes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dur: Option<f32>,
    /// Reuse the PREVIOUS note's voice → the engine glides (a slurred line) instead of
    /// re-striking. Ignored (lint) on a phrase's first note.
    #[serde(default, skip_serializing_if = "is_false")]
    pub legato: bool,
    /// Chance this note fires; a failed roll skips the strike AND its paired note-off.
    /// Rolled per tile, seeded by the tile ordinal (loop_id-free, so a tile straddling the
    /// loop seam makes the same choice on both sides).
    #[serde(default = "one")]
    pub prob: f32,
    /// MIDI Note-On velocity `1..=127` (ADR 0026; only consumed on a `type:"midi"` instrument).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    /// Fine detune in cents (dropped in plain-MIDI translation, like step voices).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cents_offset: f32,
}

/// A modulator's `phrase` arm (ADR 0028) — the third exclusive note-source shape
/// (`steps ⊕ generator ⊕ phrase`): play entries from `sequence_pools` / the built-in library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhraseRef {
    /// Pool entry ids this modulator draws from: doc-local `sequence_pools` keys and/or
    /// built-in `lib:<name>` entries. Unknown ids are a hard authoring error.
    pub pool: Vec<String>,
    /// Pin one entry (must be in `pool`). The Orrery rewrites this per phrase window; a
    /// hand-authored doc may pin it too. Absent ⇒ `select` picks per tile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Per-tile selection when `active` is absent: `"seeded"` (deterministic hash draw) |
    /// `"cycle"` (round-robin). Seeded by the tile ordinal, not `loop_id` (seam-straddle safe).
    #[serde(default = "select_seeded")]
    pub select: String,
    /// Restrict selection to entries carrying ALL of these tags (empty = the whole pool).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Orrery-materialized refit copy of the active entry — what actually plays when present
    /// (wins over `active`). Machine-written (the doc stays the deterministic ground truth);
    /// hand-authoring it lints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realized: Option<Box<PhraseDef>>,
}
fn select_seeded() -> String {
    "seeded".to_string()
}

/// A route. Its depth is written in the dest's own unit under that unit's key (`depth_oct` into a
/// cutoff, `depth_db` into a gain, plain `depth` into a `0..1` param — ADR 0054 P4) and kept
/// that way; the engine adds it as a normalized distance ([`MatrixEntry::depth_norm`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "MatrixEntryDoc", into = "MatrixEntryDoc")]
pub struct MatrixEntry {
    pub id: String,
    pub source: String,
    pub dest: String,
    pub kind: String,
    /// The depth as written, in [`depth_unit`](Self::depth_unit).
    pub depth: f32,
    /// The key it's written under (the check requires the dest's: [`depth_unit_for`]).
    pub depth_unit: DepthUnit,
    pub mode: String,
    pub depth_slew_ms: f32,
    pub voices: u32,
    pub priority: i32,
}

impl MatrixEntry {
    /// The depth as a distance in the dest's normalized `0..1` (what a route adds to the base):
    /// the written depth over its dest's span (6.64 oct for a cutoff, 66 dB for a gain, 1 for a
    /// `0..1` param).
    pub fn depth_norm(&self) -> f32 {
        self.depth / depth_span_of(&self.dest)
    }
    /// Set the depth from a normalized distance, written back in its unit.
    pub fn set_depth_norm(&mut self, n: f32) {
        self.depth = n * depth_span_of(&self.dest);
    }
    /// The route's depth key (`depth_oct`, …) — its JSONPath's last segment.
    pub fn depth_key(&self) -> &'static str {
        self.depth_unit.key()
    }
}

/// The depth unit a route into `dest` takes (ADR 0054 P4): its param's, plain for a `0..1` dest.
pub fn depth_unit_for(dest: &str) -> DepthUnit {
    crate::params::spec_for_path(dest).map_or(DepthUnit::Plain, |s| s.depth_unit())
}

/// The native span one normalized unit of depth covers on `dest` (1 for a plain depth).
pub fn depth_span_of(dest: &str) -> f32 {
    match crate::params::spec_for_path(dest) {
        Some(s) if s.depth_unit() != DepthUnit::Plain => s.curve.map_or(1.0, |c| c.depth_span()),
        _ => 1.0,
    }
}

/// [`MatrixEntry`] as a doc writes it: one of the depth keys.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct MatrixEntryDoc {
    #[serde(default, skip_serializing)]
    id: String,
    source: String,
    dest: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth_oct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth_st: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth_cents: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth_s: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth_ms: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth_db: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth_hz: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth_x: Option<f32>,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    depth_slew_ms: f32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    voices: u32,
    #[serde(default = "one_i32")]
    priority: i32,
}

impl MatrixEntryDoc {
    fn slot(&mut self, u: DepthUnit) -> &mut Option<f32> {
        match u {
            DepthUnit::Plain => &mut self.depth,
            DepthUnit::Oct => &mut self.depth_oct,
            DepthUnit::St => &mut self.depth_st,
            DepthUnit::Cents => &mut self.depth_cents,
            DepthUnit::S => &mut self.depth_s,
            DepthUnit::Ms => &mut self.depth_ms,
            DepthUnit::Db => &mut self.depth_db,
            DepthUnit::Hz => &mut self.depth_hz,
            DepthUnit::X => &mut self.depth_x,
        }
    }
}

impl From<MatrixEntryDoc> for MatrixEntry {
    /// The first depth key present (the check refuses a route with two); none ⇒ 0 in the dest's
    /// unit.
    fn from(mut d: MatrixEntryDoc) -> Self {
        let written = DepthUnit::ALL.into_iter().find_map(|u| d.slot(u).map(|v| (u, v)));
        let (depth_unit, depth) = written.unwrap_or((depth_unit_for(&d.dest), 0.0));
        MatrixEntry { id: d.id, source: d.source, dest: d.dest, kind: d.kind, depth, depth_unit, mode: d.mode, depth_slew_ms: d.depth_slew_ms, voices: d.voices, priority: d.priority }
    }
}

impl From<MatrixEntry> for MatrixEntryDoc {
    /// A zero depth is left out, as it always was.
    fn from(e: MatrixEntry) -> Self {
        let mut d = MatrixEntryDoc {
            id: e.id,
            source: e.source,
            dest: e.dest,
            kind: e.kind,
            depth: None,
            depth_oct: None,
            depth_st: None,
            depth_cents: None,
            depth_s: None,
            depth_ms: None,
            depth_db: None,
            depth_hz: None,
            depth_x: None,
            mode: e.mode,
            depth_slew_ms: e.depth_slew_ms,
            voices: e.voices,
            priority: e.priority,
        };
        if e.depth != 0.0 {
            *d.slot(e.depth_unit) = Some(e.depth);
        }
        d
    }
}
fn default_mode() -> String {
    "add".to_string()
}
fn one_i32() -> i32 {
    1
}
fn is_zero_u32(x: &u32) -> bool {
    *x == 0
}

impl LoopDoc {
    /// Backfill `id` fields from object keys so downstream code can read `.id` directly.
    fn backfill_ids(&mut self) {
        for (k, m) in self.modulators.iter_mut() {
            m.id = k.clone();
            for step in &mut m.steps {
                for (vk, nv) in step.voicing.iter_mut() {
                    nv.id = Some(vk.clone());
                }
            }
        }
        for (k, e) in self.matrix.iter_mut() {
            e.id = k.clone();
        }
        for (k, b) in self.buses.iter_mut() {
            b.id = k.clone();
            for (ek, e) in b.effects.iter_mut() {
                e.id = ek.clone();
            }
        }
    }

    /// Hard authoring errors (ADR 0026), checked at the **authoring boundary** ([`from_json_str`]
    /// via [`crate::check`]) — the first one, as a message. Internal round-trips ([`from_value`],
    /// perturbation copies) skip it since an already-valid doc stays valid. A `song` (ADR 0029) is
    /// validated block by block (each materialized block re-enters here with its song stripped).
    pub(crate) fn validate(&self) -> Result<(), String> {
        let mut diags = Vec::new();
        self.validate_all(&mut diags);
        match diags.into_iter().find(|d| d.is_error()) {
            Some(d) => Err(d.to_string()),
            None => Ok(()),
        }
    }

    /// Every hard structural error (ADR 0026 / 0033), each located by its JSONPath. The messages
    /// name their subject (`instrument 'x': …`), so they render bare.
    pub fn validate_all(&self, out: &mut Vec<crate::check::DocDiag>) {
        let mut err = |path: String, msg: String| out.push(crate::check::DocDiag::error(&path, "invalid", msg).standalone());
        for (name, inst) in &self.instruments {
            let base = format!("instruments.{name}");
            if inst.is_midi() {
                match &inst.routing {
                    None => err(base.clone(), format!("instrument '{name}': type \"midi\" requires a `routing` section")),
                    Some(routing) => {
                        if !(1..=16).contains(&routing.channel) {
                            err(format!("{base}.routing.channel"), format!("instrument '{name}': routing.channel {} out of 1..=16", routing.channel));
                        }
                        for (param, &cc) in &routing.cc {
                            if cc > 127 {
                                err(format!("{base}.routing.cc.{param}"), format!("instrument '{name}': routing.cc.{param} = {cc} out of 0..=127"));
                            }
                            if !inst.params.contains_key(param) {
                                err(
                                    format!("{base}.routing.cc.{param}"),
                                    format!("instrument '{name}': routing.cc maps '{param}', which is not in its params (typo?)"),
                                );
                            }
                        }
                    }
                }
            } else if inst.routing.is_some() {
                err(format!("{base}.routing"), format!("instrument '{name}': `routing` is only valid on type \"midi\""));
            }
            // An unknown `type` would silently fall back to subtractive in `SynthKind::from_doc` —
            // reject it so a typo ("modall") is an error, not a mysteriously wrong-sounding synth.
            let t = inst.synth_type();
            if !matches!(t, "subtractive" | "dual" | "modal" | "midi" | "binaural") {
                err(
                    format!("{base}.type"),
                    format!("instrument '{name}': unknown type \"{t}\" (known: subtractive, dual, modal, midi, binaural)"),
                );
            }
            // A binaural pair beats two pitches against each other — noise has no pitch to beat.
            if t == "binaural" && inst.wave() == Some("filtered_noise") {
                let key = if inst.string("wave").is_some() { "wave" } else { "source" };
                err(
                    format!("{base}.params.{key}"),
                    format!("instrument '{name}': a binaural instrument needs a pitched wave — filtered_noise has no pitch to beat"),
                );
            }
            if let Some(v) = inst.velocity_default {
                if !(1..=127).contains(&v) {
                    err(format!("{base}.velocity_default"), format!("instrument '{name}': velocity_default {v} out of 1..=127"));
                }
            }
            if let Some(oc) = inst.out_channel {
                if inst.is_midi() {
                    err(
                        format!("{base}.out_channel"),
                        format!("instrument '{name}': out_channel is only valid on audio instruments (type \"midi\" produces no audio)"),
                    );
                } else if oc % 2 == 0 || !(3..=63).contains(&oc) {
                    err(
                        format!("{base}.out_channel"),
                        format!("instrument '{name}': out_channel {oc} must be the odd first channel of a stereo pair, 3..=63 (1/2 is the main mix pair)"),
                    );
                }
            }
        }
        for m in self.modulators.values() {
            let base = format!("modulators.{}", m.id);
            // `sync` is a schema enum ("loop" | "free") but the engine only ever tests `== "free"`,
            // so a typo ("looop") would silently loop-sync.
            if !matches!(m.sync.as_str(), "loop" | "free") {
                err(format!("{base}.sync"), format!("modulator '{}': unknown sync \"{}\" (known: loop, free)", m.id, m.sync));
            }
            for (key, v) in [("orrery_note_min", m.orrery_note_min), ("orrery_note_max", m.orrery_note_max)] {
                if let Some(v) = v.filter(|v| !(-ORRERY_NOTE_LIMIT..=ORRERY_NOTE_LIMIT).contains(v)) {
                    err(
                        format!("{base}.{key}"),
                        format!("modulator '{}': {key} {v} is outside ±{ORRERY_NOTE_LIMIT} semitones", m.id),
                    );
                }
            }
            if let (Some(lo), Some(hi)) = (m.orrery_note_min, m.orrery_note_max) {
                if lo > hi {
                    err(
                        format!("{base}.orrery_note_min"),
                        format!("modulator '{}': orrery_note_min {lo} is above orrery_note_max {hi}", m.id),
                    );
                }
            }
            if let Some(sd) = &m.step_duration {
                // `step_duration_frac` would otherwise fall back to 1/length (or a zero-length grid)
                // for "1/0" / "0/8" / garbage — a silent re-grid, not an authoring error.
                // Whole numbers only, as the schema says (the engine would take "1.5/8" as a float).
                let whole = |x: &str| !x.trim().is_empty() && x.trim().bytes().all(|b| b.is_ascii_digit());
                if crate::modulator::parse_step_duration(sd).is_none() || !sd.split_once('/').is_some_and(|(n, d)| whole(n) && whole(d)) {
                    err(
                        format!("{base}.step_duration"),
                        format!("modulator '{}': step_duration \"{sd}\" is not a positive fraction of the loop (\"n/d\" with whole numbers n, d > 0)", m.id),
                    );
                }
            }
            for (si, step) in m.steps.iter().enumerate() {
                for (vk, nv) in &step.voicing {
                    let vpath = format!("{base}.steps[{si}].voicing.{vk}");
                    if let Some(v) = nv.velocity {
                        if !(1..=127).contains(&v) {
                            err(format!("{vpath}.velocity"), format!("modulator '{}' step {} voice '{vk}': velocity {v} out of 1..=127", m.id, step.step));
                        }
                    }
                    if !(0.0..1.0).contains(&nv.t) {
                        err(
                            format!("{vpath}.t"),
                            format!(
                                "modulator '{}' step {} voice '{vk}': t {} out of 0.0..1.0 (fraction of the host step; 1.0 belongs to the next step)",
                                m.id, step.step, nv.t
                            ),
                        );
                    }
                }
            }
            if let Some(ph) = &m.phrase {
                let ppath = format!("{base}.phrase");
                if m.kind != "step_seq" {
                    // An lfo (or unknown kind) takes its own evaluation branch first and would
                    // silently emit no phrase notes at all.
                    err(ppath.clone(), format!("modulator '{}': `phrase` requires type \"step_seq\" (found \"{}\")", m.id, m.kind));
                }
                // The third exclusive note-source shape (ADR 0028): steps ⊕ generator ⊕ phrase.
                for (present, other) in [(!m.steps.is_empty(), "steps"), (m.generator.is_some(), "generator"), (m.pitch_walk.is_some(), "pitch_walk")] {
                    if present {
                        err(ppath.clone(), format!("modulator '{}': `phrase` is exclusive with `{other}` (steps ⊕ generator ⊕ phrase)", m.id));
                    }
                }
                if m.output != "note" {
                    err(ppath.clone(), format!("modulator '{}': `phrase` is a note-source shape (output must be \"note\")", m.id));
                }
                if m.step_duration.is_none() {
                    err(
                        format!("{base}.step_duration"),
                        format!(
                            "modulator '{}': `phrase` requires an explicit `step_duration` — the tile is the entry's `length` host steps, so a 1/length fallback would re-grid per selection",
                            m.id
                        ),
                    );
                }
                if ph.pool.is_empty() {
                    err(format!("{ppath}.pool"), format!("modulator '{}': `phrase.pool` is empty", m.id));
                }
                for (pi, pid) in ph.pool.iter().enumerate() {
                    if crate::phrase::resolve(pid, self).is_none() {
                        // A typo'd id is a hard authoring error, not a silent no-op.
                        err(
                            format!("{ppath}.pool[{pi}]"),
                            format!(
                                "modulator '{}': phrase pool entry \"{pid}\" not found in sequence_pools{}",
                                m.id,
                                if pid.starts_with("lib:") { " or the built-in library" } else { " (built-ins need the lib: prefix)" }
                            ),
                        );
                    }
                }
                if let Some(a) = &ph.active {
                    if !ph.pool.contains(a) {
                        err(format!("{ppath}.active"), format!("modulator '{}': phrase.active \"{a}\" is not in its pool", m.id));
                    }
                }
                if !matches!(ph.select.as_str(), "seeded" | "cycle") {
                    err(format!("{ppath}.select"), format!("modulator '{}': unknown phrase.select \"{}\" (seeded|cycle)", m.id, ph.select));
                }
                if !ph.tags.is_empty()
                    && !ph.pool.iter().any(|pid| crate::phrase::resolve(pid, self).is_some_and(|d| ph.tags.iter().all(|t| d.tags.contains(t))))
                {
                    err(format!("{ppath}.tags"), format!("modulator '{}': phrase.tags {:?} match no pool entry — selection would be silent", m.id, ph.tags));
                }
                if let Some(r) = &ph.realized {
                    // An EMPTY realized is legal machine-written silence (the Orrery's narrative drop).
                    if !r.notes.is_empty() {
                        if let Err(e) = crate::phrase::validate_phrase(&format!("{}(realized)", m.id), r) {
                            err(format!("{ppath}.realized"), e);
                        }
                    }
                }
            }
        }
        for (id, def) in &self.sequence_pools {
            let path = format!("sequence_pools.{id}");
            if id.starts_with("lib:") {
                err(path.clone(), format!("sequence_pools id \"{id}\": the lib: prefix is reserved for built-in library refs"));
            }
            if let Err(e) = crate::phrase::validate_phrase(id, def) {
                err(path, e);
            }
        }
        let returns: Vec<&str> = self.buses.iter().filter(|(_, b)| b.is_external_return()).map(|(id, _)| id.as_str()).collect();
        if returns.len() > 1 {
            err("buses".into(), format!("at most one external_return bus is supported (found {returns:?})"));
        }
        for (id, b) in &self.buses {
            let base = format!("buses.{id}");
            if b.is_external_return() {
                if !b.effects.is_empty() {
                    err(format!("{base}.effects"), format!("bus '{id}': an external_return bus carries no effects (the DAW owns its processing)"));
                }
                if !(0.0..=500.0).contains(&b.latency_ms) {
                    err(format!("{base}.latency_ms"), format!("bus '{id}': latency_ms {} out of 0..=500", b.latency_ms));
                }
            } else if b.latency_ms != 0.0 {
                err(format!("{base}.latency_ms"), format!("bus '{id}': latency_ms is only valid on type \"external_return\""));
            }
        }
        if let Some(song) = &self.song {
            match crate::song::validate(self, song) {
                Err(e) => err("song".into(), e),
                Ok(()) => out.extend(crate::song::check_controls(self, song)),
            }
        }
    }

    /// Detach the song section (ADR 0029) — the session driver owns it; the docs it hands the
    /// controller/engine never carry one.
    pub fn take_song(&mut self) -> Option<crate::song::SongSpec> {
        self.song.take()
    }

    /// Soft authoring warnings (never fatal): the binaries/TUI surface these to the author.
    /// Currently: two midi instruments sharing one channel
    /// (their note-offs can collide on the external device — ref-counting is per-instrument,
    /// not per-channel), and a `sync: "loop"` note pattern whose step grid spans more than
    /// one master loop. (An unknown instrument `type` or modulator `sync` is a hard `validate()`
    /// error, not a lint.)
    pub fn lint(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut by_channel: BTreeMap<u8, Vec<&str>> = BTreeMap::new();
        for (name, inst) in &self.instruments {
            if let (true, Some(r)) = (inst.is_midi(), &inst.routing) {
                by_channel.entry(r.channel).or_default().push(name);
            }
        }
        for (ch, insts) in by_channel {
            if insts.len() > 1 {
                warnings.push(format!(
                    "midi instruments {insts:?} share channel {ch}: overlapping note numbers can cut each other off"
                ));
            }
        }
        // A `sync: "loop"` note pattern whose step grid spans >1 master loop silently truncates:
        // the sequence restarts at step 0 every loop, so steps past the first loop's window NEVER
        // fire (a euclid pulse or authored step landing only there = a silent track). Multi-loop
        // patterns are what `sync: "free"` is for — its phase carries across loops, so every step
        // eventually plays (the polymetric idiom the bundled fixtures use).
        for m in self.modulators.values() {
            if m.kind == "step_seq" && m.output == "note" && m.sync == "loop" {
                let span = crate::modulator::step_duration_frac(m) * m.length.max(1) as f32;
                if m.phrase.is_none() && span > 1.001 {
                    warnings.push(format!(
                        "modulator '{}': sync \"loop\" with step_duration {} × length {} spans {span:.2} loops — steps beyond the first loop never fire; use sync \"free\" for a multi-loop pattern (or shorten step_duration/length)",
                        m.id,
                        m.step_duration.as_deref().unwrap_or("1/length"),
                        m.length,
                    ));
                }
                // A phrase tile longer than the loop under sync "loop": the tile restarts at
                // step 0 every loop while its overflow is CARRIED (pending_notes), so the tail
                // plays on top of the next loop's restarted head — a pile-up, not silence.
                if let Some(ph) = &m.phrase {
                    let step_frac = crate::modulator::step_duration_frac(m);
                    for pid in &ph.pool {
                        if let Some(def) = crate::phrase::resolve(pid, self) {
                            let span = step_frac * def.length as f32;
                            if span > 1.001 {
                                warnings.push(format!(
                                    "modulator '{}': sync \"loop\" phrase entry \"{pid}\" (length {}) spans {span:.2} loops — the tile restarts every loop while its carried tail overlaps the new head; use sync \"free\" or a shorter entry",
                                    m.id, def.length,
                                ));
                            }
                        }
                    }
                }
            }
            if let Some(ph) = &m.phrase {
                if ph.realized.is_some() {
                    warnings.push(format!(
                        "modulator '{}': phrase.realized is machine-written (the Orrery's refit copy) — hand-authored content belongs in sequence_pools",
                        m.id
                    ));
                }
            }
        }
        // Per-entry phrase style warnings (doc-local pools only; the built-in library is
        // validated at embed time and kept idiomatic by its own test).
        for (id, def) in &self.sequence_pools {
            if def.notes.windows(2).any(|w| w[1].t < w[0].t) {
                warnings.push(format!("phrase '{id}': notes are not ascending by t (evaluation sorts, but author in order)"));
            }
            if def.notes.first().is_some_and(|n| n.legato) {
                warnings.push(format!("phrase '{id}': legato on the first note has no predecessor to slur from (treated as a plain strike)"));
            }
            for w in def.notes.windows(2) {
                if let (Some(d), true) = (w[0].dur, w[1].legato) {
                    if w[0].t + d <= w[1].t {
                        warnings.push(format!(
                            "phrase '{id}': a legato note at t {} starts after its predecessor's gate ends (t {} + dur {d}) — it re-strikes instead of gliding",
                            w[1].t, w[0].t,
                        ));
                    }
                }
            }
        }
        if let Some(song) = &self.song {
            warnings.extend(crate::song::lint(self, song));
        }
        warnings
    }

    /// Parse a loop doc from **JSON5** (comments + trailing commas welcome, so docs can carry inline
    /// help/examples). JSON5 is a strict superset of JSON, so plain `.json` files parse unchanged
    /// (ADR 0023). The error is a message string (every caller formats it via Display/Debug).
    /// Every check of [`crate::check::check_str`] runs: an unknown key, an unparseable root, a
    /// dangling route… is an error here (ADR 0033), the first of which is the message. Use
    /// `check_str` directly for the full located list (warnings included).
    pub fn from_json_str(s: &str) -> Result<Self, String> {
        let checked = crate::check::check_str(s);
        if let Some(e) = checked.errors().next() {
            return Err(e.to_string());
        }
        checked.doc.ok_or_else(|| "doc did not parse".to_string())
    }

    /// Deserialize a parsed tree (any `Into<DocValue>`, so a `serde_json::json!` doc works too) and
    /// backfill ids and the default bus. Unchecked: [`Self::from_json_str`] is the checked path.
    pub fn from_value(v: impl Into<DocValue>) -> Result<Self, crate::doc_value::Error> {
        let mut doc: LoopDoc = crate::doc_value::from_doc_value(v.into())?;
        doc.backfill_ids();
        Ok(doc)
    }

    /// Numeric value at a dotted JSONPath (object path only).
    pub fn get_param(&self, path: &str) -> Option<f32> {
        let v = to_doc_value(self).ok()?;
        let mut cur = &v;
        for p in path.split('.') {
            cur = cur.get(p)?;
        }
        cur.as_f64().map(|f| f as f32)
    }

    /// Return a copy with the numeric leaf at `path` set to `value` (CE-R8.3 perturbation).
    pub fn with_param(&self, path: &str, value: f32) -> LoopDoc {
        let mut v = to_doc_value(self).unwrap_or_default();
        set_json_path(&mut v, path, value);
        LoopDoc::from_value(v).unwrap_or_else(|_| self.clone())
    }

    /// Raw JSON value at a dotted JSONPath (any type — used by categorical dither for
    /// non-numeric leaves like `enabled`/`wave`/`order`).
    pub fn get_value(&self, path: &str) -> Option<DocValue> {
        let v = to_doc_value(self).ok()?;
        let mut cur = &v;
        for p in path.split('.') {
            cur = cur.get(p)?;
        }
        Some(cur.clone())
    }

    /// Return a copy with the leaf at `path` set to an arbitrary JSON `value` (categorical
    /// dither). Falls back to `self` if the result no longer deserializes (e.g. a bad type).
    pub fn with_value(&self, path: &str, value: DocValue) -> LoopDoc {
        let mut v = to_doc_value(self).unwrap_or_default();
        set_json_path_value(&mut v, path, value);
        LoopDoc::from_value(v).unwrap_or_else(|_| self.clone())
    }

    /// `get_param` expressed in normalized `0..1` space (`cutoff_hz` through its curve, a route's
    /// `depth_oct` as a distance — [`norm_of`](Self::norm_of)). The controller reasons in this
    /// space; the doc stores native units.
    pub fn get_param_norm(&self, path: &str) -> Option<f32> {
        self.get_param(path).map(|raw| self.norm_of(path, raw))
    }

    /// `with_param` taking a normalized value (written back in native units).
    pub fn with_param_norm(&self, path: &str, n: f32) -> LoopDoc {
        self.with_param(path, self.denorm_of(path, n))
    }

    pub fn with_loop_id(mut self, loop_id: u64) -> LoopDoc {
        self.loop_id = loop_id;
        self
    }

    pub fn find_modulator(&self, id: &str) -> Option<&Modulator> {
        self.modulators.get(id)
    }
    pub fn find_matrix(&self, id: &str) -> Option<&MatrixEntry> {
        self.matrix.get(id)
    }

    pub fn total_frames(&self, sample_rate: f64) -> usize {
        let beat_sec = 60.0 / self.transport.bpm as f64;
        (self.transport.master_steps as f64 * beat_sec * sample_rate).round() as usize
    }
}

/// A matrix `dest` JSONPath, resolved (CE-R3.8 canonical addressing, v0.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dest {
    /// `instruments.<inst>.params.<param>`
    InstParam { inst: String, param: String },
    /// `global.<param>`
    Global { param: String },
    /// `instruments.<inst>.notes`
    Notes { inst: String },
    /// `buses.<bus>.effects.<fx>.params.<param>`
    BusParam { bus: String, fx: String, param: String },
    /// `buses.<bus>.return_gain_db`
    BusReturn { bus: String },
    Unknown,
}

// The named range constants live in the param registry now; re-exported here so existing
// importers (the engine's modulated-shift denorm, effects' predelay cap) keep their paths.
pub use crate::params::{OSC_B_RATIO_RANGE, PREDELAY_RANGE, SHIFT_RANGE};

/// Native `(min, max)` for a controller-changeable param, keyed by its JSONPath leaf (or a send's
/// path) — **derived from [`crate::params::PARAMS`]** (the single source of truth; add new params
/// there). Default `(0.0, 1.0)`. The controller and the modulation pipeline both reason in
/// normalized `0..1` and convert at the doc boundary via [`param_norm`]/[`param_denorm`]. A gain
/// may also be `-Infinity`, below its range.
pub fn param_range(path: &str) -> (f32, f32) {
    crate::params::spec_for_path(path).map(|s| s.doc_range()).unwrap_or((0.0, 1.0))
}

/// Native value → normalized `0..1` (clamped), through the param's curve; a gain's `-Infinity`
/// stays `-inf`. A unitless or unregistered param is clamped to `0..1`.
pub fn param_norm(path: &str, raw: f32) -> f32 {
    match crate::params::spec_for_path(path) {
        Some(crate::params::ParamSpec { authored: crate::params::Authored::Native, curve: Some(c), .. }) => c.inverse(raw),
        _ => raw.clamp(0.0, 1.0),
    }
}

/// Normalized `0..1` → native value (the input clamped to `0..1`; `-inf` stays silence).
pub fn param_denorm(path: &str, n: f32) -> f32 {
    let n = if n == f32::NEG_INFINITY { n } else { n.clamp(0.0, 1.0) };
    match crate::params::spec_for_path(path) {
        Some(crate::params::ParamSpec { authored: crate::params::Authored::Native, curve: Some(c), .. }) => c.eval(n),
        _ => n,
    }
}

/// Tier-2 **safe range** (native units) for a steerable param — the hard `[min,max]` the controller
/// must never exceed (ADR 0010). Default = the native [`param_range`], except a few safety overrides
/// (e.g. `master_gain_db` never reaches silence — the controller once drove it to 0, and every gain
/// stops at 0 dB, ADR 0054 P1). A loop doc's `safe_bounds` overrides per-param; this canonical
/// default table is mirrored in `contracts/loop_contract_v0.9.schema.json`
/// (`x-safe-bounds-defaults`) and parity-tested.
pub fn safe_range_default(path: &str) -> (f32, f32) {
    // Derived from the param registry (single source of truth; schema-parity-tested below).
    crate::params::spec_for_path(path).and_then(|s| s.safe_default).unwrap_or_else(|| param_range(path))
}

/// The route a depth path names (`matrix.<id>.depth_oct`, any depth key): its id.
pub fn depth_path_route(path: &str) -> Option<&str> {
    let (id, key) = path.strip_prefix("matrix.")?.rsplit_once('.')?;
    DepthUnit::from_key(key).map(|_| id)
}

impl LoopDoc {
    /// Tier-2 safe range for `path` (native units): the doc's `safe_bounds` override if present,
    /// else the canonical [`safe_range_default`] (a route depth's default: ± its dest's span).
    /// Each side merges independently, and an explicit side wins over a default side that would
    /// invert against it (`{max: -10}` on `master_gain_db`, default floor −8 ⇒ `[-10, -10]`), so
    /// a one-sided band never yields `min > max`.
    pub fn safe_range(&self, path: &str) -> (f32, f32) {
        let (dlo, dhi) = match self.route_of_depth(path) {
            Some(e) => {
                let span = depth_span_of(&e.dest);
                (-span, span)
            }
            None => safe_range_default(path),
        };
        let band = self.safe_bounds.get(path);
        let (min, max) = (band.and_then(|b| b.min), band.and_then(|b| b.max));
        match (min, max) {
            (Some(lo), Some(hi)) => (lo, hi),
            (Some(lo), None) => (lo, dhi.max(lo)),
            (None, Some(hi)) => (dlo.min(hi), hi),
            (None, None) => (dlo, dhi),
        }
    }

    /// Tier-2 safe range in normalized `0..1` space (what the controller reasons in).
    pub fn safe_range_norm(&self, path: &str) -> (f32, f32) {
        let (lo, hi) = self.safe_range(path);
        (self.norm_of(path, lo), self.norm_of(path, hi))
    }

    /// The route a depth path (`matrix.<id>.depth_oct`) names, if the doc has it.
    pub fn route_of_depth(&self, path: &str) -> Option<&MatrixEntry> {
        depth_path_route(path).and_then(|id| self.matrix.get(id))
    }

    /// A native value at `path` → the controller's normalized space: [`param_norm`], or for a
    /// route depth its signed distance (`depth_oct / 6.64` into a cutoff).
    pub fn norm_of(&self, path: &str, native: f32) -> f32 {
        match self.route_of_depth(path) {
            Some(e) => native / depth_span_of(&e.dest),
            None => param_norm(path, native),
        }
    }

    /// The inverse of [`norm_of`](Self::norm_of).
    pub fn denorm_of(&self, path: &str, n: f32) -> f32 {
        match self.route_of_depth(path) {
            Some(e) => n * depth_span_of(&e.dest),
            None => param_denorm(path, n),
        }
    }
}

/// Set a numeric leaf at a dotted JSONPath within a doc tree (object path only — no array
/// indices; used for modulated dests and dither perturbations).
pub fn set_json_path(root: &mut DocValue, path: &str, num: f32) {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return;
    }
    let mut cur = root;
    for p in &parts[..parts.len() - 1] {
        cur = match cur.get_mut(*p) {
            Some(c) => c,
            None => return,
        };
    }
    if let DocValue::Object(m) = cur {
        m.insert(parts[parts.len() - 1].to_string(), DocValue::from(num));
    }
}

/// Set an arbitrary leaf at a dotted JSONPath within a doc tree (object path only). The generic
/// sibling of [`set_json_path`], used for categorical dither moves.
pub fn set_json_path_value(root: &mut DocValue, path: &str, value: DocValue) {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return;
    }
    let mut cur = root;
    for p in &parts[..parts.len() - 1] {
        cur = match cur.get_mut(*p) {
            Some(c) => c,
            None => return,
        };
    }
    if let DocValue::Object(m) = cur {
        m.insert(parts[parts.len() - 1].to_string(), value);
    }
}

pub fn resolve_dest(dest: &str) -> Dest {
    if let Some(rest) = dest.strip_prefix("instruments.") {
        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() >= 3 && parts[1] == "params" {
            return Dest::InstParam { inst: parts[0].to_string(), param: parts[2].to_string() };
        }
        if parts.len() == 2 && parts[1] == "notes" {
            return Dest::Notes { inst: parts[0].to_string() };
        }
    } else if let Some(param) = dest.strip_prefix("global.") {
        return Dest::Global { param: param.to_string() };
    } else if let Some(rest) = dest.strip_prefix("buses.") {
        let parts: Vec<&str> = rest.split('.').collect();
        // buses.<bus>.effects.<fx>.params.<param>
        if parts.len() == 5 && parts[1] == "effects" && parts[3] == "params" {
            return Dest::BusParam {
                bus: parts[0].to_string(),
                fx: parts[2].to_string(),
                param: parts[4].to_string(),
            };
        }
        // buses.<bus>.return_gain_db
        if parts.len() == 2 && parts[1] == "return_gain_db" {
            return Dest::BusReturn { bus: parts[0].to_string() };
        }
    }
    Dest::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) use crate::test_support::fixture;

    #[test]
    fn from_json_str_accepts_json5_comments_and_trailing_commas() {
        // ADR 0023: loop docs may carry // and /* */ comments + trailing commas (so they can hold
        // inline help/examples) and still parse, since JSON5 is a superset of JSON.
        let src = r#"{
            // a tiny json5 loop doc
            schema_version: "0.9",
            transport: { bpm: 120, master_steps: 8, step_unit: "beat", }, /* trailing comma ok */
            harmony: { root: "A", mode: "minor" },
            instruments: {
                poly: {
                    enabled: true, pitched: true,
                    note_engine: { polyphony: 1 },
                    params: { wave: "saw_soft", level_db: -6.02, cutoff_hz: 1267.9 }, // unquoted keys ok
                },
            },
        }"#;
        let d = LoopDoc::from_json_str(src).expect("json5 loop doc parses");
        assert_eq!(d.schema_version, "0.9");
        assert_eq!(d.instruments.len(), 1);
        assert!(d.instruments.contains_key("poly"));
    }

    #[test]
    fn loop_template_json5_parses_as_a_valid_doc() {
        // The shipped annotated loop template (doc/loop_contract_v0.9.json5) must remain a valid,
        // engine-parseable doc (ADR 0023) — guards the template against drift.
        let d = LoopDoc::from_json_str(crate::contract::LOOP_TEMPLATE_JSON5)
            .expect("annotated loop template parses via the engine");
        assert!(!d.instruments.is_empty(), "template has instruments");
    }

    #[test]
    fn parses_v07_objects() {
        let d = fixture();
        assert_eq!(d.schema_version, "0.9");
        assert_eq!(d.instruments.len(), 4);
        assert_eq!(d.modulators.len(), 8); // incl. the phrase-armed `glints` (ADR 0028)
        assert_eq!(d.matrix.len(), 8);
        // ids backfilled from keys
        assert_eq!(d.modulators["arp_shimmer"].id, "arp_shimmer");
        assert_eq!(d.matrix["m_breath_c"].id, "m_breath_c");
    }

    #[test]
    fn voicing_is_id_keyed_with_backfilled_ids() {
        let d = fixture();
        let chords = &d.modulators["chords"];
        let step0 = chords.steps.iter().find(|s| s.step == 0).unwrap();
        assert_eq!(step0.voicing.len(), 4);
        assert_eq!(step0.voicing["v3"].id.as_deref(), Some("v3"));
        assert_eq!(step0.voicing["v3"].cents_offset, -5.0);
    }

    #[test]
    fn resolves_bus_dests() {
        assert_eq!(
            resolve_dest("buses.rv.effects.verb.params.room"),
            Dest::BusParam { bus: "rv".into(), fx: "verb".into(), param: "room".into() }
        );
        assert_eq!(resolve_dest("buses.rv.return_gain_db"), Dest::BusReturn { bus: "rv".into() });
        assert_eq!(resolve_dest("buses.rv.return_gain"), Dest::Unknown, "v0.8's key is gone");
    }

    /// Schema v0.9 dropped `global.reverb_mix` and the reverb it implied: a doc without buses plays
    /// dry (`engines-migrate` writes a v0.8 doc's implied bus out).
    #[test]
    fn a_doc_without_buses_plays_dry() {
        let d = LoopDoc::from_json_str(r#"{ schema_version: "0.9", transport: { bpm: 60, master_steps: 4 }, harmony: { root: "C", mode: "ionian" },
            instruments: { pad: { params: { level_db: -6 } } } }"#).unwrap();
        assert!(d.buses.is_empty() && d.instruments["pad"].sends_db.is_empty());
        let e = LoopDoc::from_json_str(r#"{ schema_version: "0.9", transport: { bpm: 60, master_steps: 4 }, harmony: { root: "C", mode: "ionian" }, global: { reverb_mix: 0.3 } }"#).unwrap_err();
        assert!(e.contains("reverb_mix"), "{e}");
    }

    #[test]
    fn predelay_is_millisecond_ranged() {
        let p = "buses.rv.effects.verb.params.predelay_ms";
        assert_eq!(param_range(p), PREDELAY_RANGE);
        // 0.5 normalized → 50 ms native, round-trips.
        assert!((param_denorm(p, 0.5) - 50.0).abs() < 1e-4);
        assert!((param_norm(p, 50.0) - 0.5).abs() < 1e-4);
    }

    // v0.8 (ADR 0010): safe_bounds + orrery_role parse; safe_range override + default fallback.
    #[test]
    fn parses_v08_safe_bounds_and_orrery_role() {
        let doc = LoopDoc::from_json_str(
            r#"{
                "schema_version": "0.9",
                "transport": { "bpm": 60, "master_steps": 16 },
                "harmony": { "root": "C", "mode": "ionian" },
                "safe_bounds": { "global.master_gain_db": { "min": -6.02, "max": -0.92 } },
                "modulators": { "m": { "type": "step_seq", "output": "note", "orrery_role": "context" } }
            }"#,
        )
        .unwrap();
        assert_eq!(doc.safe_range("global.master_gain_db"), (-6.02, -0.92), "authored safe_bounds override");
        assert_eq!(doc.modulators["m"].orrery_role.as_deref(), Some("context"));
        // an un-overridden param falls back to the canonical default table (native, ADR 0054)
        assert_eq!(doc.safe_range("instruments.x.params.cutoff_hz"), (101.0, 8000.0));
    }

    // Parity (ADR 0010 D5): the engine's safe_range_default table == the v0.9 schema's canonical
    // `x-safe-bounds-defaults`, so the two can never silently diverge.
    #[test]
    fn safe_bounds_defaults_match_schema_table() {
        let schema_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contracts/loop_contract_v0.9.schema.json");
        let schema: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(schema_path).unwrap()).unwrap();
        let table = schema["x-safe-bounds-defaults"].as_object().expect("x-safe-bounds-defaults present");
        assert!(!table.is_empty());
        for (leaf, band) in table {
            // safe_range_default matches on the path leaf (a send on its own path).
            let path = if leaf == "sends_db" { "instruments.x.sends_db.rv".to_string() } else { format!("instruments.x.params.{leaf}") };
            let (lo, hi) = safe_range_default(&path);
            let smin = band["min"].as_f64().unwrap() as f32;
            let smax = band["max"].as_f64().unwrap() as f32;
            assert!((lo - smin).abs() < 1e-6 && (hi - smax).abs() < 1e-6, "{leaf}: code ({lo},{hi}) != schema ({smin},{smax})");
        }
    }

    #[test]
    fn param_norm_denorm_roundtrip() {
        let p = "instruments.poly.params.shift_st";
        // shift_st: -24 → 0, 0 → 0.5, 24 → 1
        assert!((param_norm(p, -24.0) - 0.0).abs() < 1e-6);
        assert!((param_norm(p, 0.0) - 0.5).abs() < 1e-6);
        assert!((param_norm(p, 24.0) - 1.0).abs() < 1e-6);
        // round-trips
        for raw in [-24.0, -12.0, 0.0, 7.0, 24.0] {
            assert!((param_denorm(p, param_norm(p, raw)) - raw).abs() < 1e-4);
        }
        // a log curve normalizes through it (800 Hz is half way across 80..8000); a unitless param is
        // its own value; shift_mix's leaf keeps the (0,1) range (no prefix collision)
        assert_eq!(param_range("instruments.poly.params.cutoff_hz"), (80.0, 8000.0));
        assert!((param_norm("instruments.poly.params.cutoff_hz", 800.0) - 0.5).abs() < 1e-6);
        assert!((param_denorm("instruments.poly.params.cutoff_hz", 0.5) - 800.0).abs() < 1e-3);
        assert_eq!(param_range("instruments.poly.params.shift_mix"), (0.0, 1.0));
        assert!((param_norm("instruments.poly.params.resonance", 0.42) - 0.42).abs() < 1e-6);
        // a gain's silence stays silence both ways
        assert_eq!(param_norm("global.master_gain_db", f32::NEG_INFINITY), f32::NEG_INFINITY);
        assert_eq!(param_denorm("global.master_gain_db", f32::NEG_INFINITY), f32::NEG_INFINITY);
    }

    #[test]
    fn dest_is_jsonpath() {
        let d = fixture();
        assert_eq!(d.matrix["m_breath_c"].dest, "instruments.poly.params.cutoff_hz");
        assert_eq!(resolve_dest(&d.matrix["m_breath_c"].dest), Dest::InstParam { inst: "poly".into(), param: "cutoff_hz".into() });
        assert_eq!(resolve_dest(&d.matrix["m_breath_w"].dest), Dest::Global { param: "stereo_width".into() });
        assert_eq!(resolve_dest(&d.matrix["m_arp"].dest), Dest::Notes { inst: "poly".into() });
    }

    #[test]
    fn reserializes_without_ids_or_nulls() {
        let d = fixture();
        let v = serde_json::to_value(&d).unwrap();
        // modulators is an object; entries have no inner "id"; clean of nulls
        let arp = &v["modulators"]["arp_shimmer"];
        assert!(arp.get("id").is_none());
        assert_eq!(arp["generator"]["pulses"], 17);
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains(":null"), "mirror should not contain nulls");
    }

    #[test]
    fn total_frames_matches() {
        let d = fixture();
        let expected = (16.0 * 60.0 / 65.0 * 48_000.0_f64).round() as usize;
        assert_eq!(d.total_frames(48_000.0), expected);
    }

    // ── MIDI translator instruments (ADR 0026) ───────────────────────────────

    fn midi_doc_src(routing: &str) -> String {
        format!(
            r#"{{
            "schema_version": "0.9",
            "transport": {{ "bpm": 60, "master_steps": 16 }},
            "harmony": {{ "root": "C", "mode": "ionian" }},
            "instruments": {{ "ext": {{ "type": "midi", "params": {{ "cutoff": 0.5 }}{routing} }} }}
        }}"#
        )
    }

    #[test]
    fn midi_instrument_parses_with_routing_and_velocity_fields() {
        let d = LoopDoc::from_json_str(&midi_doc_src(
            r#", "velocity_default": 90, "routing": { "channel": 3, "cc": { "cutoff": 74 } }"#,
        ))
        .unwrap();
        let ext = &d.instruments["ext"];
        assert!(ext.is_midi());
        assert_eq!(ext.velocity_default, Some(90));
        let r = ext.routing.as_ref().unwrap();
        assert_eq!((r.channel, r.cc["cutoff"]), (3, 74));
        assert!(ext.sends_db.is_empty());
    }

    #[test]
    fn midi_validation_rejects_bad_authoring() {
        // type "midi" without a routing section
        let e = LoopDoc::from_json_str(&midi_doc_src("")).unwrap_err();
        assert!(e.contains("requires a `routing`"), "{e}");
        // cc mapping a param that isn't in params (typo guard)
        let e = LoopDoc::from_json_str(&midi_doc_src(r#", "routing": { "channel": 1, "cc": { "cutof": 74 } }"#))
            .unwrap_err();
        assert!(e.contains("cutof"), "{e}");
        // channel out of the human 1..=16 range
        let e = LoopDoc::from_json_str(&midi_doc_src(r#", "routing": { "channel": 17, "cc": {} }"#)).unwrap_err();
        assert!(e.contains("channel"), "{e}");
        // velocity_default out of 1..=127
        let e = LoopDoc::from_json_str(&midi_doc_src(
            r#", "velocity_default": 200, "routing": { "channel": 1, "cc": {} }"#,
        ))
        .unwrap_err();
        assert!(e.contains("velocity_default"), "{e}");
        // routing on a non-midi instrument
        let e = LoopDoc::from_json_str(
            r#"{ "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 16 },
                 "harmony": { "root": "C", "mode": "ionian" },
                 "instruments": { "poly": { "params": {}, "routing": { "channel": 1 } } } }"#,
        )
        .unwrap_err();
        assert!(e.contains("only valid on type \"midi\""), "{e}");
        // voicing velocity out of 1..=127
        let e = LoopDoc::from_json_str(
            r#"{ "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 16 },
                 "harmony": { "root": "C", "mode": "ionian" },
                 "modulators": { "seq": { "type": "step_seq", "output": "note",
                     "steps": [{ "step": 0, "voicing": { "v1": { "offset": 0, "velocity": 0 } } }] } } }"#,
        )
        .unwrap_err();
        assert!(e.contains("velocity 0"), "{e}");
    }

    #[test]
    fn substep_voice_t_validates_range_and_serde_skips_zero() {
        // ADR 0028: per-voice sub-step onset `t` — in-range parses and round-trips;
        // the 0 default is serde-skipped so legacy docs re-serialize byte-identically.
        let doc = |t: &str| {
            format!(
                r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 16 }},
                     "harmony": {{ "root": "C", "mode": "ionian" }},
                     "modulators": {{ "seq": {{ "type": "step_seq", "output": "note",
                         "steps": [{{ "step": 0, "voicing": {{ "v1": {{ "offset": 0, "t": {t} }} }} }}] }} }} }}"#
            )
        };
        let d = LoopDoc::from_json_str(&doc("0.5")).unwrap();
        assert_eq!(d.modulators["seq"].steps[0].voicing["v1"].t, 0.5);
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["modulators"]["seq"]["steps"][0]["voicing"]["v1"]["t"], 0.5);

        let v0 = serde_json::to_value(&LoopDoc::from_json_str(&doc("0.0")).unwrap()).unwrap();
        assert!(
            v0["modulators"]["seq"]["steps"][0]["voicing"]["v1"].get("t").is_none(),
            "t: 0 is the default and never serialized"
        );

        // 1.0 belongs to the next step; negatives are nonsense.
        assert!(LoopDoc::from_json_str(&doc("1.0")).unwrap_err().contains("t 1"));
        assert!(LoopDoc::from_json_str(&doc("-0.1")).unwrap_err().contains("out of 0.0..1.0"));
    }

    #[test]
    fn the_orrery_note_range_is_bounded_and_ordered() {
        // ADR 0042: within ±127 semitones of the root, and min ≤ max.
        let doc = |range: &str| {
            format!(
                r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 16 }},
                     "harmony": {{ "root": "C", "mode": "ionian" }},
                     "modulators": {{ "seq": {{ "type": "step_seq", "output": "note", {range},
                         "steps": [{{ "step": 0, "voicing": {{ "v1": {{ "offset": 0 }} }} }}] }} }} }}"#
            )
        };
        let d = LoopDoc::from_json_str(&doc(r#""orrery_note_min": -127, "orrery_note_max": 127"#)).unwrap();
        assert_eq!((d.modulators["seq"].orrery_note_min, d.modulators["seq"].orrery_note_max), (Some(-127.0), Some(127.0)));
        assert!(LoopDoc::from_json_str(&doc(r#""orrery_note_min": 1e9"#)).is_err(), "a far-off bound is refused");
        assert!(LoopDoc::from_json_str(&doc(r#""orrery_note_max": -128"#)).is_err());
        assert!(LoopDoc::from_json_str(&doc(r#""orrery_note_min": 12, "orrery_note_max": 0"#)).unwrap_err().contains("above orrery_note_max"));
    }

    // ── ADR 0028: sequence pools + the phrase arm ─────────────────────────────

    /// Minimal doc with one phrase-armed modulator + one pool entry; `mod_extra` splices
    /// into the modulator, `pool_extra` replaces the default pool entry body.
    fn phrase_doc(mod_extra: &str, pool: &str) -> String {
        format!(
            r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 16 }},
                 "harmony": {{ "root": "C", "mode": "ionian" }},
                 "modulators": {{ "seq": {{ "type": "step_seq", "output": "note",
                     "length": 8, "step_duration": "1/8"{mod_extra},
                     "phrase": {{ "pool": ["cell"] }} }} }},
                 "sequence_pools": {{ "cell": {pool} }} }}"#
        )
    }
    const CELL: &str = r#"{ "length": 4, "notes": [{ "t": 0, "offset": 0, "dur": 0.5 }] }"#;

    #[test]
    fn phrase_arm_parses_and_validates() {
        let d = LoopDoc::from_json_str(&phrase_doc("", CELL)).unwrap();
        let ph = d.modulators["seq"].phrase.as_ref().unwrap();
        assert_eq!(ph.pool, ["cell"]);
        assert_eq!(ph.select, "seeded", "select defaults to seeded");
        assert_eq!(d.sequence_pools["cell"].fit, "chord", "fit defaults to chord");
        // built-in library refs validate too
        let src = phrase_doc("", CELL).replace(r#""pool": ["cell"]"#, r#""pool": ["lib:glass_up"]"#);
        assert!(LoopDoc::from_json_str(&src).is_ok(), "lib: refs resolve against the embedded library");
    }

    #[test]
    fn phrase_validation_rejects_bad_shapes() {
        // XOR with the other note-source arms
        let e = LoopDoc::from_json_str(&phrase_doc(r#", "generator": { "kind": "euclid", "pulses": 3 }"#, CELL)).unwrap_err();
        assert!(e.contains("exclusive with `generator`"), "{e}");
        let e = LoopDoc::from_json_str(&phrase_doc(r#", "steps": [{ "step": 0, "voicing": { "v": { "offset": 0 } } }]"#, CELL))
            .unwrap_err();
        assert!(e.contains("exclusive with `steps`"), "{e}");
        // an explicit step_duration is required
        let e = LoopDoc::from_json_str(&phrase_doc("", CELL).replace(r#""step_duration": "1/8","#, "")).unwrap_err();
        assert!(e.contains("requires an explicit `step_duration`"), "{e}");
        // unknown ids are hard errors — doc-local and lib:
        let e = LoopDoc::from_json_str(&phrase_doc("", CELL).replace(r#"["cell"]"#, r#"["nope"]"#)).unwrap_err();
        assert!(e.contains("\"nope\" not found"), "{e}");
        let e = LoopDoc::from_json_str(&phrase_doc("", CELL).replace(r#"["cell"]"#, r#"["lib:nope"]"#)).unwrap_err();
        assert!(e.contains("built-in library"), "{e}");
        // active must be in the pool; select is a closed enum
        let e = LoopDoc::from_json_str(&phrase_doc("", CELL).replace(r#""pool": ["cell"]"#, r#""pool": ["cell"], "active": "other""#))
            .unwrap_err();
        assert!(e.contains("not in its pool"), "{e}");
        let e = LoopDoc::from_json_str(&phrase_doc("", CELL).replace(r#""pool": ["cell"]"#, r#""pool": ["cell"], "select": "shuffle""#))
            .unwrap_err();
        assert!(e.contains("unknown phrase.select"), "{e}");
        // tags that match no entry would select silence
        let e = LoopDoc::from_json_str(&phrase_doc("", CELL).replace(r#""pool": ["cell"]"#, r#""pool": ["cell"], "tags": ["zydeco"]"#))
            .unwrap_err();
        assert!(e.contains("match no pool entry"), "{e}");
        // entry shape: t within the tile, dur within (0, length), lib: ids reserved
        let e = LoopDoc::from_json_str(&phrase_doc("", r#"{ "length": 4, "notes": [{ "t": 4.0 }] }"#)).unwrap_err();
        assert!(e.contains("out of 0..length"), "{e}");
        // a gate must END inside its tile: t + dur ≤ length (== is fine; > is an error)
        let e = LoopDoc::from_json_str(&phrase_doc("", r#"{ "length": 4, "notes": [{ "t": 0, "dur": 4.5 }] }"#)).unwrap_err();
        assert!(e.contains("exceeds the tile"), "{e}");
        let e = LoopDoc::from_json_str(&phrase_doc("", r#"{ "length": 4, "notes": [{ "t": 3.5, "dur": 0.6 }] }"#)).unwrap_err();
        assert!(e.contains("exceeds the tile"), "{e}");
        assert!(LoopDoc::from_json_str(&phrase_doc("", r#"{ "length": 4, "notes": [{ "t": 3.5, "dur": 0.5 }] }"#)).is_ok(), "t + dur == length is allowed");
        let e = LoopDoc::from_json_str(&phrase_doc("", r#"{ "length": 4, "notes": [{ "t": 0, "dur": 0 }] }"#)).unwrap_err();
        assert!(e.contains("must be > 0"), "{e}");
        // an UNREFERENCED doc-local pool key may still not squat on the lib: prefix
        let e = LoopDoc::from_json_str(&phrase_doc("", CELL).replace(
            r#""sequence_pools": { "cell":"#,
            r#""sequence_pools": { "lib:x": { "length": 1, "notes": [{ "t": 0 }] }, "cell":"#,
        ));
        assert!(e.unwrap_err().contains("reserved"), "lib:-prefixed pool ids are reserved");
    }

    #[test]
    fn step_duration_must_be_a_positive_fraction_and_phrase_needs_step_seq() {
        // A malformed explicit step_duration used to fall back silently to 1/length (or a
        // zero grid) — now a hard authoring error, for every modulator kind.
        for bad in ["1/0", "0/8", "abc", "1/", "/4"] {
            let src = phrase_doc("", CELL).replace(r#""step_duration": "1/8""#, &format!(r#""step_duration": "{bad}""#));
            let e = LoopDoc::from_json_str(&src).unwrap_err();
            assert!(e.contains("not a positive fraction"), "{bad}: {e}");
        }
        // a plain step_seq (no phrase) is covered too
        let src = r#"{ "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 16 },
                       "harmony": { "root": "C", "mode": "ionian" },
                       "modulators": { "w": { "type": "step_seq", "output": "control", "length": 4, "step_duration": "1/0",
                                              "steps": [{ "step": 0, "value": 0.5 }] } } }"#;
        assert!(LoopDoc::from_json_str(src).unwrap_err().contains("not a positive fraction"));
        // a phrase arm on an lfo would take the lfo branch and silently emit nothing
        let src = phrase_doc(r#", "rate_hz": 1.0"#, CELL).replace(r#""type": "step_seq""#, r#""type": "lfo""#);
        let e = LoopDoc::from_json_str(&src).unwrap_err();
        assert!(e.contains("requires type \"step_seq\""), "{e}");
    }

    #[test]
    fn phrase_lints_warn_on_style_issues() {
        // legato on the first note + notes out of order + a slur that starts after its
        // predecessor's gate ends + a hand-authored realized copy
        let src = phrase_doc(
            "",
            r#"{ "length": 4, "notes": [
                { "t": 1, "offset": 0, "legato": true, "dur": 0.25 },
                { "t": 0.5, "offset": 2, "dur": 0.25 },
                { "t": 3, "offset": 4, "legato": true, "dur": 0.5 }
            ] }"#,
        )
        .replace(
            r#""phrase": { "pool": ["cell"] }"#,
            r#""phrase": { "pool": ["cell"], "realized": { "length": 1, "notes": [{ "t": 0 }] } }"#,
        );
        let d = LoopDoc::from_json_str(&src).unwrap();
        let lints = d.lint().join("\n");
        assert!(lints.contains("not ascending by t"), "{lints}");
        assert!(lints.contains("legato on the first note"), "{lints}");
        assert!(lints.contains("re-strikes instead of gliding"), "{lints}");
        assert!(lints.contains("machine-written"), "{lints}");
    }

    #[test]
    fn external_return_bus_parses_and_validates() {
        // ADR 0026 phase D: the DAW-return bus is session/shell config — parse + guardrails.
        let base = |bus: &str| {
            format!(
                r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 16 }},
                     "harmony": {{ "root": "C", "mode": "ionian" }},
                     "buses": {{ {bus} }} }}"#
            )
        };
        let d = LoopDoc::from_json_str(&base(
            r#""daw": { "type": "external_return", "return_gain_db": -1.94, "latency_ms": 120 }"#,
        ))
        .unwrap();
        let daw = &d.buses["daw"];
        assert!(daw.is_external_return());
        assert_eq!((daw.return_gain_db, daw.latency_ms), (-1.94, 120.0));
        assert!(!d.buses.contains_key("rv"));

        // two external_return buses
        let e = LoopDoc::from_json_str(&base(
            r#""a": { "type": "external_return" }, "b": { "type": "external_return" }"#,
        ))
        .unwrap_err();
        assert!(e.contains("at most one"), "{e}");
        // effects on an external_return bus
        let e = LoopDoc::from_json_str(&base(
            r#""daw": { "type": "external_return", "effects": { "v": { "type": "reverb" } } }"#,
        ))
        .unwrap_err();
        assert!(e.contains("no effects"), "{e}");
        // latency out of range / on the wrong bus kind
        let e = LoopDoc::from_json_str(&base(r#""daw": { "type": "external_return", "latency_ms": 900 }"#)).unwrap_err();
        assert!(e.contains("latency_ms"), "{e}");
        let e = LoopDoc::from_json_str(&base(r#""fx": { "return_gain_db": -6.02, "latency_ms": 10 }"#)).unwrap_err();
        assert!(e.contains("only valid on type \"external_return\""), "{e}");
    }

    #[test]
    fn midi_lint_warns_on_shared_channel() {
        let d = LoopDoc::from_json_str(
            r#"{ "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 16 },
                 "harmony": { "root": "C", "mode": "ionian" },
                 "instruments": {
                    "a": { "type": "midi", "params": {}, "routing": { "channel": 2, "cc": {} } },
                    "b": { "type": "midi", "params": {}, "routing": { "channel": 2, "cc": {} } },
                    "c": { "type": "midi", "params": {}, "routing": { "channel": 3, "cc": {} } } } }"#,
        )
        .unwrap();
        let warnings = d.lint();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("channel 2"), "{warnings:?}");
    }

    #[test]
    fn out_channel_validation() {
        // ADR 0027: `out_channel` must be the odd first channel of a stereo pair (3..=63), and
        // only on audio instruments (a midi translator produces no audio to route).
        let base = |inst: &str| {
            format!(
                r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 16 }},
                     "harmony": {{ "root": "C", "mode": "ionian" }},
                     "instruments": {{ {inst} }} }}"#
            )
        };
        // odd pair start ⇒ ok
        assert!(LoopDoc::from_json_str(&base(r#""a": { "params": { "level_db": -6.02 }, "out_channel": 3 }"#)).is_ok());
        // even ⇒ rejected (crosses pair boundaries)
        let e = LoopDoc::from_json_str(&base(r#""a": { "params": { "level_db": -6.02 }, "out_channel": 4 }"#)).unwrap_err();
        assert!(e.contains("odd"), "{e}");
        // 1 would collide with the main mix pair ⇒ rejected
        let e = LoopDoc::from_json_str(&base(r#""a": { "params": { "level_db": -6.02 }, "out_channel": 1 }"#)).unwrap_err();
        assert!(e.contains("out_channel"), "{e}");
        // midi translators produce no audio ⇒ rejected
        let e = LoopDoc::from_json_str(&base(
            r#""m": { "type": "midi", "params": {}, "routing": { "channel": 1, "cc": {} }, "out_channel": 3 }"#,
        ))
        .unwrap_err();
        assert!(e.contains("audio instruments"), "{e}");
    }

    #[test]
    fn lint_warns_on_sync_loop_pattern_spanning_multiple_loops() {
        // A sync:"loop" note step_seq restarts at step 0 every loop, so a step grid spanning >1
        // master loop silently never fires its later steps (the field-reported "half my euclid
        // pattern is missing / track reads MUTE"). sync:"free" carries phase and is exempt (the
        // polymetric idiom the bundled fixtures use).
        let base = |sync: &str, step_dur: &str| {
            format!(
                r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 16 }},
                     "harmony": {{ "root": "C", "mode": "ionian" }},
                     "instruments": {{ "m": {{ "params": {{ "level_db": -6.02 }} }} }},
                     "modulators": {{ "arp": {{ "type": "step_seq", "output": "note", "sync": "{sync}",
                        "length": 16, "step_duration": "{step_dur}",
                        "generator": {{ "kind": "euclid", "pulses": 4, "rotation": 1 }} }} }},
                     "matrix": {{ "r": {{ "source": "arp", "dest": "instruments.m.notes", "kind": "note", "voices": 2, "priority": 1 }} }} }}"#
            )
        };
        // 1/8 × 16 = 2 loops under sync:"loop" → warn.
        let w = LoopDoc::from_json_str(&base("loop", "1/8")).unwrap().lint();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("spans 2.00 loops") && w[0].contains("sync \"free\""), "{w:?}");
        // Exactly one loop (1/16 × 16) → clean.
        assert!(LoopDoc::from_json_str(&base("loop", "1/16")).unwrap().lint().is_empty());
        // Multi-loop under sync:"free" is the intentional polymetric idiom → clean.
        assert!(LoopDoc::from_json_str(&base("free", "1/8")).unwrap().lint().is_empty());
        // A typo'd sync is a HARD validate() error (the engine would otherwise silently
        // loop-sync it — and this lint's "sync \"loop\"" wording would be a lie).
        let e = LoopDoc::from_json_str(&base("looop", "1/8")).unwrap_err();
        assert!(e.contains("unknown sync \"looop\""), "{e}");
    }

    #[test]
    fn another_schema_version_or_an_unknown_type_is_refused() {
        // Another schema version is refused (ADR 0054 D5: only `engines-migrate` reads v0.8); an
        // unknown instrument `type` is a HARD validate() error at the authoring boundary — the old
        // silent fall-back-to-subtractive turned a typo into a mysteriously wrong synth.
        let parse = |ver: &str, ty: &str| {
            LoopDoc::from_json_str(&format!(
                r#"{{ "schema_version": "{ver}", "transport": {{ "bpm": 60, "master_steps": 16 }},
                     "harmony": {{ "root": "C", "mode": "ionian" }},
                     "instruments": {{ "a": {{ {ty} "params": {{ "level_db": -6.02 }} }} }} }}"#
            ))
        };
        assert!(parse("0.9", "").unwrap().lint().is_empty());
        for old in ["0.8", "0.7"] {
            let e = parse(old, "").unwrap_err();
            assert!(e.contains(&format!("schema {old}")) && e.contains("engines-migrate"), "{e}");
        }
        let e = parse("0.9", r#""type": "modall","#).unwrap_err();
        assert!(e.contains("unknown type \"modall\""), "{e}");
        // known types parse + stay lint-clean (midi needs routing to validate, so dual here)
        assert!(parse("0.9", r#""type": "dual","#).unwrap().lint().is_empty());
        assert!(parse("0.9", r#""type": "binaural","#).unwrap().lint().is_empty());
    }

    #[test]
    fn binaural_noise_is_rejected() {
        // ADR 0047: the pair needs a pitch — `filtered_noise` (as `wave` or `source`) is an error.
        let parse = |params: &str| {
            LoopDoc::from_json_str(&format!(
                r#"{{ "schema_version": "0.9", "transport": {{ "bpm": 60, "master_steps": 16 }},
                     "harmony": {{ "root": "C", "mode": "ionian" }},
                     "instruments": {{ "b": {{ "type": "binaural", "params": {{ {params} }} }} }} }}"#
            ))
        };
        assert!(parse(r#""wave": "sine", "spread_hz": 4"#).is_ok());
        let e = parse(r#""source": "filtered_noise""#).unwrap_err();
        assert!(e.contains("needs a pitched wave"), "{e}");
        assert!(parse(r#""wave": "filtered_noise""#).unwrap_err().contains("needs a pitched wave"));
    }
}
