use super::*;

fn diags(src: &str) -> Vec<DocDiag> {
    check_str(src).diags
}

/// A minimal valid doc to mutate (JSON5).
const BASE: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 120, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -7.96, cutoff_hz: 800 } } },
  modulators: {
    lfo: { type: "lfo", output: "control", rate_hz: 0.1 },
    seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] },
  },
  matrix: {
    m_notes: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 2 },
    m_cut: { source: "lfo", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: 0.664 },
  },
}"#;

fn with(from: &str, to: &str) -> String {
    assert!(BASE.contains(from), "test fixture must contain {from:?}");
    BASE.replacen(from, to, 1)
}

fn has(d: &[DocDiag], path: &str, code: &str) -> bool {
    d.iter().any(|x| x.path == path && x.code == code)
}

#[test]
fn the_base_doc_is_clean() {
    let c = check_str(BASE);
    assert!(c.diags.is_empty(), "{:#?}", c.diags);
    assert!(c.doc.is_some());
}

#[test]
fn a_param_typo_is_a_located_error_with_a_suggestion() {
    let d = diags(&with("cutoff_hz: 800", "cutof_hz: 800"));
    let e = d.iter().find(|x| x.path == "instruments.pad.params.cutof_hz").expect("located");
    assert!(e.is_error() && e.code == "unknown-key");
    assert!(e.message.contains("did you mean `cutoff_hz`"), "{e}");
    assert!(LoopDoc::from_json_str(&with("cutoff_hz: 800", "cutof_hz: 800")).is_err(), "the engine refuses a typo'd doc");
    // A v0.8 name is a typo now (D2: a stale key fails instead of mis-rendering).
    let d = diags(&with("cutoff_hz: 800", "cutoff: 0.5"));
    assert!(d.iter().any(|x| x.path == "instruments.pad.params.cutoff" && x.is_error() && x.message.contains("cutoff_hz")), "{d:#?}");
}

#[test]
fn unknown_keys_anywhere_are_errors_but_x_annotations_are_free() {
    assert!(has(&diags(&with("bpm: 120", "bpm: 120, tempo: 3")), "transport.tempo", "unknown-key"));
    assert!(has(&diags(&with("rate_hz: 0.1", "rate_hz: 0.1, rate: 2")), "modulators.lfo.rate", "unknown-key"));
    assert!(diags(&with("bpm: 120", "bpm: 120, x_note: \"slow\"")).is_empty(), "x_ keys are annotations");
}

#[test]
fn enum_type_and_range_problems_are_errors() {
    let d = diags(&with("wave: \"saw_soft\"", "wave: \"sawtooth\""));
    assert!(d.iter().any(|x| x.path == "instruments.pad.params.wave" && x.code == "enum" && x.message.contains("saw_soft")), "{d:#?}");
    assert!(has(&diags(&with("level_db: -7.96", "level_db: \"-8\"")), "instruments.pad.params.level_db", "type"));
    assert!(has(&diags(&with("level_db: -7.96", "level_db: 7")), "instruments.pad.params.level_db", "range"), "+6 dB is the ceiling (P1)");
    assert!(has(&diags(&with("level_db: -7.96", "level_db: -61")), "instruments.pad.params.level_db", "range"), "−60 dB the floor (or -Infinity)");
    assert!(has(&diags(&with("cutoff_hz: 800", "cutoff_hz: 9000")), "instruments.pad.params.cutoff_hz", "range"));
}

#[test]
fn a_bad_root_or_mode_no_longer_falls_back_silently() {
    let d = diags(&with("root: \"D\"", "root: \"eb\""));
    let e = d.iter().find(|x| x.path == "harmony.root").expect("root error");
    assert!(e.message.contains("\"Eb\""), "{e}");
    let d = diags(&with("mode: \"dorian\"", "mode: \"dorain\""));
    let e = d.iter().find(|x| x.path == "harmony.mode").expect("mode error");
    assert!(e.message.contains("did you mean \"dorian\""), "{e}");
}

#[test]
fn dangling_routes_are_errors() {
    let d = diags(&with("source: \"lfo\"", "source: \"lf0\""));
    assert!(d.iter().any(|x| x.path == "matrix.m_cut.source" && x.message.contains("did you mean `lfo`")), "{d:#?}");
    assert!(has(&diags(&with("instruments.pad.notes", "instruments.pd.notes")), "matrix.m_notes.dest", "dangling"));
    assert!(has(&diags(&with("instruments.pad.params.cutoff_hz\"", "global.loudness\"")), "matrix.m_cut.dest", "invalid"));
    assert!(has(&diags(&with("instruments.pad.params.cutoff_hz\"", "instruments.pad.notes\"")), "matrix.m_cut.dest", "invalid"), "a mod route can't target notes");
}

