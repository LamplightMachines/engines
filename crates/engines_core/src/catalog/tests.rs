//! Parity: the catalog against the registries it's built from, the loop schema and its own contract —
//! and against renders: an omitted param sounds bit-identical to its catalog `default`, routed or not.
//! Each render proof also checks that the test doc hears the param (another value changes the
//! audio), so none passes by being deaf to it. The test docs are [`DocValue`]s: a silent gain's
//! default is `-Infinity`.

use super::*;
use crate::check::{check_str, DocDiag};
use crate::dsp::resonance_to_q;
use crate::doc_value::{to_doc_value, DocValue, Map};
use crate::engine::{band_to_hz, cutoff_to_hz, hz_to_band, hz_to_cutoff, voice_spread_cents, Engine, PhaseSnapshot, ATTACK_MAX_S, DECAY_MAX_S, RELEASE_MAX_S};
use crate::loop_doc::{depth_unit_for, param_denorm};
use crate::params::DEFAULTS;
use crate::synth::{ring_ratio, t60_fundamental, DETUNE_CENTS_MAX, FM_INDEX_MAX};
use serde_json::{json, Value};

/// `json!` as a doc tree.
macro_rules! doc {
    ($($t:tt)*) => { DocValue::from(json!($($t)*)) };
}

fn schema() -> &'static Value {
    crate::schema::schema()
}

/// The enum strings of a schema property.
fn enum_of(s: &Value) -> Vec<&str> {
    s["enum"].as_array().unwrap_or_else(|| panic!("no enum in {s}")).iter().map(|v| v.as_str().unwrap()).collect()
}

/// Every param of the catalog with the section it sits in.
fn all_params(c: &Catalog) -> Vec<(&'static str, &'static str, &ParamInfo)> {
    let sections = [("params", &c.params), ("reverb", &c.effects["reverb"].params), ("global", &c.global), ("bus", &c.buses)];
    sections.into_iter().flat_map(|(s, m)| m.iter().map(move |(l, p)| (s, *l, p))).collect()
}

fn kind_name(k: ParamKind) -> &'static str {
    match k {
        ParamKind::Continuous => "continuous",
        ParamKind::Categorical(_) => "categorical",
        ParamKind::IntSteps { .. } => "int_steps",
        ParamKind::Fixed => "fixed",
    }
}

#[test]
fn every_registered_param_is_in_the_catalog_once_with_its_registry_row() {
    let c = catalog();
    let all = all_params(&c);
    for s in PARAMS {
        let found: Vec<_> = all.iter().filter(|(_, l, _)| *l == s.leaf).collect();
        assert_eq!(found.len(), 1, "`{}` is in the catalog {} times", s.leaf, found.len());
        let p = found[0].2;
        assert_eq!(p.kind, kind_name(s.kind), "{}", s.leaf);
        assert_eq!(p.safe_default, s.safe_default.map(|(lo, hi)| [lo, hi]), "{}", s.leaf);
        match s.kind {
            ParamKind::Continuous | ParamKind::Fixed => assert_eq!(p.range, Some(s.doc_range().into()), "{}", s.leaf),
            ParamKind::IntSteps { lo, hi } => assert_eq!(p.range, Some([lo as f32, hi as f32]), "{}", s.leaf),
            ParamKind::Categorical(opts) => assert_eq!(p.options.as_deref(), Some(opts), "{}", s.leaf),
        }
    }
    // …and the only params outside the registry are the instrument's string params.
    let unregistered: Vec<&str> = all.iter().filter(|(_, l, _)| spec_for(l).is_none()).map(|(_, l, p)| {
        assert_eq!((p.kind, p.value_type), ("enum", "string"), "{l}");
        *l
    }).collect();
    assert_eq!(unregistered, ["source", "t60_mode"]);
    // Every default and path is the catalog's own.
    for (_, leaf, p) in &all {
        assert_eq!(p.default, default_of(leaf), "{leaf}");
        assert!(p.path.ends_with(&format!(".{leaf}")), "{leaf}: {}", p.path);
    }
    // No orphan defaults.
    for (leaf, _) in DEFAULTS {
        assert!(all.iter().any(|(_, l, _)| l == leaf), "a default for unknown param `{leaf}`");
    }
}

