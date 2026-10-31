//! The **capability catalog** (ADR 0050 D6): what the engine can do, as data — the synth types and
//! the params each one reads; every param's kind, range, safe band, default, modulatability and unit;
//! the reverb, the global and bus params, the harmony modes and the modulators; and the **unit maps**
//! between the engine's normalized `0..1` (where drift steps and route depths add) and a doc's native
//! numbers (Hz, s, dB, …), plus the DSP readings of the unitless ones (Q, radians) — each with sample
//! points computed by the engine's own functions.
//!
//! `engines-check --catalog [--json]` prints it, so `engines-listen` and its mapping guide read the engine
//! instead of keeping copies that drift. Contract: `contracts/capability_catalog_v2.schema.json` (v2:
//! schema v0.9's native units, ADR 0054; its JSON is the doc writer's, so a silent gain's default
//! reads `-Infinity`).
//!
//! Everything is derived from the engine's registries — [`PARAMS`], [`crate::params::DEFAULTS`],
//! [`param_applies`], [`MODULATABLE_INST_PARAMS`], [`REVERB_PARAM_LEAVES`], the map functions — and
//! **proven by render**: an omitted param sounds bit-identical to its `default` written out (the
//! parity tests). Pure data, no I/O.

use crate::check::{param_applies, LFO_WAVES, MODES};
use crate::effects::REVERB_PARAM_LEAVES;
use crate::engine::{is_glideable_dest, MODULATABLE_INST_PARAMS};
use crate::loop_doc::{depth_span_of, LoopDoc};
use crate::params::units::{unit_label, unit_of, Unit};
pub use crate::params::{default_of, Dflt};
use crate::params::{spec_for, DepthUnit, ParamKind, MATERIAL_OPTIONS, PARAMS, WAVE_OPTIONS};
use crate::params::{ATTACK, BAND, CUTOFF, DECAY, DRIVE, FM_INDEX, GAIN, OSC_B_DETUNE, OSC_B_RATIO, PREDELAY, RELEASE, RESONANCE_Q, RING_RATIO, ROOT_HZ, SHIFT, SPREAD_HZ, T60, VOICES_DETUNE};
use crate::synth::{material_partials, T60_MAX, T60_MIN};
use crate::theory::scale::Mode;
use serde::Serialize;
use std::collections::BTreeMap;

/// The catalog's shape version — bumped when a consumer would have to change.
const CATALOG_VERSION: u32 = 2;

/// The synth types (ADR 0009, 0026, 0047); the first is what an instrument without `type` gets.
pub(crate) const SYNTH_TYPES: [&str; 5] = ["subtractive", "dual", "modal", "midi", "binaural"];

/// The modal `t60_mode`: the ring time is `t60_s` (`fixed`) or `ring_ratio` × the note's step (`note`).
const T60_MODES: [&str; 2] = ["fixed", "note"];

/// The `global.*` params (serde defaults in `loop_doc::GlobalParams`), all modulatable.
const GLOBAL_PARAMS: [&str; 2] = ["master_gain_db", "stereo_width"];
/// The per-bus param (`buses.<id>.return_gain_db`).
const BUS_PARAMS: [&str; 1] = ["return_gain_db"];

/// The Hz maps' sample rate: 48 kHz, whose Nyquist is above both maps' ceilings.
const SAMPLE_NYQUIST: f32 = 24_000.0;