#[test]
fn a_route_into_a_silent_gain_is_warned_inaudible() {
    // ADR 0054 P5: −∞ + any dB is −∞ — the route does nothing until the gain has a finite floor.
    let route = |src: String| src.replacen("instruments.pad.params.cutoff_hz\", kind: \"mod\", depth_oct: 0.664", "instruments.pad.params.level_db\", kind: \"mod\", depth_db: 6", 1);
    let d = diags(&route(with("level_db: -7.96", "level_db: -Infinity")));
    let w = d.iter().find(|x| x.path == "matrix.m_cut.dest" && x.message.contains("inaudible")).expect("warned");
    assert!(!w.is_error() && w.message.contains("-60 dB"), "{w}");
    assert!(diags(&route(with("level_db: -7.96", "level_db: -60"))).is_empty(), "a finite floor is the fix");
    assert!(diags(&with("level_db: -7.96", "level_db: -Infinity")).is_empty(), "no route ⇒ a clean mute");
    // …and so does an omitted gain whose default is silence (a dual's osc B).
    let dual = with("pad: { note_engine", "fm: { type: \"dual\", note_engine: { polyphony: 1 }, params: { osc_a_level_db: -2 } }, pad: { note_engine").replacen("instruments.pad.params.cutoff_hz\", kind: \"mod\", depth_oct: 0.664", "instruments.fm.params.osc_b_level_db\", kind: \"mod\", depth_db: 6", 1);
    assert!(diags(&dual).iter().any(|x| x.path == "matrix.m_cut.dest" && x.message.contains("inaudible")), "{:#?}", diags(&dual));
}

#[test]
fn a_route_takes_one_depth_key_in_its_dests_unit() {
    let d = diags(&with("depth_oct: 0.664", "depth: 0.1"));
    let e = d.iter().find(|x| x.path == "matrix.m_cut.depth").expect("the wrong key is located");
    assert!(e.is_error() && e.message.contains("`depth_oct`"), "{e}");
    let d = diags(&with("depth_oct: 0.664", "depth_oct: 0.664, depth_db: 3"));
    assert!(d.iter().any(|x| x.path == "matrix.m_cut" && x.is_error() && x.message.contains("one depth key")), "{d:#?}");
    let d = diags(&with("depth_oct: 0.664", "depth_db: 3"));
    assert!(has(&d, "matrix.m_cut.depth_db", "invalid"), "{d:#?}");
}

#[test]
fn conditional_requirements_are_enforced() {
    // The schema requires `rate_hz` only for an LFO (an allOf/if/then branch) — an LFO without it
    // would silently run at the 1 Hz default.
    let d = diags(&with("rate_hz: 0.1", "wave: \"sine\""));
    let e = d.iter().find(|x| x.path == "modulators.lfo" && x.code == "missing").expect("missing rate_hz");
    assert!(e.message.contains("rate_hz"), "{e}");
    assert!(LoopDoc::from_json_str(&with("rate_hz: 0.1", "wave: \"sine\"")).is_err());
    assert!(!has(&diags(BASE), "modulators.seq", "missing"), "a step_seq needs no rate_hz");
}

// ADR 0046: a noise instrument that says `pitched: true` reads `resonance` (the note's peak); one that
// doesn't (false, or left out) ignores it, and its warning says what would make it count. `cutoff_hz`
// does nothing on either.
#[test]
fn resonance_applies_to_pitched_noise_only() {
    let noise = |pitched: &str| with("pad: { note_engine", &format!("hiss: {{ {pitched}note_engine: {{ polyphony: 1 }}, params: {{ source: \"filtered_noise\", band_lo_hz: 260, band_hi_hz: 1830, resonance: 0.6, cutoff_hz: 800 }} }}, pad: {{ note_engine"));
    let d = diags(&noise("pitched: true, "));
    assert!(!has(&d, "instruments.hiss.params.resonance", "no-effect"), "{d:#?}");
    assert!(has(&d, "instruments.hiss.params.cutoff_hz", "no-effect"), "{d:#?}");
    for unpitched in ["", "pitched: false, "] {
        let d = diags(&noise(unpitched));
        let w = d.iter().find(|x| x.path == "instruments.hiss.params.resonance" && x.code == "no-effect").expect("unpitched noise ignores resonance");
        assert!(w.message.contains("pitched: true"), "{unpitched:?}: {w}");
    }
    assert!(param_applies("subtractive", true, true, "resonance") && !param_applies("subtractive", true, false, "resonance"));
    assert!(!param_applies("dual", true, true, "resonance"), "a dual voice's noise source has no note band");
}