#[test]
fn sections_match_the_loop_schema() {
    let c = catalog();
    let keys = |v: &Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    let names = |m: &BTreeMap<&'static str, ParamInfo>| -> Vec<String> { m.keys().map(|k| k.to_string()).collect() };
    assert_eq!(names(&c.params), keys(&schema()["$defs"]["params"]["properties"]), "instrument params");
    assert_eq!(names(&c.effects["reverb"].params), keys(&schema()["$defs"]["busEffect"]["properties"]["params"]["properties"]), "reverb params");
    let mut reverb: Vec<&str> = REVERB_PARAM_LEAVES.to_vec();
    reverb.sort();
    assert_eq!(c.effects["reverb"].params.keys().copied().collect::<Vec<_>>(), reverb);
    assert_eq!(names(&c.global), keys(&schema()["properties"]["global"]["properties"]), "global");
    for leaf in c.buses.keys() {
        assert!(schema()["$defs"]["bus"]["properties"].get(*leaf).is_some(), "bus param `{leaf}` not in the schema");
    }
    let types = &schema()["$defs"]["instrument"]["properties"]["type"];
    assert_eq!(c.synth_types.keys().copied().collect::<Vec<_>>(), { let mut t = enum_of(types); t.sort(); t });
    let default: Vec<&str> = c.synth_types.iter().filter(|(_, t)| t.default).map(|(k, _)| *k).collect();
    assert_eq!(default, [types["default"].as_str().unwrap()]);
    assert_eq!(c.schema_version, "0.9");
    assert_eq!(c.engine_version, crate::version());
}

#[test]
fn synth_types_list_exactly_what_param_applies_says() {
    let c = catalog();
    let leaves = instrument_leaves();
    for (kind, t) in &c.synth_types {
        for &leaf in &leaves {
            for (noise, pitched) in [(false, false), (false, true), (true, false), (true, true)] {
                let noisy = t.variants.get("filtered_noise").unwrap_or(&t.params);
                let listed = match (noise, pitched) {
                    (false, _) => &t.params,
                    (true, false) => noisy,
                    (true, true) => t.variants.get("pitched_noise").unwrap_or(noisy),
                };
                assert_eq!(listed.contains(&leaf), param_applies(kind, noise, pitched, leaf), "{kind} `{leaf}` (noise {noise}, pitched {pitched})");
            }
        }
        assert_eq!((t.audio, t.free_form), (*kind != "midi", *kind == "midi"), "{kind}");
    }
    // The modal materials are the engine's partial tables.
    let materials = c.synth_types["modal"].materials.as_ref().expect("modal lists its materials");
    assert_eq!(materials.len(), MATERIAL_OPTIONS.len());
    for m in MATERIAL_OPTIONS {
        let engine = material_partials(m);
        let listed = &materials[m];
        assert_eq!(listed.len(), engine.len(), "{m}");
        for (l, e) in listed.iter().zip(engine) {
            assert_eq!((l.ratio, l.amp, l.decay_rel), (e.ratio, e.amp, e.decay_rel), "{m}");
        }
    }
    assert!(c.synth_types.iter().all(|(k, t)| t.materials.is_some() == (*k == "modal")));
}

/// A route from `source` into `dest` at a tenth of its span, under the dest's depth key.
fn route(source: &str, dest: &str) -> Value {
    let mut r = json!({ "source": source, "dest": dest, "kind": "mod" });
    r[depth_unit_for(dest).key()] = json!(0.1 * crate::loop_doc::depth_span_of(dest));
    r
}

