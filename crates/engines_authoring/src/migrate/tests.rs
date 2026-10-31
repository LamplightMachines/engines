//! `engines-migrate`'s conversions, per unit and per section, against hand-worked values; its frozen maps
//! against the v0.9 curves (P11: a migrated value lands where the v0.8 engine read it); every repo doc
//! is v0.9 and left alone; and three snapshots of whole docs (`ENGINES_BLESS=1` rewrites them, say why).

use super::*;
use crate::cst::Cst;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn mig(src: &str) -> Migration {
    migrate(src).unwrap_or_else(|e| panic!("{e}\n{src}"))
}

/// A one-instrument doc around `params` / `matrix` / extra top-level members.
fn doc(params: &str, matrix: &str, extra: &str) -> String {
    format!(r#"{{ schema_version: "0.8", global: {{ master_gain: 0.5 }}, instruments: {{ pad: {{ params: {{ {params} }}, sends: {{ rv: 0.5 }} }} }}, buses: {{ rv: {{ return_gain: 0.25, effects: {{}} }} }}, matrix: {{ {matrix} }}{extra} }}"#)
}

fn has(m: &Migration, needle: &str) {
    assert!(m.text.contains(needle), "missing `{needle}` in:\n{}", m.text);
}

// ── the frozen maps ──────────────────────────────────────────────────────────

/// ADR 0054 P11, permanent: a v0.8 value through the frozen map, then the v0.9 curve's inverse, lands
/// on the normalized value the v0.8 engine read (within 1e-5) — so a migrated doc renders like its
/// original. Three changes are deliberate: `decay` is 0–4 s now (P12: the same seconds sit at ¾ of the
/// old value), a gain is dB, so the v0.8 amplitude comes back through `db_to_amp`, and the band edges'
/// curve was re-anchored at 20 Hz, so they land on the Hz the v0.8 engine played instead.
#[test]
fn the_frozen_maps_land_where_the_v08_engine_read() {
    use engines_core::params::{db_to_amp, spec_for};
    let back = |leaf: &str, native: f64| spec_for(leaf).and_then(|s| s.curve).unwrap_or_else(|| panic!("{leaf} has a curve")).inverse(native as f32) as f64;
    let close = |got: f64, want: f64, what: &str, x: f64| assert!((got - want).abs() <= 1e-5, "{what} at {x}: {got} vs {want}");
    for i in 0..=100 {
        let x = i as f64 / 100.0;
        for (leaf, native) in [
            ("cutoff_hz", v08::cutoff_hz(x)),
            ("t60_s", v08::t60_s(x)),
            ("ring_ratio", v08::ring_ratio(x)),
            ("attack_s", x * v08::ATTACK_MAX_S),
            ("release_s", x * v08::RELEASE_MAX_S),
            ("voices_detune_cents", x * v08::VOICES_DETUNE_CENTS),
            ("osc_b_detune_cents", x * v08::OSC_B_DETUNE_CENTS),
        ] {
            close(back(leaf, native), x, leaf, x);
        }
        for leaf in ["band_lo_hz", "band_hi_hz"] {
            let (hz, band) = (v08::band_hz(x), spec_for(leaf).and_then(|s| s.curve).unwrap());
            close(band.eval(band.inverse(hz as f32)) as f64 / hz, 1.0, leaf, x);
        }
        close(back("decay_s", x * v08::DECAY_MAX_S), x * 0.75, "decay_s (0–4 s)", x);
        close(back("decay_s", x * v08::BELL_DECAY_MAX_S), x, "the event bell's decay_s", x);
        // drive: the v0.8 pre-gain 1 + 9x is the v0.9 10^(dB/20).
        close(db_to_amp(v08::drive_db(x) as f32) as f64, 1.0 + 9.0 * x, "drive's pre-gain", x);
        // A gain above the −60 dB floor: amplitude → dB → amplitude.
        if x >= 0.001 {
            close(db_to_amp(amp_db(x).unwrap() as f32) as f64, x, "a gain", x);
        }
    }
    // The spans a route's depth is scaled by (depth_st = depth × 48, …) are the v0.9 ranges.
    let span = |leaf: &str| {
        let (lo, hi) = engines_core::loop_doc::param_range(leaf);
        (hi - lo) as f64
    };
    for (leaf, want) in [("shift_st", v08::SHIFT_SPAN_ST), ("predelay_ms", v08::PREDELAY_SPAN_MS), ("osc_b_ratio", v08::OSC_B_RATIO_SPAN), ("root_hz", v08::ROOT_HZ_SPAN), ("spread_hz", v08::SPREAD_HZ_SPAN)] {
        assert_eq!(span(leaf), want, "{leaf}");
    }
}

#[test]
fn numbers_are_written_to_their_units_precision() {
    assert_eq!((fmt(318.486, 2), fmt(1.05, 3), fmt(2.0, 3), fmt(-0.0001, 2), fmt(12.0, 1)), ("318.49".into(), "1.05".into(), "2".into(), "0".into(), "12".into()));
    assert_eq!((fmt_hz(318.486), fmt_hz(1267.94)), ("318.49".into(), "1267.9".into()));
    assert_eq!(amp_db(0.0), None);
    assert!((amp_db(0.5).unwrap() + 6.0206).abs() < 1e-3);
    assert_eq!(amp_db(1e-5), Some(FLOOR_DB));
}

// ── values ───────────────────────────────────────────────────────────────────

#[test]
fn time_frequency_and_gain_take_real_units_with_suffixed_keys() {
    let m = mig(&doc("cutoff: 0.3, band_lo: 0.2, band_hi: 0.8, attack: 0.35, decay: 0.2, sustain: 0.8, release: 0.5, level: 0.5, drive: 0.25, voices_detune: 0.04, resonance: 0.2, shift: 12, shift_mix: 0.5", "", ""));
    for want in ["cutoff_hz: 318.49", "band_lo_hz: 174.94", "band_hi_hz: 1829.2", "attack_s: 1.05", "decay_s: 0.6", "sustain: 0.8", "release_s: 2", "level_db: -6.02", "drive_db: 10.24", "voices_detune_cents: 4", "resonance: 0.2", "shift_st: 12", "shift_mix: 0.5"] {
        has(&m, want);
    }
    has(&m, r#"schema_version: "0.9""#);
    has(&m, "master_gain_db: -6.02");
    has(&m, "sends_db: { rv: -6.02 }");
    has(&m, "return_gain_db: -12.04");
}

#[test]
fn the_bell_rings_over_4_s_and_t60_follows_its_mode() {
    let src = r#"{ schema_version: "0.8", instruments: {
        event: { params: { decay: 0.5 } },
        pad: { params: { decay: 0.5 } },
        d: { type: "dual", params: { decay: 0.5, osc_b_detune: 0.2, osc_b_ratio: 1.5 } },
        bell: { type: "modal", params: { t60_scale: 0.5 } },
        pluck: { type: "modal", params: { t60_mode: "note", t60_scale: 0.75 } },
        bin: { type: "binaural", params: { root_hz: 109.5, spread_hz: 4 } } } }"#;
    let m = mig(src);
    has(&m, "event: { params: { decay_s: 2 } }");
    has(&m, "pad: { params: { decay_s: 1.5 } }");
    has(&m, "params: { decay_s: 1.5, osc_b_detune_cents: 10, osc_b_ratio: 1.5 }");
    has(&m, "t60_s: 0.632");
    has(&m, r#"t60_mode: "note", ring_ratio: 1.414"#);
    has(&m, "root_hz: 109.5, spread_hz: 4");
}

#[test]
fn silence_is_minus_infinity_and_quiet_gains_meet_the_floor() {
    let m = mig(&doc("level: 0.0, osc_a_level: 0.0005", "", ""));
    has(&m, "level_db: -Infinity");
    has(&m, "osc_a_level_db: -60");
    assert!(m.notes.iter().any(|n| n.contains("raised to the −60 dB floor")), "{:?}", m.notes);
}

#[test]
fn a_midi_instruments_params_are_its_synths_and_stay() {
    let src = r#"{ schema_version: "0.8", instruments: { ext: { type: "midi", params: { cutoff: 0.4, level: 0.7 } } },
        matrix: { m: { source: "lfo", dest: "instruments.ext.params.cutoff", kind: "mod", depth: 0.2 } } }"#;
    let m = mig(src);
    has(&m, "params: { cutoff: 0.4, level: 0.7 }");
    has(&m, r#"dest: "instruments.ext.params.cutoff", kind: "mod", depth: 0.2"#);
}

// ── depths ───────────────────────────────────────────────────────────────────

#[test]
fn a_depth_is_a_distance_in_its_dests_unit() {
    let routes = r#"
        a: { source: "l", dest: "instruments.pad.params.cutoff", kind: "mod", depth: 0.06 },
        b: { source: "l", dest: "instruments.pad.params.attack", kind: "mod", depth: 0.1 },
        c: { source: "l", dest: "instruments.pad.params.shift", kind: "mod", depth: 0.25 },
        d: { source: "l", dest: "instruments.pad.params.voices_detune", kind: "mod", depth: 0.1 },
        e: { source: "l", dest: "instruments.pad.params.resonance", kind: "mod", depth: 0.3 },
        f: { source: "l", dest: "buses.rv.effects.verb.params.predelay", kind: "mod", depth: 0.2 },
        g: { source: "l", dest: "global.stereo_width", kind: "mod", depth: 0.1 },
        h: { source: "l", dest: "instruments.pad.params.band_hi", kind: "mod", depth: -0.5 },"#;
    let m = mig(&doc("cutoff: 0.3", routes, ""));
    has(&m, r#"dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: 0.399"#);
    has(&m, r#"dest: "instruments.pad.params.attack_s", kind: "mod", depth_s: 0.3"#);
    has(&m, r#"dest: "instruments.pad.params.shift_st", kind: "mod", depth_st: 12"#);
    has(&m, r#"dest: "instruments.pad.params.voices_detune_cents", kind: "mod", depth_cents: 10"#);
    has(&m, r#"dest: "instruments.pad.params.resonance", kind: "mod", depth: 0.3"#);
    has(&m, r#"dest: "buses.rv.effects.verb.params.predelay_ms", kind: "mod", depth_ms: 20"#);
    has(&m, r#"dest: "global.stereo_width", kind: "mod", depth: 0.1"#);
    has(&m, r#"dest: "instruments.pad.params.band_hi_hz", kind: "mod", depth_oct: -2.822"#);
}

#[test]
fn a_t60_depth_is_in_octaves_on_either_mode() {
    let src = r#"{ schema_version: "0.8", instruments: { b: { type: "modal", params: { t60_scale: 0.5 } }, n: { type: "modal", params: { t60_mode: "note", t60_scale: 0.5 } } },
        matrix: { x: { source: "l", dest: "instruments.b.params.t60_scale", kind: "mod", depth: 0.1 }, y: { source: "l", dest: "instruments.n.params.t60_scale", kind: "mod", depth: 0.25 } } }"#;
    let m = mig(src);
    has(&m, r#"dest: "instruments.b.params.t60_s", kind: "mod", depth_oct: 0.732"#);
    has(&m, r#"dest: "instruments.n.params.ring_ratio", kind: "mod", depth_oct: 1"#);
}

/// The owner's rule (2026-10-27): the source at +1 lands on the amplitude it did.
#[test]
fn a_gain_depth_keeps_the_sources_plus_one() {
    let routes = r#"chug: { source: "s", dest: "instruments.pad.params.level", kind: "mod", depth: -0.28 },
        fire: { source: "s", dest: "instruments.pad.params.level", kind: "mod", depth: 0.07 },
        duck: { source: "s", dest: "instruments.pad.params.level", kind: "mod", depth: -0.9 },
        mg: { source: "s", dest: "global.master_gain", kind: "mod", depth: 0.25 },"#;
    let m = mig(&doc("level: 0.55", routes, ""));
    // 20·log10(0.27 / 0.55) = −6.18; 20·log10(0.62 / 0.55) = +1.04; a duck past silence lands on the floor.
    has(&m, "depth_db: -6.18");
    has(&m, "depth_db: 1.04");
    has(&m, &format!("depth_db: {}", fmt(FLOOR_DB - 20.0 * 0.55f64.log10(), 2)));
    has(&m, r#"dest: "global.master_gain_db", kind: "mod", depth_db: 3.52"#);
    assert_eq!(m.notes.iter().filter(|n| n.contains("gain route")).count(), 4, "{:?}", m.notes);
}

#[test]
fn a_route_lifting_a_silent_gain_starts_it_at_the_floor() {
    let src = r#"{ schema_version: "0.8", instruments: { pad: { params: { level: 0.0 } }, d: { type: "dual", params: { level: 0.5 } } }, buses: { rv: { effects: {} } },
        matrix: {
          swell: { source: "s", dest: "instruments.pad.params.level", kind: "mod", depth: 0.5 },
          b: { source: "s", dest: "instruments.d.params.osc_b_level", kind: "mod", depth: 0.1 },
          r: { source: "s", dest: "buses.rv.return_gain", kind: "mod", depth: 0.3 } } }"#;
    let m = mig(src);
    has(&m, "pad: { params: { level_db: -60 } }");
    has(&m, "depth_db: 53.98"); // −60 dB → 0.5 (−6.02 dB)
    has(&m, "osc_b_level_db: -60");
    has(&m, "return_gain_db: -60");
    assert!(m.notes.iter().any(|n| n.contains("lifts it")), "{:?}", m.notes);
}

// ── sections ─────────────────────────────────────────────────────────────────

#[test]
fn bands_and_constraints_follow_their_paths() {
    let extra = r#", safe_bounds: { "instruments.pad.params.level": { min: 0.05, max: 1.0 }, "instruments.pad.params.cutoff": { min: 0.2 }, "matrix.m.depth": { min: 0.0, max: 0.1 }, "instruments.pad.params.sustain": { max: 0.9 } },
        constraints: { "instruments.pad.params.attack": { min: 0.1, max: 0.2 } }"#;
    let m = mig(&doc("level: 0.5", r#"m: { source: "l", dest: "instruments.pad.params.cutoff", kind: "mod", depth: 0.05 },"#, extra));
    has(&m, r#""instruments.pad.params.level_db": { min: -26.02, max: 0 }"#);
    has(&m, r#""instruments.pad.params.cutoff_hz": { min: 200.95 }"#);
    has(&m, r#""matrix.m.depth_oct": { min: 0, max: 0.664 }"#);
    has(&m, r#""instruments.pad.params.sustain": { max: 0.9 }"#);
    has(&m, r#""instruments.pad.params.attack_s": { min: 0.3, max: 0.6 }"#);
}

#[test]
fn a_song_layer_takes_its_routes_dest() {
    let extra = r#", song: { matrices: { calm: { m: { depth: -0.1 }, gone: null, add: { source: "l", dest: "instruments.pad.params.release", kind: "mod", depth: 0.1 } } }, sequence: [{ matrix: "calm", loops: 2 }] }"#;
    let m = mig(&doc("level: 0.55", r#"m: { source: "u", dest: "instruments.pad.params.level", kind: "mod", depth: 0.0 }, gone: { source: "u", dest: "instruments.pad.params.cutoff", kind: "mod", depth: 0.1 },"#, extra));
    has(&m, "calm: { m: { depth_db: -1.74 }, gone: null,");
    has(&m, r#"add: { source: "l", dest: "instruments.pad.params.release_s", kind: "mod", depth_s: 0.4 }"#);
}

#[test]
fn a_song_blocks_drift_overrides_follow_their_paths() {
    // ADR 0055: a block's `control.drift` keys are param paths and its bands are native, like
    // `constraints`; "hold" and `{ max_drift }` carry no unit.
    let extra = r#", song: { sequence: [{ loops: 2, control: { movement: { orrery: false }, drift: { "instruments.pad.params.cutoff": "hold", "instruments.pad.params.level": { min: 0.05, max: 1.0 }, "instruments.pad.params.attack": { max_drift: 0.05 } } } }] }"#;
    let m = mig(&doc("level: 0.5", r#"m: { source: "l", dest: "instruments.pad.params.cutoff", kind: "mod", depth: 0.05 },"#, extra));
    has(&m, r#""instruments.pad.params.cutoff_hz": "hold""#);
    has(&m, r#""instruments.pad.params.level_db": { min: -26.02, max: 0 }"#);
    has(&m, r#""instruments.pad.params.attack_s": { max_drift: 0.05 }"#);
    has(&m, "movement: { orrery: false }");
}

#[test]
fn reverb_mix_writes_out_the_bus_it_made_and_its_routes_go() {
    let src = r#"{ schema_version: "0.8", global: { master_gain: 1.0, reverb_mix: 0.25 },
        instruments: { a: { params: { level: 0.5 } }, b: { params: { level: 0.5 }, sends: {} }, m: { type: "midi", params: {} } },
        matrix: { r: { source: "l", dest: "global.reverb_mix", kind: "mod", depth: 0.1 } },
        safe_bounds: { "matrix.r.depth": { min: 0, max: 0.2 } } }"#;
    let m = mig(src);
    assert!(!m.text.contains("reverb_mix"), "{}", m.text);
    has(&m, "a: { params: { level_db: -6.02 }, sends_db: { rv: 0 } }");
    has(&m, "sends_db: { rv: 0 } }, m:");
    has(&m, r#"buses: { rv: { return_gain_db: -12.04, effects: { verb: { type: "reverb", params: { room: 0.5, damping: 0.5, shimmer: 0, width: 0.5, predelay_ms: 0 } } } } }"#);
    assert!(!m.text.contains("matrix.r") && !m.text.contains(r#"r: { source"#), "{}", m.text);
    assert!(Cst::parse(&m.text).is_ok(), "{}", m.text);
    // With buses of its own, reverb_mix did nothing: it just goes.
    let m = mig(r#"{ schema_version: "0.8", global: { reverb_mix: 0.25 }, instruments: {}, buses: { x: { return_gain: 1.0, effects: {} } } }"#);
    assert!(!m.text.contains("reverb_mix") && !m.text.contains("rv:"), "{}", m.text);
}

#[test]
fn a_parts_target_and_placeholders_migrate() {
    let src = r#"{ part: { summary: "s", target: "cutoff" }, instruments: { $id: { params: { level: 0.3 }, sends: { $bus: 0.1 } } },
        matrix: { m_$id: { source: "$id", dest: "instruments.$target.params.level", kind: "mod", depth: 0.1 } } }"#;
    let m = mig(src);
    assert_eq!(m.kind, DocKind::Part);
    has(&m, r#"target: "cutoff_hz""#);
    has(&m, "params: { level_db: -10.46 }, sends_db: { $bus: -20 }");
    has(&m, r#"dest: "instruments.$target.params.level_db""#);
    assert!(m.notes.iter().any(|n| n.contains("assumed base 0.5")), "{:?}", m.notes);
}

#[test]
fn an_objectives_file_migrates_against_its_doc() {
    let obj = r#"{ schema_version: "objectives_v0.1", revision: 1, optimize: "drift",
        constraints: { "instruments.event.params.decay": { min: 0.25, max: 0.5 } },
        overrides: { "instruments.pad.params.cutoff": 0.5 },
        guardrails: { max_change_per_loop: 0.05, ceilings: { "global.master_gain": 0.5 } } }"#;
    let the_doc = r#"{ schema_version: "0.8", instruments: { event: { type: "dual" }, pad: {} } }"#;
    let m = migrate_with(obj, Some(the_doc)).unwrap();
    assert_eq!(m.kind, DocKind::Objectives);
    has(&m, r#""instruments.event.params.decay_s": { min: 0.75, max: 1.5 }"#); // a dual `event` is an ADSR
    has(&m, r#""instruments.pad.params.cutoff_hz": 800"#);
    has(&m, r#""global.master_gain_db": -6.02"#);
    let alone = migrate(obj).unwrap();
    has(&alone, r#""instruments.event.params.decay_s": { min: 1, max: 2 }"#); // assumed the bell
    assert!(alone.notes.iter().any(|n| n.contains("without its loop doc")));
    assert!(migrate(r#"{ schema_version: "objectives_v0.1", optimize: "hold" }"#).unwrap().unchanged);
}

// ── text ─────────────────────────────────────────────────────────────────────

#[test]
fn a_comment_quoting_a_changed_number_gets_a_todo() {
    let src = "{ schema_version: \"0.8\",\n  instruments: { pad: { params: {\n    // a dark pad: 0.3 ≈ 318 Hz\n    cutoff: 0.3, // was 0.25\n    sustain: 0.5, // 0.5 of the peak\n  } } } }";
    let m = mig(src);
    has(&m, "// a dark pad: 0.3 ≈ 318 Hz TODO(v0.9)");
    has(&m, "// was 0.25 TODO(v0.9)");
    has(&m, "// 0.5 of the peak\n"); // its number didn't change
    assert!(mig(&m.text).unchanged, "the TODOs aren't doubled on a second run");
}

#[test]
fn a_v09_doc_is_left_alone_and_others_are_refused() {
    assert!(mig(r#"{ schema_version: "0.9", instruments: {} }"#).unchanged);
    assert!(migrate(r#"{ schema_version: "0.7" }"#).unwrap_err().contains("reads \"0.8\""));
    assert!(migrate(r#"{ instruments: {} }"#).unwrap_err().contains("not a loop doc"));
    assert!(migrate("{ schema_version: ").unwrap_err().contains("JSON5"));
}

#[test]
fn the_diff_shows_removed_then_added_lines() {
    assert_eq!(diff("a\nb\nc\n", "a\nB\nc\nd\n"), "-    2 b\n+    2 B\n+    4 d\n");
}

// ── every doc in the repo ────────────────────────────────────────────────────

fn repo_docs() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    // The closed folders (`design/`, `sandbox/`, `tools/engines-listen/`) are absent in the open set.
    let mut add = |dir: &str, keep: &dyn Fn(&str) -> bool| {
        let Ok(entries) = std::fs::read_dir(format!("{ROOT}/{dir}")) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().and_then(|n| n.to_str()).is_some_and(keep) {
                out.push(p);
            }
        }
    };
    let doc = |n: &str| (n.ends_with(".json") || n.ends_with(".json5")) && !n.contains("objectives");
    add("lab/tests/fixtures", &doc);
    add("crates/engines_authoring/assets/parts", &doc);
    add("design/vs_extension/reference", &doc);
    add("doc", &|n| n.starts_with("loop_contract_v0.9") && n.ends_with(".json5"));
    add("sandbox", &doc);
    add("tools/engines-listen/bench/docs", &doc);
    out.sort();
    assert!(out.len() >= 28, "{out:?}");
    out
}

/// Every doc in the repo is v0.9 (U5 migrated them): the migrator leaves each alone, and each whole
/// doc checks clean — not a part (it has placeholders), nor the extension's design snapshot, kept as
/// drawn (it repeats `m_fire`, as its v0.8 original did).
#[test]
fn every_repo_doc_is_v09_and_checks_clean() {
    for p in repo_docs() {
        let src = std::fs::read_to_string(&p).unwrap();
        let m = migrate(&src).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(m.unchanged, "{}: still has v0.8 in it", p.display());
        let path = p.to_string_lossy();
        if path.contains("assets/parts") || path.contains("design/vs_extension") {
            continue;
        }
        let errors: Vec<String> = engines_core::check::check_str(&src).errors().map(|d| d.to_string()).collect();
        assert!(errors.is_empty(), "{}: {errors:#?}", p.display());
    }
}

/// A strict-JSON doc stays strict JSON: Python's `json` reads the lab's fixtures.
#[test]
fn a_strict_json_doc_stays_json() {
    for p in repo_docs().into_iter().filter(|p| p.extension().is_some_and(|e| e == "json")) {
        let m = migrate(&std::fs::read_to_string(&p).unwrap()).unwrap();
        // serde_json refuses -Infinity (Python's json reads it): check with it swapped for a number.
        let json = m.text.replace("-Infinity", "-1e308");
        if let Err(e) = serde_json::from_str::<Value>(&json) {
            panic!("{}: not JSON after migrating: {e}", p.display());
        }
    }
}

/// Whole docs, against reviewed snapshots: a song with comments and gain routes, a strict-JSON doc on
/// the back-compat bus, and a part. The v0.8 inputs are kept beside the snapshots (copies of
/// the neutral test song `song_lydian_tide.json5` written back as v0.8, `loop_contract_v0.8.json` and the `halo` part as they were), so the test
/// outlives the repo's own docs moving to v0.9.
#[test]
fn whole_docs_match_their_snapshots() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/migrate");
    for (src, snap) in [
        ("song_lydian_tide.v0.8.json5", "song_lydian_tide.v0.9.json5"),
        ("loop_contract_v0.8.v0.8.json", "loop_contract_v0.8.v0.9.json"),
        ("halo.v0.8.json5", "halo.v0.9.json5"),
    ] {
        let m = migrate(&std::fs::read_to_string(format!("{dir}/{src}")).unwrap()).unwrap();
        let path = format!("{dir}/{snap}");
        if std::env::var_os("ENGINES_BLESS").is_some() {
            std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
            std::fs::write(&path, &m.text).unwrap();
        }
        let want = std::fs::read_to_string(&path).expect("run with ENGINES_BLESS=1 to write the snapshot");
        assert_eq!(m.text, want, "{src} migrates differently: re-bless with ENGINES_BLESS=1 and say why");
    }
}