/// BASE plus a `binaural` instrument `bin` with `params` (and `extra` keys before `note_engine`).
fn binaural(params: &str, extra: &str) -> String {
    with("pad: { note_engine", &format!("bin: {{ type: \"binaural\", {extra}note_engine: {{ polyphony: 1 }}, params: {{ {params} }} }}, pad: {{ note_engine"))
}

fn about_bin(d: &[DocDiag]) -> Vec<&DocDiag> {
    d.iter().filter(|x| x.path.contains("bin")).collect()
}

// ADR 0047: a binaural pair reads the subtractive params plus root_hz/spread_hz, in real Hz.
#[test]
fn binaural_reads_the_subtractive_params_and_real_hz() {
    let d = diags(&binaural("wave: \"sine\", root_hz: 109.5, spread_hz: 2.25, cutoff_hz: 318, resonance: 0.1, shift_st: 12, shift_mix: 0.2, voices_detune_cents: 10, attack_s: 1.5, sustain: 1.0, drive_db: 5.6, level_db: -10.5", ""));
    assert!(about_bin(&d).is_empty(), "{d:#?}");
    let d = diags(&binaural("spread_hz: 4, band_lo_hz: 175, fm_index: 0.3", ""));
    assert!(has(&d, "instruments.bin.params.band_lo_hz", "no-effect") && has(&d, "instruments.bin.params.fm_index", "no-effect"), "{d:#?}");
    // …and the Hz params do nothing on any other type.
    assert!(has(&diags(&with("cutoff_hz: 800", "cutoff_hz: 800, root_hz: 110, spread_hz: 4")), "instruments.pad.params.root_hz", "no-effect"));
    for leaf in ["root_hz", "spread_hz", "wave", "cutoff_hz", "shift_st", "voices_detune_cents", "release_s"] {
        assert!(param_applies("binaural", false, false, leaf), "{leaf}");
    }
    for kind in ["subtractive", "dual", "modal"] {
        assert!(!param_applies(kind, false, false, "root_hz") && !param_applies(kind, false, false, "spread_hz"), "{kind}");
    }
}

#[test]
fn binaural_hz_ranges_are_checked_and_noise_is_refused() {
    assert!(has(&diags(&binaural("root_hz: 5, spread_hz: 4", "")), "instruments.bin.params.root_hz", "range"));
    assert!(has(&diags(&binaural("root_hz: 1200, spread_hz: 4", "")), "instruments.bin.params.root_hz", "range"));
    assert!(has(&diags(&binaural("spread_hz: 41", "")), "instruments.bin.params.spread_hz", "range"));
    let d = diags(&binaural("source: \"filtered_noise\", spread_hz: 4", ""));
    let e = d.iter().find(|x| x.path == "instruments.bin.params.source" && x.is_error()).expect("noise refused");
    assert!(e.message.contains("pitched wave"), "{e}");
}

#[test]
fn binaural_advisories_are_warnings() {
    // no beat: absent (located at params) or 0 (at the param) — quiet once a route moves it.
    assert!(has(&diags(&binaural("wave: \"sine\"", "")), "instruments.bin.params", "no-effect"));
    assert!(has(&diags(&binaural("spread_hz: 0", "")), "instruments.bin.params.spread_hz", "no-effect"));
    let routed = binaural("wave: \"sine\"", "").replacen("matrix: {", "matrix: {\n    m_beat: { source: \"lfo\", dest: \"instruments.bin.params.spread_hz\", kind: \"mod\", depth_hz: 4 },", 1);
    assert!(about_bin(&diags(&routed)).is_empty(), "{:#?}", diags(&routed));
    // an authored send blends L and R in the bus
    let sent = binaural("spread_hz: 4", "sends_db: { rv: -14 }, ").replacen("matrix: {", "buses: { rv: { return_gain_db: -8 } },\n  matrix: {", 1);
    let d = diags(&sent);
    let w = d.iter().find(|x| x.path == "instruments.bin.sends_db.rv").expect("send warned");
    assert!(!w.is_error() && w.message.contains("blends"), "{w}");
    // a pinned root with undetuned unison stacks identical pairs
    let poly = |params: &str| binaural(params, "").replacen("note_engine: { polyphony: 1 }, params: { root", "note_engine: { polyphony: 3 }, params: { root", 1);
    assert!(has(&diags(&poly("root_hz: 110, spread_hz: 4")), "instruments.bin.note_engine.polyphony", "no-effect"));
    assert!(!has(&diags(&poly("root_hz: 110, spread_hz: 4, voices_detune_cents: 10")), "instruments.bin.note_engine.polyphony", "no-effect"));
    // a route into an unset root_hz does nothing (the root follows the note)
    let route = |params: &str| binaural(params, "").replacen("matrix: {", "matrix: {\n    m_root: { source: \"lfo\", dest: \"instruments.bin.params.root_hz\", kind: \"mod\", depth_hz: 9.8 },", 1);
    let d = diags(&route("spread_hz: 4"));
    let w = d.iter().find(|x| x.path == "matrix.m_root.dest").expect("unset root route warned");
    assert!(w.message.contains("follows the note"), "{w}");
    assert!(!has(&diags(&route("root_hz: 110, spread_hz: 4")), "matrix.m_root.dest", "no-effect"));
}