/// The diagnostics `engines-check` gives a route from an LFO into `dest` (an instrument `x` of `kind`
/// with `params`, a reverb bus `rv` with an effect `verb`).
fn route_diags(kind: &str, params: Value, dest: &str) -> Vec<DocDiag> {
    let doc = json!({
        "schema_version": "0.9", "transport": { "bpm": 60, "master_steps": 4 }, "harmony": { "root": "C", "mode": "ionian" },
        "instruments": { "x": { "type": kind, "note_engine": { "polyphony": 1 }, "params": params, "sends_db": { "rv": -10 } } },
        "modulators": { "lfo": { "type": "lfo", "output": "control", "rate_hz": 1 }, "seq": { "type": "step_seq", "output": "note", "length": 1, "steps": [ { "step": 0, "voicing": { "a": { "offset": 0 } } } ] } },
        "matrix": { "m": route("lfo", dest), "n": { "source": "seq", "dest": "instruments.x.notes", "kind": "note", "voices": 1 } },
        "buses": { "rv": { "return_gain_db": -10, "effects": { "verb": { "type": "reverb", "params": { "room": 0.5 } } } } },
    });
    check_str(&doc.to_string()).diags.into_iter().filter(|d| d.path.starts_with("matrix.m")).collect()
}

#[test]
fn modulatable_and_glideable_match_the_engine() {
    let c = catalog();
    let doc = empty_doc();
    for leaf in MODULATABLE_INST_PARAMS {
        assert!(c.params.contains_key(leaf), "`{leaf}` is modulatable but not a catalog param");
    }
    for (section, leaf, p) in all_params(&c) {
        let dest = match section {
            "params" => format!("instruments.x.params.{leaf}"),
            "reverb" => format!("buses.rv.effects.verb.params.{leaf}"),
            "global" => format!("global.{leaf}"),
            _ => format!("buses.rv.{leaf}"),
        };
        assert_eq!(p.glideable, is_glideable_dest(&dest, &doc), "{dest}: glideable");
        if section == "params" {
            assert_eq!(p.modulatable, MODULATABLE_INST_PARAMS.contains(&leaf), "{leaf}: modulatable");
            continue;
        }
        // Outside the instruments, the checker is the authority: a route it accepts without a
        // `no-effect` warning moves the param.
        let diags = route_diags("subtractive", json!({ "level_db": -10 }), &dest);
        let clean = diags.iter().all(|d| !d.is_error() && d.code != "no-effect");
        assert_eq!(p.modulatable, clean, "{dest}: modulatable, but engines-check says {diags:?}");
    }
}

#[test]
fn enums_and_ranges_match_the_loop_schema() {
    let c = catalog();
    let sections = [
        ("params", &schema()["$defs"]["params"]["properties"]),
        ("reverb", &schema()["$defs"]["busEffect"]["properties"]["params"]["properties"]),
        ("global", &schema()["properties"]["global"]["properties"]),
        ("bus", &schema()["$defs"]["bus"]["properties"]),
    ];
    for (section, leaf, p) in all_params(&c) {
        let s = &sections.iter().find(|(n, _)| *n == section).unwrap().1[leaf];
        match s.get("enum") {
            Some(_) => assert_eq!(p.options.as_deref(), Some(enum_of(s).as_slice()), "{leaf}: options"),
            None => {
                let (lo, hi) = (s["minimum"].as_f64().unwrap() as f32, s["maximum"].as_f64().unwrap() as f32);
                assert_eq!(p.range, Some([lo, hi]), "{leaf}: range");
                assert_eq!(p.value_type, s["type"].as_str().unwrap(), "{leaf}: type");
            }
        }
        if let (Some(d), Some(Dflt::Num(v))) = (s.get("default").and_then(Value::as_f64), p.default) {
            assert_eq!(v, d as f32, "{leaf}: the schema's default");
        }
    }
    let m = &c.modulators;
    let modulator = &schema()["$defs"]["modulator"]["properties"];
    for (choice, s) in [
        (&m.kind, &modulator["type"]),
        (&m.output, &modulator["output"]),
        (&m.sync, &modulator["sync"]),
        (&m.lfo_polarity, &modulator["polarity"]),
        (&m.interp, &modulator["interp"]),
        (&m.orrery_role, &modulator["orrery_role"]),
        (&m.generator, &schema()["$defs"]["generator"]["properties"]["kind"]),
        (&m.pitch_walk_order, &schema()["$defs"]["pitchWalk"]["properties"]["order"]),
        (&m.route_kind, &schema()["$defs"]["matrixEntry"]["properties"]["kind"]),
    ] {
        assert_eq!(choice.options, enum_of(s), "{s}");
        if let Some(d) = s.get("default") {
            assert_eq!(choice.default, d.as_str(), "{s}: default");
        }
    }
    assert_eq!(m.lfo_wave.options, LFO_WAVES);
    // Every mode spelling the checker documents is a mode or an alias; every root parses.
    for spelling in MODES {
        assert!(c.harmony.modes.iter().any(|md| md.name == *spelling || md.aliases.contains(spelling)), "mode `{spelling}`");
    }
    assert_eq!(c.harmony.modes.iter().map(|md| md.name).collect::<Vec<_>>(), Mode::ALL.map(Mode::name));
    for r in &c.harmony.roots {
        assert_eq!(crate::voice::try_root_semitone(&r.name), Some(r.semitone), "{}", r.name);
    }
    assert_eq!(c.harmony.roots.iter().map(|r| r.semitone as i32).collect::<std::collections::BTreeSet<_>>().len(), 12, "every pitch class has a spelling");
}