/// The whole catalog.
#[derive(Clone, Debug, Serialize)]
pub struct Catalog {
    pub catalog_version: u32,
    /// The loop-doc schema version this engine reads.
    pub schema_version: &'static str,
    pub engine_version: &'static str,
    /// The engine build ([`crate::build_id`], ADR 0054 P10). Not part of the hash: a new commit that
    /// changes nothing the catalog describes keeps its cache key.
    pub engine_build: &'static str,
    /// FNV-1a 64 (hex) of the catalog's compact JSON with `hash` and `engine_build` empty — it changes
    /// iff the content does.
    pub hash: String,
    pub synth_types: BTreeMap<&'static str, SynthType>,
    /// Instrument params (`instruments.<id>.params.<leaf>`), by leaf.
    pub params: BTreeMap<&'static str, ParamInfo>,
    /// Bus effects by type; each holds its params (`buses.<bus>.effects.<fx>.params.<leaf>`).
    pub effects: BTreeMap<&'static str, Effect>,
    /// `global.<leaf>`.
    pub global: BTreeMap<&'static str, ParamInfo>,
    /// `buses.<id>.<leaf>`.
    pub buses: BTreeMap<&'static str, ParamInfo>,
    pub unit_maps: BTreeMap<&'static str, UnitMap>,
    pub harmony: Harmony,
    pub modulators: Modulators,
}

/// One synth type: the params it reads (per source variant) and what else a mapper needs.
#[derive(Clone, Debug, Serialize)]
pub struct SynthType {
    /// What an instrument without `type` gets.
    pub default: bool,
    /// `false` for `midi`: it sends notes and CCs to an external synth and renders nothing.
    pub audio: bool,
    /// `midi`: params are free-form value holders (CC sources via `routing.cc`), so every name applies.
    pub free_form: bool,
    /// The params it reads with a pitched oscillator (`wave`/`source` not `filtered_noise`).
    pub params: Vec<&'static str>,
    /// Where a noise source reads a different set: `filtered_noise` (`source: "filtered_noise"`) and
    /// `pitched_noise` (plus `pitched: true`, ADR 0046). Only the variants that differ are listed.
    pub variants: BTreeMap<&'static str, Vec<&'static str>>,
    /// `modal`: each `material`'s partials, the first `num_modes` of which ring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materials: Option<BTreeMap<&'static str, Vec<PartialInfo>>>,
    /// What the param lists can't say (pinned by the render tests).
    pub note: Option<&'static str>,
}

/// One modal partial: frequency `ratio` to the note, strike `amp`, ring time relative to the
/// fundamental's T60 (`decay_rel`).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct PartialInfo {
    pub ratio: f32,
    pub amp: f32,
    pub decay_rel: f32,
}

/// One param's capabilities.
#[derive(Clone, Debug, Serialize)]
pub struct ParamInfo {
    /// Its JSONPath, ids as `<id>` / `<bus>` / `<fx>`.
    pub path: String,
    /// How the drift controller treats it: `continuous` (drifted across `range`), `categorical` (flipped
    /// among `options`), `int_steps` (±1 steps), `fixed` (never moved by the controller), `enum` (a
    /// string the controller never touches).
    pub kind: &'static str,
    /// `number`, `integer` or `string`.
    pub value_type: &'static str,
    /// The authored range, in `unit` (`null` for strings).
    pub range: Option<[f32; 2]>,
    /// The Tier-2 safety band the controller stays inside (ADR 0011), when narrower than `range`.
    pub safe_default: Option<[f32; 2]>,
    /// What the engine reads when the param is omitted, in `unit` — where a route into an omitted
    /// param starts too (`null`: no single value — see the description). A silent gain's is
    /// `-Infinity`.
    pub default: Option<Dflt>,
    /// A matrix `mod` route into it moves it.
    pub modulatable: bool,
    /// The key a route into it writes its depth under (`depth_oct`, `depth_db`, … — ADR 0054 P4);
    /// `null` when it isn't modulatable.
    pub depth_key: Option<&'static str>,
    /// How much of `unit` (octaves for a log curve) one unit of normalized depth covers: the
    /// param's span. A depth of `depth_span` sweeps the whole range.
    pub depth_span: Option<f32>,
    /// A song seam glides it between blocks (else it steps, ADR 0029).
    pub glideable: bool,
    /// The values a string param takes.
    pub options: Option<Vec<&'static str>>,
    /// What an authored number means: `hz`, `s`, `ms`, `semitones`, `cents`, `ratio`, `db`,
    /// `normalized` (a unitless 0..1) or `count`.
    pub unit: Option<&'static str>,
    /// The unit the editor shows (`Hz`, `s`, `st`, `ms`, `¢`, `×`, `dB`, `0..1`).
    pub display_unit: Option<&'static str>,
    /// The unit maps (keys of `unit_maps`) that read this param.
    pub unit_maps: Vec<&'static str>,
    /// The loop schema's description.
    pub description: Option<&'static str>,
}