#[test]
fn routes_and_params_a_synth_ignores_are_warnings() {
    let d = diags(&with("instruments.pad.params.cutoff_hz\", kind: \"mod\", depth_oct: 0.664", "instruments.pad.params.fm_index\", kind: \"mod\", depth: 0.1"));
    assert!(d.iter().any(|x| x.path == "matrix.m_cut.dest" && x.code == "no-effect" && !x.is_error()), "{d:#?}");
    let d = diags(&with("cutoff_hz: 800", "cutoff_hz: 800, fm_index: 0.3"));
    assert!(d.iter().any(|x| x.path == "instruments.pad.params.fm_index" && x.code == "no-effect" && !x.is_error()), "{d:#?}");
    assert!(LoopDoc::from_json_str(&with("cutoff_hz: 800", "cutoff_hz: 800, fm_index: 0.3")).is_ok(), "warnings don't refuse the doc");
}

#[test]
fn silent_instruments_and_buses_are_warned_about() {
    let d = diags(&with("note_engine: { polyphony: 2 }, ", ""));
    assert!(d.iter().any(|x| x.path == "instruments.pad.note_engine" && x.code == "silent"), "{d:#?}");
    let src = with("matrix: {", "buses: { rv: { effects: { verb: { type: \"reverb\", params: { room: 0.5 } } } } },\n  matrix: {");
    let d = diags(&src);
    assert!(d.iter().any(|x| x.path == "buses.rv" && x.code == "silent"), "{d:#?}");
}

#[test]
fn every_problem_is_reported_not_just_the_first() {
    let src = with("cutoff_hz: 800", "cutof_hz: 800").replacen("source: \"lfo\"", "source: \"nope\"", 1).replacen("root: \"D\"", "root: \"H\"", 1);
    let errors: Vec<_> = check_str(&src).diags.into_iter().filter(|d| d.is_error()).collect();
    assert!(errors.len() >= 3, "{errors:#?}");
}

#[test]
fn the_engines_own_messages_win_over_the_schemas_for_the_same_node() {
    let d = diags(&with("rate_hz: 0.1", "rate_hz: 0.1, sync: \"looop\""));
    let errs: Vec<_> = d.iter().filter(|x| x.path == "modulators.lfo.sync").collect();
    assert_eq!(errs.len(), 1, "{errs:#?}");
    assert!(errs[0].to_string().contains("unknown sync"), "the engine's message: {}", errs[0]);
}