/// Each map's engine function, wired here independently of the catalog's own table.
fn engine_map(id: &str, x: f32) -> f32 {
    match id {
        "cutoff_hz" => cutoff_to_hz(x, SAMPLE_NYQUIST),
        "hz_cutoff" => hz_to_cutoff(x),
        "band_hz" => band_to_hz(x, SAMPLE_NYQUIST),
        "hz_band" => hz_to_band(x),
        "attack_s" => x * ATTACK_MAX_S,
        "decay_s" => x * DECAY_MAX_S,
        "release_s" => x * RELEASE_MAX_S,
        "gain_db" => param_denorm("global.master_gain_db", x),
        "drive_db" => param_denorm("instruments.x.params.drive_db", x),
        "resonance_q" => resonance_to_q(x),
        "fm_index_rad" => x * FM_INDEX_MAX,
        "osc_b_detune_cents" => x * DETUNE_CENTS_MAX,
        "voices_detune_cents" => voice_spread_cents(x, 1, 2),
        "t60_s" => t60_fundamental(x),
        "ring_ratio" => ring_ratio(x),
        "shift_st" => param_denorm("instruments.x.params.shift_st", x),
        "predelay_ms" => param_denorm("buses.rv.effects.verb.params.predelay_ms", x),
        "osc_b_ratio" => param_denorm("instruments.x.params.osc_b_ratio", x),
        "root_hz" => param_denorm("instruments.x.params.root_hz", x),
        "spread_hz" => param_denorm("instruments.x.params.spread_hz", x),
        other => panic!("no engine function wired for unit map `{other}`"),
    }
}

#[test]
fn unit_map_samples_come_from_the_engine_and_fit_their_formula() {
    let c = catalog();
    let all = all_params(&c);
    for (id, m) in &c.unit_maps {
        assert!(m.samples.len() >= 5, "{id}");
        for &[x, y] in &m.samples {
            assert_eq!(y.to_bits(), engine_map(id, x).to_bits(), "{id}({x}): the sample isn't the engine's");
            let model = match m.kind {
                "exp" => m.lo * (m.hi / m.lo).powf(x),
                "log" => ((x / m.lo).ln() / (m.hi / m.lo).ln()).clamp(0.0, 1.0),
                "linear" => m.hi * x,
                "affine" => m.lo + (m.hi - m.lo) * x,
                k => panic!("{id}: unknown kind {k}"),
            };
            assert!((model - y).abs() <= 1e-4 * y.abs().max(1.0), "{id}({x}): the {} formula gives {model}, the engine {y}", m.kind);
        }
        for leaf in &m.params {
            let p = all.iter().find(|(_, l, _)| l == leaf).unwrap_or_else(|| panic!("{id}: unknown param `{leaf}`")).2;
            assert!(p.unit_maps.contains(id), "{leaf} doesn't list {id}");
        }
    }
    // Every param the editor shows in physical units reads through a map.
    for (_, leaf, p) in &all {
        if matches!(p.display_unit, Some("Hz" | "s" | "st" | "ms" | "×" | "¢" | "dB")) {
            assert!(!p.unit_maps.is_empty(), "`{leaf}` shows {:?} but has no unit map", p.display_unit);
        }
    }
    // The anchors the mapping guide quotes.
    let um = &c.unit_maps;
    assert_eq!((um["cutoff_hz"].lo, um["cutoff_hz"].hi), (80.0, 8000.0));
    assert_eq!((um["band_hz"].lo, um["band_hz"].hi), (20.0, 4000.0));
    assert_eq!((um["t60_s"].lo, um["t60_s"].hi), (T60_MIN, T60_MAX));
    assert_eq!((um["t60_s"].lo, um["t60_s"].hi), (t60_fundamental(0.0), t60_fundamental(1.0)));
    assert_eq!((um["attack_s"].hi, um["decay_s"].hi, um["release_s"].hi), (3.0, 4.0, 4.0));
    assert_eq!((um["gain_db"].lo, um["gain_db"].hi, um["drive_db"].hi), (-60.0, 6.0, 20.0));
    assert_eq!((um["fm_index_rad"].hi, um["osc_b_detune_cents"].hi, um["voices_detune_cents"].hi), (8.0, 50.0, 100.0));
    assert!((um["resonance_q"].lo - 0.707).abs() < 1e-6 && (um["resonance_q"].hi - 6.0).abs() < 1e-5);
}