/// A bus effect type.
#[derive(Clone, Debug, Serialize)]
pub struct Effect {
    pub params: BTreeMap<&'static str, ParamInfo>,
}

/// A unit map: a param's number → a physical quantity, `x` in → `y` out.
///
/// - `exp`: y = lo · (hi/lo)^x, x in 0..1;
/// - `log`: y = ln(x/lo) / ln(hi/lo), clamped to 0..1 (the inverse of an `exp` map: x in `from` units);
/// - `linear`: y = hi · x (lo = 0), x in 0..1;
/// - `affine`: y = lo + (hi − lo) · x, x in 0..1.
#[derive(Clone, Debug, Serialize)]
pub struct UnitMap {
    pub kind: &'static str,
    pub from: &'static str,
    pub to: &'static str,
    pub lo: f32,
    pub hi: f32,
    /// The engine function or constant it comes from.
    pub source: &'static str,
    pub params: Vec<&'static str>,
    pub note: Option<&'static str>,
    /// `[x, y]` pairs computed by `source`.
    pub samples: Vec<[f32; 2]>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Harmony {
    /// The `harmony.root` spellings, with their semitone above C.
    pub roots: Vec<Root>,
    pub modes: Vec<ModeInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Root {
    pub name: String,
    pub semitone: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModeInfo {
    pub name: &'static str,
    /// Semitones above the root, degree 1 first.
    pub degrees: &'static [i32],
    /// Other spellings `harmony.mode` accepts for it.
    pub aliases: Vec<&'static str>,
}

/// A string choice: its values and the one an omitted key gets (`null`: no default — see the schema).
#[derive(Clone, Debug, Serialize)]
pub struct Choice {
    pub options: Vec<&'static str>,
    pub default: Option<&'static str>,
}

/// The modulators' and matrix routes' string choices.
#[derive(Clone, Debug, Serialize)]
pub struct Modulators {
    #[serde(rename = "type")]
    pub kind: Choice,
    pub output: Choice,
    pub sync: Choice,
    /// An LFO's `wave` (anything else plays as a sine).
    pub lfo_wave: Choice,
    pub lfo_polarity: Choice,
    /// A control `step_seq`'s `interp` (absent ⇒ stepped).
    pub interp: Choice,
    pub generator: Choice,
    pub pitch_walk_order: Choice,
    pub orrery_role: Choice,
    /// A matrix route's `kind`.
    pub route_kind: Choice,
}

/// A minimal doc for [`is_glideable_dest`] (it only looks into the doc for a midi instrument's CCs).
fn empty_doc() -> LoopDoc {
    LoopDoc::from_value(serde_json::json!({ "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 1 }, "harmony": { "root": "C", "mode": "ionian" } })).expect("a minimal doc parses")
}

/// The instrument params, in registry order (`source` after `wave`, `t60_mode` after `ring_ratio`).
pub(crate) fn instrument_leaves() -> Vec<&'static str> {
    let outside: Vec<&str> = GLOBAL_PARAMS.iter().chain(&BUS_PARAMS).chain(&REVERB_PARAM_LEAVES).copied().collect();
    let mut leaves = Vec::new();
    for s in PARAMS.iter().filter(|s| !outside.contains(&s.leaf)) {
        leaves.push(s.leaf);
        match s.leaf {
            "wave" => leaves.push("source"),
            "ring_ratio" => leaves.push("t60_mode"),
            _ => {}
        }
    }
    leaves
}

/// The options of a string param outside the registry: `source` is `wave`'s alias plus the noise.
fn string_options(leaf: &str) -> Option<Vec<&'static str>> {
    match leaf {
        "source" => Some(WAVE_OPTIONS.iter().copied().chain(["filtered_noise"]).collect()),
        "t60_mode" => Some(T60_MODES.to_vec()),
        _ => None,
    }
}

/// The loop schema's description of a property (`$defs.params`, `global`, the reverb's params, `bus`).
fn schema_description(section: &str, leaf: &str) -> Option<&'static str> {
    let s = crate::schema::schema();
    let props = match section {
        "params" => &s["$defs"]["params"]["properties"],
        "global" => &s["properties"]["global"]["properties"],
        "reverb" => &s["$defs"]["busEffect"]["properties"]["params"]["properties"],
        _ => &s["$defs"]["bus"]["properties"],
    };
    props.get(leaf).and_then(|p| p.get("description")).and_then(|d| d.as_str())
}

fn param_info(section: &str, path: String, leaf: &'static str, modulatable: bool, doc: &LoopDoc) -> ParamInfo {
    let spec = spec_for(leaf);
    let (kind, value_type, range, options) = match spec.map(|s| s.kind) {
        Some(ParamKind::Continuous) => ("continuous", "number", spec.map(|s| s.doc_range().into()), None),
        Some(ParamKind::Fixed) => ("fixed", "number", spec.map(|s| s.doc_range().into()), None),
        Some(ParamKind::IntSteps { lo, hi }) => ("int_steps", "integer", Some([lo as f32, hi as f32]), None),
        Some(ParamKind::Categorical(opts)) => ("categorical", "string", None, Some(opts.to_vec())),
        None => ("enum", "string", None, string_options(leaf)),
    };
    let unit = match (value_type, unit_of(&path)) {
        ("string", _) => None,
        ("integer", _) => Some("count"),
        (_, Unit::Normalized) => Some("normalized"),
        (_, Unit::Hz | Unit::HzNative) => Some("hz"),
        (_, Unit::Seconds) => Some("s"),
        (_, Unit::Semitones) => Some("semitones"),
        (_, Unit::Cents) => Some("cents"),
        (_, Unit::Millis) => Some("ms"),
        (_, Unit::Ratio) => Some("ratio"),
        (_, Unit::Db) => Some("db"),
    };
    let display_unit = (value_type == "number").then(|| unit_label(&path));
    let routable = modulatable && value_type == "number";
    ParamInfo {
        kind,
        value_type,
        range,
        safe_default: spec.and_then(|s| s.safe_default).map(|(lo, hi)| [lo, hi]),
        default: default_of(leaf),
        modulatable,
        depth_key: routable.then(|| spec.map_or(DepthUnit::Plain, |s| s.depth_unit()).key()),
        depth_span: routable.then(|| depth_span_of(&path)),
        glideable: is_glideable_dest(&path, doc),
        options,
        unit,
        display_unit,
        unit_maps: UNIT_MAPS.iter().filter(|m| m.params.contains(&leaf)).map(|m| m.id).collect(),
        description: schema_description(section, leaf),
        path,
    }
}

/// A unit map's static half; [`unit_map`] computes its bounds and samples from `f`.
struct MapDef {
    id: &'static str,
    kind: &'static str,
    from: &'static str,
    to: &'static str,
    source: &'static str,
    params: &'static [&'static str],
    note: Option<&'static str>,
    /// `(lo, hi)`: the curve's ends (the modal ring's clamp for `t60_s`).
    bounds: fn() -> (f32, f32),
    f: fn(f32) -> f32,
}

/// The `x`s a forward map is sampled at (0..1 in tenths) and a `log` map at (Hz, either side of its range).
const SAMPLE_X: [f32; 11] = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
const SAMPLE_HZ: [f32; 11] = [40.0, 80.0, 100.0, 200.0, 400.0, 800.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];

const UNIT_MAPS: &[MapDef] = &[
    MapDef { id: "cutoff_hz", kind: "exp", from: "normalized", to: "hz", source: "engine::cutoff_to_hz", params: &["cutoff_hz"], note: Some("the low-pass corner; capped at Nyquist (never reached at 44.1/48 kHz)"), bounds: || CUTOFF.ends(), f: |x| CUTOFF.eval(x).min(SAMPLE_NYQUIST) },
    MapDef { id: "hz_cutoff", kind: "log", from: "hz", to: "normalized", source: "engine::hz_to_cutoff", params: &["cutoff_hz"], note: Some("the inverse of cutoff_hz"), bounds: || CUTOFF.ends(), f: |hz| CUTOFF.inverse(hz) },
    MapDef { id: "band_hz", kind: "exp", from: "normalized", to: "hz", source: "engine::band_to_hz", params: &["band_lo_hz", "band_hi_hz"], note: Some("a filtered_noise band edge; capped at Nyquist"), bounds: || BAND.ends(), f: |x| BAND.eval(x).min(SAMPLE_NYQUIST) },
    MapDef { id: "hz_band", kind: "log", from: "hz", to: "normalized", source: "engine::hz_to_band", params: &["band_lo_hz", "band_hi_hz"], note: Some("the inverse of band_hz"), bounds: || BAND.ends(), f: |hz| BAND.inverse(hz) },
    MapDef { id: "attack_s", kind: "linear", from: "normalized", to: "s", source: "engine::ATTACK_MAX_S", params: &["attack_s"], note: None, bounds: || ATTACK.ends(), f: |x| ATTACK.eval(x) },
    MapDef { id: "decay_s", kind: "linear", from: "normalized", to: "s", source: "engine::DECAY_MAX_S", params: &["decay_s"], note: Some("the ADSR's decay, and the legacy `event` bell's ring (an instrument named `event`, subtractive)"), bounds: || DECAY.ends(), f: |x| DECAY.eval(x) },
    MapDef { id: "release_s", kind: "linear", from: "normalized", to: "s", source: "engine::RELEASE_MAX_S", params: &["release_s"], note: None, bounds: || RELEASE.ends(), f: |x| RELEASE.eval(x) },
    MapDef { id: "gain_db", kind: "affine", from: "normalized", to: "db", source: "params::GAIN", params: &["level_db", "osc_a_level_db", "osc_b_level_db", "master_gain_db", "return_gain_db"], note: Some("every gain (and a send): −60..+6 dB, -Infinity silent; the DSP multiplies by 10^(dB/20)"), bounds: || GAIN.ends(), f: |x| GAIN.eval(x) },
    MapDef { id: "drive_db", kind: "affine", from: "normalized", to: "db", source: "params::DRIVE", params: &["drive_db"], note: Some("the soft clipper's pre-gain: ×10^(dB/20), 0 dB bypasses it"), bounds: || DRIVE.ends(), f: |x| DRIVE.eval(x) },
    MapDef { id: "resonance_q", kind: "affine", from: "normalized", to: "q", source: "dsp::resonance_to_q", params: &["resonance"], note: Some("the filter's Q (0 ⇒ Butterworth, no peak); on a `pitched: true` filtered_noise instrument resonance instead sets the note band's peak"), bounds: || RESONANCE_Q.ends(), f: |x| RESONANCE_Q.eval(x) },
    MapDef { id: "fm_index_rad", kind: "linear", from: "normalized", to: "rad", source: "synth::FM_INDEX_MAX", params: &["fm_index"], note: Some("osc-B→osc-A phase-modulation depth"), bounds: || FM_INDEX.ends(), f: |x| FM_INDEX.eval(x) },
    MapDef { id: "osc_b_detune_cents", kind: "linear", from: "normalized", to: "cents", source: "synth::DETUNE_CENTS_MAX", params: &["osc_b_detune_cents"], note: Some("osc B only, upward"), bounds: || OSC_B_DETUNE.ends(), f: |x| OSC_B_DETUNE.eval(x) },
    MapDef { id: "voices_detune_cents", kind: "linear", from: "normalized", to: "cents", source: "engine::voice_spread_cents", params: &["voices_detune_cents"], note: Some("the outermost voice's offset: the instrument's sounding voices (every note, not only unison) spread evenly across ±y cents; one voice ⇒ none"), bounds: || VOICES_DETUNE.ends(), f: |x| VOICES_DETUNE.eval(x) },
    MapDef { id: "t60_s", kind: "exp", from: "normalized", to: "s", source: "synth::t60_fundamental", params: &["t60_s"], note: Some("t60_mode \"fixed\" (the default): the fundamental's ring time; each partial rings decay_rel × it"), bounds: || (T60_MIN, T60_MAX), f: |x| T60.eval(x) },
    MapDef { id: "ring_ratio", kind: "exp", from: "normalized", to: "ratio", source: "synth::ring_ratio", params: &["ring_ratio"], note: Some("t60_mode \"note\": the fundamental rings for the note's step × y (clamped to t60_s's range)"), bounds: || RING_RATIO.ends(), f: |x| RING_RATIO.eval(x) },
    MapDef { id: "shift_st", kind: "affine", from: "normalized", to: "semitones", source: "loop_doc::param_denorm", params: &["shift_st"], note: None, bounds: || SHIFT.ends(), f: |x| SHIFT.eval(x) },
    MapDef { id: "predelay_ms", kind: "affine", from: "normalized", to: "ms", source: "loop_doc::param_denorm", params: &["predelay_ms"], note: None, bounds: || PREDELAY.ends(), f: |x| PREDELAY.eval(x) },
    MapDef { id: "osc_b_ratio", kind: "affine", from: "normalized", to: "ratio", source: "loop_doc::param_denorm", params: &["osc_b_ratio"], note: None, bounds: || OSC_B_RATIO.ends(), f: |x| OSC_B_RATIO.eval(x) },
    MapDef { id: "root_hz", kind: "affine", from: "normalized", to: "hz", source: "loop_doc::param_denorm", params: &["root_hz"], note: Some("ADR 0047"), bounds: || ROOT_HZ.ends(), f: |x| ROOT_HZ.eval(x) },
    MapDef { id: "spread_hz", kind: "affine", from: "normalized", to: "hz", source: "loop_doc::param_denorm", params: &["spread_hz"], note: Some("the binaural beat rate"), bounds: || SPREAD_HZ.ends(), f: |x| SPREAD_HZ.eval(x) },
];

fn unit_map(d: &MapDef) -> UnitMap {
    let (lo, hi) = (d.bounds)();
    let xs: &[f32] = if d.kind == "log" { &SAMPLE_HZ } else { &SAMPLE_X };
    UnitMap { kind: d.kind, from: d.from, to: d.to, lo, hi, source: d.source, params: d.params.to_vec(), note: d.note, samples: xs.iter().map(|&x| [x, (d.f)(x)]).collect() }
}

/// The params `kind` reads, with a noise source (`noise`) or not, `pitched` or not.
fn applies(kind: &str, noise: bool, pitched: bool, leaves: &[&'static str]) -> Vec<&'static str> {
    leaves.iter().copied().filter(|l| param_applies(kind, noise, pitched, l)).collect()
}

fn synth_type(kind: &'static str, leaves: &[&'static str]) -> SynthType {
    let tonal = applies(kind, false, false, leaves);
    let mut variants = BTreeMap::new();
    for (name, pitched) in [("filtered_noise", false), ("pitched_noise", true)] {
        let set = applies(kind, true, pitched, leaves);
        if set != tonal && variants.values().all(|v| v != &set) {
            variants.insert(name, set);
        }
    }
    let materials = (kind == "modal").then(|| {
        MATERIAL_OPTIONS.iter().map(|&m| (m, material_partials(m).iter().map(|p| PartialInfo { ratio: p.ratio, amp: p.amp, decay_rel: p.decay_rel }).collect())).collect()
    });
    let note = match kind {
        "dual" => Some("the oscillators are osc_a_wave/osc_b_wave: `wave` does nothing, and `source` matters only as filtered_noise (shaped by band_lo/band_hi)"),
        "binaural" => Some("never filtered_noise; without `root_hz` the pair's root is the note's pitch"),
        _ => None,
    };
    SynthType { default: kind == SYNTH_TYPES[0], audio: kind != "midi", free_form: kind == "midi", params: tonal, variants, materials, note }
}

fn choice(options: &[&'static str], default: Option<&'static str>) -> Choice {
    Choice { options: options.to_vec(), default }
}

/// Build the catalog.
pub fn catalog() -> Catalog {
    let doc = empty_doc();
    let leaves = instrument_leaves();
    let params = leaves.iter().map(|&l| (l, param_info("params", format!("instruments.<id>.params.{l}"), l, MODULATABLE_INST_PARAMS.contains(&l), &doc))).collect();
    let reverb = REVERB_PARAM_LEAVES.iter().map(|&l| (l, param_info("reverb", format!("buses.<bus>.effects.<fx>.params.{l}"), l, true, &doc))).collect();
    let global = GLOBAL_PARAMS.iter().map(|&l| (l, param_info("global", format!("global.{l}"), l, true, &doc))).collect();
    let buses = BUS_PARAMS.iter().map(|&l| (l, param_info("bus", format!("buses.<id>.{l}"), l, true, &doc))).collect();
    let roots = ["C", "C#", "Db", "D", "D#", "Eb", "E", "F", "F#", "Gb", "G", "G#", "Ab", "A", "A#", "Bb", "B", "Cb", "B#", "E#", "Fb"]
        .iter()
        .filter_map(|&r| crate::voice::try_root_semitone(r).map(|semitone| Root { name: r.to_string(), semitone }))
        .collect();
    let modes = Mode::ALL
        .iter()
        .map(|&m| ModeInfo { name: m.name(), degrees: m.degrees(), aliases: MODES.iter().copied().filter(|a| *a != m.name() && Mode::try_from_name(a) == Some(m)).collect() })
        .collect();
    let mut c = Catalog {
        catalog_version: CATALOG_VERSION,
        schema_version: crate::schema::schema()["properties"]["schema_version"]["const"].as_str().expect("the schema pins its version"),
        engine_version: crate::version(),
        engine_build: "",
        hash: String::new(),
        synth_types: SYNTH_TYPES.iter().map(|&k| (k, synth_type(k, &leaves))).collect(),
        params,
        effects: BTreeMap::from([("reverb", Effect { params: reverb })]),
        global,
        buses,
        unit_maps: UNIT_MAPS.iter().map(|d| (d.id, unit_map(d))).collect(),
        harmony: Harmony { roots, modes },
        modulators: Modulators {
            kind: choice(&["lfo", "step_seq"], None),
            output: choice(&["control", "note"], None),
            sync: choice(&["loop", "free"], Some("loop")),
            lfo_wave: choice(LFO_WAVES, Some("sine")),
            lfo_polarity: choice(&["bipolar", "unipolar"], Some("bipolar")),
            interp: choice(&["smooth"], None),
            generator: choice(&["euclid"], None),
            pitch_walk_order: choice(&["up", "down", "random"], Some("up")),
            orrery_role: choice(&["chord", "bass", "arp", "oneshot", "context"], None),
            route_kind: choice(&["mod", "note"], None),
        },
    };
    let json = crate::doc_value::to_string(&c).expect("the catalog serializes");
    c.hash = format!("{:016x}", crate::hash::fnv1a_bytes(json.as_bytes()));
    c.engine_build = crate::build_id();
    c
}

#[cfg(test)]
mod tests;