const CONTROL: &str = r#"optimize: "hold",
  movement: { orrery: true, bind: { chord: "seq" }, phrase_loops: 2, tension_target: 0.4, arrange: { return_hazard_scale: 1.5 } },
  constraints: { "instruments.pad.params.level_db": { min: -7.96, max: -7.96 }, "instruments.pad.params.cutoff_hz": { min: 318, max: 2000 } },
  matrix: {"#;

#[test]
fn the_docs_own_control_settings_check_clean_and_parse() {
    let c = check_str(&with("matrix: {", CONTROL));
    assert!(c.diags.is_empty(), "{:#?}", c.diags);
    let doc = c.doc.unwrap();
    let control = crate::contract::SessionControl::from_doc(&doc).expect("the doc carries control");
    assert_eq!(control.optimize, crate::contract::OptimizeMode::Hold);
    assert!(control.movement.orrery);
    assert_eq!(control.movement.phrase_loops, 2);
    assert_eq!(control.constraints["instruments.pad.params.level_db"].max, Some(-7.96));
}

#[test]
fn control_setting_mistakes_are_located() {
    let src = with("matrix: {", CONTROL);
    assert!(has(&diags(&src.replacen("orrery: true", "orrey: true", 1)), "movement.orrey", "unknown-key"));
    assert!(has(&diags(&src.replacen("\"hold\"", "\"frozen\"", 1)), "optimize", "enum"));
    assert!(has(&diags(&src.replacen("min: 318, max: 2000", "min: 2000, max: 318", 1)), "constraints.instruments.pad.params.cutoff_hz", "range"));
    // A band's min is silence only on a gain.
    assert!(diags(&src.replacen("min: -7.96, max: -7.96", "min: -Infinity, max: -7.96", 1)).is_empty());
    assert!(has(&diags(&src.replacen("min: 318, max: 2000", "min: -Infinity, max: 2000", 1)), "constraints.instruments.pad.params.cutoff_hz.min", "type"));
    let d = diags(&src.replacen("chord: \"seq\"", "chord: \"seqq\"", 1));
    let w = d.iter().find(|x| x.path == "movement.bind.chord").expect("an explicitly bound unknown id is flagged");
    assert!(!w.is_error() && w.message.contains("did you mean `seq`"), "{w}");
    assert!(diags(&src.replacen("bind: { chord: \"seq\" }, ", "", 1)).is_empty(), "unbound defaults may name absent modulators");
}

#[test]
fn the_root_defaults_merge_into_a_clean_doc() {
    // `engines-defaults.json5` is what /engines-new's skeleton carries into a new piece: only control keys, and the
    // result must check clean and carry the control.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let defaults: serde_json::Value = json5::from_str(&std::fs::read_to_string(root.join("engines-defaults.json5")).unwrap()).unwrap();
    let defaults = defaults.as_object().expect("an object");
    const ALLOWED: &[&str] = &["optimize", "movement", "constraints", "max_change", "max_drift"];
    for k in defaults.keys() {
        assert!(ALLOWED.contains(&k.as_str()) || k.starts_with("x_"), "`{k}` doesn't belong in the defaults");
    }
    let mut doc: serde_json::Value = json5::from_str(BASE).unwrap();
    doc.as_object_mut().unwrap().extend(defaults.clone());
    let c = check_str(&doc.to_string());
    assert!(c.diags.is_empty(), "{:#?}", c.diags);
    let control = crate::contract::SessionControl::from_doc(&c.doc.unwrap()).expect("control");
    assert_eq!(control.optimize, crate::contract::OptimizeMode::Drift);
    assert!(control.movement.orrery);
}

#[test]
fn every_tracked_doc_has_no_errors() {
    // The fixtures and the annotated template must all pass strict
    // checking (warnings are allowed — they're advice, not refusals).
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut paths: Vec<std::path::PathBuf> = vec![root.join("doc/loop_contract_v0.9.json5")];
    for dir in ["lab/tests/fixtures"] {
        for e in std::fs::read_dir(root.join(dir)).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if (name.ends_with(".json") || name.ends_with(".json5")) && !name.contains("objectives") {
                paths.push(p);
            }
        }
    }
    let mut failures = Vec::new();
    for p in &paths {
        let c = check_str(&std::fs::read_to_string(p).unwrap());
        for e in c.errors() {
            failures.push(format!("{}: {e}", p.display()));
        }
        // What the engine serializes (the controller's operating point, history snapshots) must
        // check clean too — serde fills defaults the author left out.
        if let Some(doc) = c.doc {
            for e in check_str(&crate::doc_value::to_string(&doc).unwrap()).errors() {
                failures.push(format!("{} (re-serialized): {e}", p.display()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(paths.len() > 15, "found the docs ({})", paths.len());
}

#[test]
fn another_schema_version_is_refused_whole_with_the_way_forward() {
    // A clean break (ADR 0054 D5): a v0.8 doc's every renamed key would be an error, so the doc is
    // refused once, at its version, naming `engines-migrate`.
    let c = check_str(&with("schema_version: \"0.9\"", "schema_version: \"0.8\""));
    assert_eq!(c.diags.len(), 1, "{:#?}", c.diags);
    let e = &c.diags[0];
    assert!(e.is_error() && e.path == "schema_version" && e.message.starts_with("schema 0.8: run `engines-migrate <doc>`"), "{e}");
    assert!(c.doc.is_none());
    let e = LoopDoc::from_json_str(&with("schema_version: \"0.9\"", "schema_version: \"0.7\"")).unwrap_err();
    assert!(e.contains("schema 0.7") && e.contains("engines-migrate"), "{e}");
}

#[test]
fn did_you_mean_is_conservative() {
    assert_eq!(did_you_mean("cutof", &["cutoff", "drive"]), Some("cutoff"));
    assert_eq!(did_you_mean("xyz", &["cutoff", "drive"]), None);
    assert_eq!(did_you_mean("Dorain", &["dorian"]), Some("dorian"));
}

#[test]
fn a_one_sided_safe_band_that_collapses_or_clamps_is_explained() {
    // `{max: -40}` on a level sits under its default floor −26 dB: the explicit side wins, so the
    // band is [-40, -40] and the authored −7.96 plays clamped — both said, neither refused.
    let d = diags(&with("  matrix: {", "  safe_bounds: { \"instruments.pad.params.level_db\": { max: -40 } },\n  matrix: {"));
    let at = "safe_bounds.instruments.pad.params.level_db";
    assert!(d.iter().all(|x| !x.is_error()), "{d:#?}");
    assert!(d.iter().any(|x| x.path == at && x.message.contains("effective band is [-40, -40]")), "{d:#?}");
    assert!(d.iter().any(|x| x.path == at && x.message.contains("plays clamped to -40")), "{d:#?}");
    // A band that holds the authored value is silent.
    let d = diags(&with("  matrix: {", "  safe_bounds: { \"instruments.pad.params.level_db\": { max: -3 } },\n  matrix: {"));
    assert!(d.is_empty(), "{d:#?}");
}

#[test]
fn an_x_prefixed_id_is_checked_like_any_other_id() {
    // `x_` makes a *key* an annotation in a record, but in an id-keyed map it's an id the engine
    // plays — an `x_pad` with a typo must not check clean.
    let d = diags(&with("instruments: { pad:", "instruments: { x_pad: { note_engine: { polyphony: 1 }, params: { wave: \"saw_soft\", cutof_hz: 800 } }, pad:"));
    assert!(has(&d, "instruments.x_pad.params.cutof_hz", "unknown-key"), "{d:#?}");
    let d = diags(&with("lfo: { type: \"lfo\", output: \"control\", rate_hz: 0.1 },", "lfo: { type: \"lfo\", output: \"control\", rate_hz: 0.1 }, x_lfo: { type: \"lfo\", rat_hz: 0.1 },"));
    assert!(has(&d, "modulators.x_lfo.rat_hz", "unknown-key"), "{d:#?}");
    // …while an `x_` key inside a record is still a free annotation, numbers included.
    assert!(diags(&with("cutoff_hz: 800", "cutoff_hz: 800, x_note: 0.5")).is_empty());
}

#[test]
fn a_repeated_key_is_an_error_not_a_silent_drop() {
    // A copy-pasted block whose id wasn't renamed would drop the original: refuse it, located.
    let d = diags(&with("cutoff_hz: 800", "cutoff_hz: 800, cutoff_hz: 900"));
    assert!(has(&d, "instruments.pad.params.cutoff_hz", "duplicate"), "{d:#?}");
    let twice = with("  matrix: {", "  instruments: { other: { note_engine: { polyphony: 1 }, params: { wave: \"sine\" } } },\n  matrix: {");
    let d = diags(&twice);
    assert!(has(&d, "instruments", "duplicate"), "{d:#?}");
    assert!(LoopDoc::from_json_str(&twice).is_err(), "the engine refuses it");
}

#[test]
fn song_layer_routes_get_the_semantic_dest_check() {
    // A layer route to a param the voice never reads does nothing in its block — warned like the
    // same route at top level, located in the layer.
    let song = |route: &str| {
        with(
            "  matrix: {",
            &format!("  song: {{ matrices: {{ deep: {{ {route} }} }}, sequence: [ {{ loops: 4 }}, {{ matrix: \"deep\", loops: 4 }} ], transition_s: 1, tail_loops: 2 }},\n  matrix: {{"),
        )
    };
    let d = diags(&song(r#"m_fm: { source: "lfo", dest: "instruments.pad.params.fm_index", kind: "mod", depth: 0.1 }"#));
    assert!(!d.iter().any(|x| x.path.starts_with("song.matrices.deep.m_fm") && x.is_error()), "plain depth into a 0..1 param: {d:#?}");
    let d2 = diags(&song(r#"m_cut: { depth_db: 3 }"#));
    assert!(has(&d2, "song.matrices.deep.m_cut.depth_db", "invalid"), "a layer's depth key must match the base route's dest: {d2:#?}");
    assert!(has(&d, "song.matrices.deep.m_fm.dest", "no-effect"), "{d:#?}");
    let d = diags(&song(r#"m_cut: { dest: "instruments.pad.params.cutof" }"#));
    assert!(has(&d, "song.matrices.deep.m_cut.dest", "no-effect"), "a patch naming its own dest: {d:#?}");
    let d = diags(&song(r#"m_ghost: { source: "lfo", dest: "instruments.nope.params.cutoff_hz", kind: "mod", depth_oct: 0.664 }"#));
    assert_eq!(d.iter().filter(|x| x.is_error()).count(), 1, "one error for a dangling layer dest, not two: {d:#?}");
    assert!(diags(&song(r#"m_cut: { depth_oct: 1.3 }"#)).iter().all(|x| !x.path.starts_with("song.matrices")), "a depth-only patch keeps the checked base dest");
}

#[test]
fn checker_quick_wins_roots_nan_steps_fractions_and_dotted_ids() {
    // Every spelling the schema pattern admits plays (Cb = B, E# = F, …).
    for r in ["Cb", "Fb", "E#", "B#"] {
        assert!(diags(&with("root: \"D\"", &format!("root: \"{r}\""))).is_empty(), "{r}");
    }
    // A NaN is a number nothing reads: say so.
    let d = diags(&with("level_db: -7.96", "level_db: NaN"));
    assert!(d.iter().any(|x| x.path == "instruments.pad.params.level_db" && x.message.contains("NaN")), "{d:#?}");
    // A note step past `length` never plays.
    let d = diags(&with("steps: [ { step: 0,", "steps: [ { step: 9,"));
    assert!(has(&d, "modulators.seq.steps[0].step", "no-effect"), "{d:#?}");
    // step_duration is whole numbers, as the schema says.
    let d = diags(&with("length: 4,", "length: 4, step_duration: \"1.5/4\","));
    assert!(d.iter().any(|x| x.is_error() && x.path == "modulators.seq.step_duration"), "{d:#?}");
    assert!(diags(&with("length: 4,", "length: 4, step_duration: \"3/16\",")).is_empty());
    // A dotted id can't be a path segment.
    let d = diags(&with("m_cut:", "\"m.cut\":"));
    assert!(has(&d, "matrix.m.cut", "invalid"), "{d:#?}");
}

#[test]
fn minus_infinity_is_silence_on_a_gain_and_refused_anywhere_else() {
    // ADR 0054 P3: the tree keeps `-inf` (`serde_json` made it `null`, so the value read as absent).
    // It's a gain's silence; anywhere else, and `Infinity`/`NaN` anywhere, it's a type error.
    for gain in ["level_db: -Infinity", "level_db: -Infinity, osc_b_level_db: -Infinity"] {
        let c = check_str(&with("level_db: -7.96", gain));
        assert!(c.errors().next().is_none(), "{gain}: {:#?}", c.diags);
    }
    let send = with("pad: { note_engine: { polyphony: 2 },", "pad: { sends_db: { rv: -Infinity }, note_engine: { polyphony: 2 },").replacen("  matrix: {", "  buses: { rv: { return_gain_db: -Infinity } },\n  matrix: {", 1);
    assert!(check_str(&send).errors().next().is_none(), "{:#?}", check_str(&send).diags);
    assert!(check_str(&with("transport: {", "global: { master_gain_db: -Infinity },\n  transport: {")).errors().next().is_none());
    for (from, to, path) in [
        ("cutoff_hz: 800", "cutoff_hz: -Infinity", "instruments.pad.params.cutoff_hz"),
        ("depth_oct: 0.664", "depth_oct: -Infinity", "matrix.m_cut.depth_oct"),
        ("level_db: -7.96", "level_db: Infinity", "instruments.pad.params.level_db"),
        ("level_db: -7.96", "level_db: NaN", "instruments.pad.params.level_db"),
    ] {
        let src = with(from, to);
        let c = check_str(&src);
        let e = c.errors().find(|d| d.path == path).unwrap_or_else(|| panic!("{to}: {:#?}", c.diags));
        assert_eq!(e.code, "type", "{to}");
        assert!(e.message.starts_with("expected a finite number, found ") && e.message.contains(to.rsplit(' ').next().unwrap()), "{to}: {}", e.message);
        assert!(LoopDoc::from_json_str(&src).is_err(), "{to}: the engine refuses it");
    }
    let doc = LoopDoc::from_json_str(&with("level_db: -7.96", "level_db: -Infinity")).unwrap();
    assert_eq!(doc.instruments["pad"].params["level_db"].as_f64(), Some(f64::NEG_INFINITY), "-inf, not null");
}

/// ADR 0054 P7: a modal instrument rings by `t60_s` in `t60_mode: "fixed"` and by `ring_ratio` in
/// `"note"`; the other mode's key is refused.
#[test]
fn a_modal_ring_key_matches_its_mode() {
    let modal = |params: &str| with("pad: { note_engine", &format!("bell: {{ type: \"modal\", note_engine: {{ polyphony: 1 }}, params: {{ {params} }} }}, pad: {{ note_engine"));
    assert!(check_str(&modal("t60_s: 1.2")).errors().next().is_none());
    assert!(check_str(&modal("t60_mode: \"note\", ring_ratio: 1.5")).errors().next().is_none());
    assert!(has(&diags(&modal("ring_ratio: 1.5")), "instruments.bell.params.ring_ratio", "invalid"));
    assert!(has(&diags(&modal("t60_mode: \"note\", t60_s: 1.2")), "instruments.bell.params.t60_s", "invalid"));
}

#[test]
fn minus_infinity_survives_the_docs_tree_edits_and_song_blocks() {
    let mut doc = LoopDoc::from_json_str(BASE).unwrap();
    doc.instruments.get_mut("pad").unwrap().params.insert("level_db".into(), f64::NEG_INFINITY.into());
    let level = |d: &LoopDoc| d.instruments["pad"].params["level_db"].as_f64();
    assert_eq!(level(&doc.with_param("instruments.pad.params.cutoff_hz", 1200.0)), Some(f64::NEG_INFINITY), "a drift step elsewhere keeps it");
    assert_eq!(level(&doc.with_value("instruments.pad.params.wave", "sine".into())), Some(f64::NEG_INFINITY), "so does a categorical move");
    assert_eq!(doc.get_value("instruments.pad.params.level_db").and_then(|v| v.as_f64()), Some(f64::NEG_INFINITY));
    assert_eq!(doc.get_param("instruments.pad.params.level_db"), Some(f32::NEG_INFINITY));

    let song: crate::doc_value::DocValue = serde_json::json!({ "sequence": [{ "loops": 1 }, { "loops": 1, "matrix": "deep" }], "matrices": { "deep": { "m_cut": { "depth_oct": 1.5 } } } }).into();
    doc.song = Some(crate::doc_value::from_doc_value(song).unwrap());
    let spec = doc.song.clone().unwrap();
    let block = crate::song::materialize(&doc, &spec, 1).unwrap();
    assert_eq!((level(&block), block.matrix["m_cut"].depth), (Some(f64::NEG_INFINITY), 1.5), "a song block keeps it");
    let text = crate::doc_value::to_string_pretty(&block).unwrap();
    assert!(text.contains("\"level_db\": -Infinity"), "`--emit-blocks` writes it: {text}");
    let back: crate::doc_value::DocValue = json5::from_str(&text).unwrap();
    assert_eq!(level(&LoopDoc::from_value(back).unwrap()), Some(f64::NEG_INFINITY), "and reads it back");
}

#[test]
fn a_midi_instrument_is_said_to_make_no_sound_in_the_player() {
    // The free player sends no MIDI, and no player sounds a `midi` instrument.
    let src = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  instruments: {
    pad: { note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -10.46 } },
    ext: { type: "midi", note_engine: { polyphony: 1 }, routing: { channel: 2 } },
  },
}"#;
    let c = check_str(src);
    assert!(c.errors().next().is_none(), "{:?}", c.errors().collect::<Vec<_>>());
    let w: Vec<&DocDiag> = c.warnings().filter(|d| d.path == "instruments.ext.type").collect();
    assert_eq!(w.len(), 1, "{:?}", c.warnings().collect::<Vec<_>>());
    assert_eq!(w[0].code, "silent");
    assert!(w[0].message.contains("makes no sound in `engines-live`") && w[0].message.contains("engines-play --midi"), "{}", w[0].message);
    assert!(c.warnings().all(|d| !d.path.starts_with("instruments.pad")), "the audio instrument is fine");
}