#[test]
fn the_hash_follows_the_content() {
    let (a, b) = (catalog(), catalog());
    assert_eq!(a.hash, b.hash, "deterministic");
    assert!(a.hash.len() == 16 && a.hash.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()), "{}", a.hash);
    let mut c = a.clone();
    c.hash.clear();
    // The build isn't content (a commit that changes nothing here keeps the cache key): the hash is
    // taken with it empty.
    assert_eq!(c.engine_build, crate::build_id());
    c.engine_build = "";
    let hash = |c: &Catalog| format!("{:016x}", crate::hash::fnv1a_bytes(crate::doc_value::to_string(c).unwrap().as_bytes()));
    assert_eq!(hash(&c), a.hash);
    c.params.get_mut("cutoff_hz").unwrap().default = Some(Dflt::Num(2000.0));
    assert_ne!(hash(&c), a.hash);
}

// ── the contract ────────────────────────────────────────────────────────────

fn contract() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contracts/capability_catalog_v2.schema.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).expect("the contract parses")
}

#[test]
fn the_catalog_matches_its_contract() {
    let schema = contract();
    // The catalog's JSON is the doc writer's (a silent gain's default is -Infinity); the contract
    // validator reads strict JSON, where that is null — which `default` allows too.
    let text = crate::doc_value::to_string(&catalog()).unwrap();
    assert!(text.contains("\"default\":-Infinity"), "a silent default is written as one");
    let v = json5::from_str::<DocValue>(&text).unwrap().to_json();
    let errs = crate::schema::validate_subset(&v, &schema);
    assert!(errs.is_empty(), "{errs:#?}");
    // …and the check has teeth: a key it doesn't know, a missing one, a wrong type and a bad enum fail.
    let mut bad = v.clone();
    bad["extra"] = json!(1);
    bad.as_object_mut().unwrap().remove("hash");
    bad["params"]["cutoff_hz"]["modulatable"] = json!("yes");
    bad["unit_maps"]["cutoff_hz"]["kind"] = json!("cubic");
    let errs = crate::schema::validate_subset(&bad, &schema);
    assert_eq!(errs.len(), 4, "{errs:#?}");
}

// ── renders ─────────────────────────────────────────────────────────────────

const SR: f64 = 8_000.0;

/// What the test docs author for a param: a value away from its default, so its absence is audible.
fn exercise(leaf: &str) -> DocValue {
    match leaf {
        "wave" => doc!("saw_soft"),
        "source" => doc!("triangle"),
        "level_db" => doc!(-8),
        "drive_db" => doc!(6),
        "cutoff_hz" => doc!(600),
        "resonance" => doc!(0.5),
        "attack_s" => doc!(0.09),
        "decay_s" => doc!(0.32),
        "sustain" => doc!(0.6),
        "release_s" => doc!(0.24),
        "band_lo_hz" => doc!(250),
        "band_hi_hz" => doc!(1000),
        "voices_detune_cents" => doc!(30),
        "shift_st" => doc!(12),
        "shift_mix" => doc!(0.4),
        "osc_a_wave" => doc!("triangle"),
        "osc_b_wave" => doc!("square"),
        "osc_a_level_db" => doc!(-4.4),
        "osc_b_level_db" => doc!(-8),
        "osc_b_ratio" => doc!(2.0),
        "osc_b_detune_cents" => doc!(15),
        "fm_index" => doc!(0.25),
        "fm_feedback" => doc!(0.2),
        "material" => doc!("vibraphone"),
        "num_modes" => doc!(2),
        "brightness" => doc!(0.3),
        "strike_pos" => doc!(0.2),
        "t60_s" => doc!(0.24),
        "ring_ratio" => doc!(0.7),
        "t60_mode" => doc!("note"),
        "stretch" => doc!(0.5),
        "root_hz" => doc!(110),
        "spread_hz" => doc!(6),
        "room" => doc!(0.6),
        "damping" => doc!(0.4),
        "shimmer" => doc!(0.3),
        "width" => doc!(0.7),
        "predelay_ms" => doc!(20),
        "master_gain_db" => doc!(-2),
        "stereo_width" => doc!(0.8),
        "return_gain_db" => doc!(-6),
        other => panic!("no exercise value for `{other}`"),
    }
}

/// A test doc: instrument `x` playing a two-voice chord that drops to one at the half loop (the other
/// releases), sent to a reverb bus `rv` (unless `buses` is off), optionally an LFO route into `route`.
#[derive(Clone)]
struct Patch {
    kind: &'static str,
    pitched: bool,
    params: Map,
    verb: Map,
    bus: Map,
    global: Map,
    buses: bool,
    route: Option<String>,
}

impl Patch {
    fn new(kind: &'static str) -> Self {
        let full = |leaves: &[&str]| leaves.iter().map(|l| (l.to_string(), exercise(l))).collect::<Map>();
        Patch { kind, pitched: false, params: Map::new(), verb: full(&REVERB_PARAM_LEAVES), bus: full(&BUS_PARAMS), global: Map::new(), buses: true, route: None }
    }

    fn doc(&self) -> DocValue {
        let mut inst = doc!({ "type": self.kind, "note_engine": { "polyphony": 4 } });
        inst["params"] = DocValue::Object(self.params.clone());
        if self.pitched {
            inst["pitched"] = doc!(true);
        }
        if self.buses {
            inst["sends_db"] = doc!({ "rv": -6 });
        }
        let mut d = doc!({
            "schema_version": "0.9", "transport": { "bpm": 240, "master_steps": 4 }, "harmony": { "root": "A", "mode": "aeolian" },
            "modulators": { "seq": { "type": "step_seq", "output": "note", "length": 4, "steps": [
                { "step": 0, "voicing": { "a": { "offset": -12 }, "b": { "offset": -5 } } },
                { "step": 2, "voicing": { "a": { "offset": -9 }, "b": { "release": true } } } ] } },
            "matrix": { "m": { "source": "seq", "dest": "instruments.x.notes", "kind": "note", "voices": 2 } },
        });
        d["global"] = DocValue::Object(self.global.clone());
        d["instruments"]["x"] = inst;
        if self.buses {
            let mut bus = self.bus.clone();
            let mut verb = doc!({ "type": "reverb" });
            verb["params"] = DocValue::Object(self.verb.clone());
            bus.insert("effects".into(), DocValue::Object(Map::from([("verb".to_string(), verb)])));
            d["buses"]["rv"] = DocValue::Object(bus);
        }
        if let Some(dest) = &self.route {
            // Unipolar, so it's off the base at the strikes (a bipolar sine is 0 at every onset here).
            d["modulators"]["lfo"] = doc!({ "type": "lfo", "output": "control", "rate_hz": 3, "wave": "sine", "polarity": "unipolar" });
            let mut r = doc!({ "source": "lfo", "dest": dest, "kind": "mod" });
            r[depth_unit_for(dest).key()] = DocValue::from(0.2 * crate::loop_doc::depth_span_of(dest));
            d["matrix"]["r"] = r;
        }
        d
    }

    /// One loop, as sample bits.
    fn render(&self) -> Vec<u32> {
        let src = self.doc().to_string();
        let doc = LoopDoc::from_json_str(&src).unwrap_or_else(|e| panic!("{e}\n{src}"));
        Engine::new(SR).render(&doc, &PhaseSnapshot::empty()).audio.iter().map(|s| s.to_bits()).collect()
    }
}

/// `p`, `section`'s param `leaf` set to `v` (or removed).
fn with(p: &Patch, section: &str, leaf: &str, v: Option<DocValue>) -> Patch {
    let mut p = p.clone();
    let map = match section {
        "params" => &mut p.params,
        "reverb" => &mut p.verb,
        "global" => &mut p.global,
        _ => &mut p.bus,
    };
    match v {
        Some(v) => map.insert(leaf.into(), v),
        None => map.remove(leaf),
    };
    p
}

/// Leaving `leaf` out of `base` renders exactly like writing `expect` — and unlike writing its
/// exercise value (the doc hears the param). `what` names the case.
fn assert_omitted_sounds_like(base: &Patch, section: &str, leaf: &str, expect: DocValue, what: &str) {
    let omitted = with(base, section, leaf, None).render();
    assert!(omitted.iter().any(|&s| s != 0), "{what}: the test doc is silent");
    assert!(omitted == with(base, section, leaf, Some(expect.clone())).render(), "{what}: omitting `{leaf}` doesn't sound like {expect}");
    // Dual's oscillators are osc_a/osc_b_wave: `source` matters only as the noise, and `wave` (whose
    // options are all pitched) not at all — `dual_reads_wave_only_to_choose_the_noise` pins that.
    let other = match (base.kind, leaf) {
        ("dual", "wave") => return,
        ("dual", "source") => doc!("filtered_noise"),
        _ => exercise(leaf),
    };
    assert!(omitted != with(base, section, leaf, Some(other)).render(), "{what}: the test doc can't hear `{leaf}`");
}

/// The synth-type variants with audio: `(type, noise, pitched)`.
const VARIANTS: [(&str, bool, bool); 7] = [
    ("subtractive", false, false),
    ("subtractive", true, false),
    ("subtractive", true, true),
    ("dual", false, false),
    ("dual", true, false),
    ("modal", false, false),
    ("binaural", false, false),
];

/// The variant's params (from the catalog) and its test patch: every one of them authored away from
/// its default — except `wave`/`source` (one or the other; a noise variant's `source` selects it),
/// `root_hz` (set, it overrides the note) and a modal's `t60_mode` and `ring_ratio` (the fixed mode's
/// `t60_s` is authored; `the_note_mode_rings_by_ring_ratio` covers the other).
fn variant(c: &Catalog, kind: &'static str, noise: bool, pitched: bool) -> (Vec<&'static str>, Patch) {
    let t = &c.synth_types[kind];
    let noisy = t.variants.get("filtered_noise").unwrap_or(&t.params);
    let leaves = match (noise, pitched) {
        (false, _) => t.params.clone(),
        (true, false) => noisy.clone(),
        (true, true) => t.variants.get("pitched_noise").unwrap_or(noisy).clone(),
    };
    let mut patch = Patch::new(kind);
    patch.pitched = pitched;
    for &l in &leaves {
        if !(matches!(l, "source" | "root_hz" | "t60_mode" | "ring_ratio") || noise && l == "wave") {
            patch.params.insert(l.into(), exercise(l));
        }
    }
    if noise {
        patch.params.insert("source".into(), doc!("filtered_noise"));
    }
    let leaves = leaves.into_iter().filter(|l| !matches!(*l, "t60_mode" | "ring_ratio")).collect();
    (leaves, patch)
}

#[test]
fn an_omitted_instrument_param_renders_as_its_default() {
    let c = catalog();
    std::thread::scope(|s| {
        for (kind, noise, pitched) in VARIANTS {
            let c = &c;
            s.spawn(move || {
                let (leaves, base) = variant(c, kind, noise, pitched);
                for leaf in leaves {
                    let Some(default) = c.params[leaf].default else { continue };
                    if noise && matches!(leaf, "wave" | "source") {
                        continue; // the source picks the variant
                    }
                    // `wave` and `source` are one param: test either alone.
                    let base = if leaf == "source" { with(&base, "params", "wave", None) } else { base.clone() };
                    let what = format!("{kind}{}", if pitched { " pitched noise" } else if noise { " noise" } else { "" });
                    assert_omitted_sounds_like(&base, "params", leaf, to_doc_value(&default).unwrap(), &what);
                }
            });
        }
    });
}

#[test]
fn dual_reads_wave_only_to_choose_the_noise() {
    let c = catalog();
    assert!(c.synth_types["dual"].params.contains(&"wave") && c.synth_types["dual"].note.is_some_and(|n| n.contains("osc_a_wave")));
    let (_, base) = variant(&c, "dual", false, false);
    let base = with(&base, "params", "wave", None); // `wave` wins over `source`
    let plain = base.render();
    for w in WAVE_OPTIONS {
        assert!(plain == with(&base, "params", "wave", Some(doc!(w))).render(), "dual `wave: {w}` changes the sound — update the dual note");
        assert!(plain == with(&base, "params", "source", Some(doc!(w))).render(), "dual `source: {w}` changes the sound — update the dual note");
    }
    assert!(plain != with(&base, "params", "source", Some(doc!("filtered_noise"))).render());
}

/// A modal in `t60_mode: "note"` rings by `ring_ratio` (and an omitted mode is `fixed`): the half of
/// the modal ring the main variant leaves out.
#[test]
fn the_note_mode_rings_by_ring_ratio() {
    let c = catalog();
    let (_, base) = variant(&c, "modal", false, false);
    let ringless = with(&base, "params", "t60_s", None);
    assert_omitted_sounds_like(&ringless, "params", "t60_mode", to_doc_value(&c.params["t60_mode"].default.unwrap()).unwrap(), "modal");
    let note = with(&ringless, "params", "t60_mode", Some(doc!("note")));
    assert_omitted_sounds_like(&note, "params", "ring_ratio", to_doc_value(&c.params["ring_ratio"].default.unwrap()).unwrap(), "modal note");
    let routed = Patch { route: Some("instruments.x.params.ring_ratio".into()), ..note };
    assert_omitted_sounds_like(&routed, "params", "ring_ratio", to_doc_value(&c.params["ring_ratio"].default.unwrap()).unwrap(), "modal note routed");
}

#[test]
fn an_omitted_bus_or_global_param_renders_as_its_default() {
    let c = catalog();
    let (_, base) = variant(&c, "subtractive", false, false);
    for (section, leaf, p) in all_params(&c).into_iter().filter(|(s, _, _)| *s != "params") {
        let default = to_doc_value(&p.default.expect("bus and global params all have defaults")).unwrap();
        assert_omitted_sounds_like(&base, section, leaf, default, section);
    }
}

/// A route into an omitted param starts from its default, as the param plays unrouted (schema v0.9;
/// v0.8 started it from 0). A silent gain's route is inaudible either way.
#[test]
fn a_route_into_an_omitted_param_starts_from_its_default() {
    let c = catalog();
    std::thread::scope(|s| {
        for (kind, noise, pitched) in VARIANTS {
            let c = &c;
            s.spawn(move || {
                let (leaves, base) = variant(c, kind, noise, pitched);
                for leaf in leaves {
                    let p = &c.params[leaf];
                    let Some(default) = p.default.filter(|_| p.modulatable) else { continue };
                    let routed = Patch { route: Some(format!("instruments.x.params.{leaf}")), ..base.clone() };
                    assert_omitted_sounds_like(&routed, "params", leaf, to_doc_value(&default).unwrap(), &format!("{kind} routed"));
                }
            });
        }
    });
    let (_, base) = variant(&c, "subtractive", false, false);
    for (section, leaf, p) in all_params(&c).into_iter().filter(|(s, _, p)| *s != "params" && p.modulatable) {
        let dest = match section {
            "reverb" => format!("buses.rv.effects.verb.params.{leaf}"),
            "global" => format!("global.{leaf}"),
            _ => format!("buses.rv.{leaf}"),
        };
        let routed = Patch { route: Some(dest), ..base.clone() };
        assert_omitted_sounds_like(&routed, section, leaf, to_doc_value(&p.default.unwrap()).unwrap(), &format!("{section} routed"));
    }
}
