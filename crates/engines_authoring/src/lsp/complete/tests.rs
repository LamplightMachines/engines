use super::*;

/// A small D dorian doc; `‸` marks the cursor in each case (removed before analysis).
const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 8 },
  harmony: { root: "D", mode: "dorian" },
  instruments: {
    pad: { type: "dual", note_engine: { polyphony: 3 }, params: { level_db: -7.96, ‸PARAMS }, sends_db: { ‸SENDS } },
    hiss: { note_engine: { polyphony: 1 }, params: { source: "filtered_noise", ‸NOISE } },
    bin: { type: "binaural", note_engine: { polyphony: 1 }, params: { wave: "sine", ‸PAIR } },
  },
  global: { ‸GLOBAL },
  buses: { rv: { return_gain_db: -6.02, effects: { verb: { type: "reverb", params: { room: 0.8 } } } }, dry: { ‸BUS } },
  modulators: {
    lfo: { type: "lfo", output: "control", rate_hz: 0.05 },
    seq: { type: "step_seq", output: "note", length: 4,
           steps: [ { step: 0, voicing: { v1: { offset: ‸OFFSET }, v2: { offset: 7 } } }, { step: 2, voicing: { ‸VOICE } } ] },
  },
  matrix: {
    m_notes: { source: "‸SOURCE", dest: "instruments.pad.notes", kind: "note", voices: 2 },
    m_cut: { source: "lfo", dest: ‸DEST, kind: "mod", depth_oct: 0.2 },
    m_lvl: { source: "lfo", dest: "instruments.pad.params.level_db", kind: "mod", ‸RKEY },
  },
  movement: { bind: { chord: ‸BIND } },
  song: { matrices: { deep: { m_new: { source: "‸LSOURCE", dest: ‸LDEST, kind: "mod" }, m_notes: { source: ‸LPATCH }, m_lvl: { ‸PKEY } } },
          sequence: [ { loops: 2, control: { ‸CTL } }, { loops: 2, control: { drift: { ‸DRIFT } } } ] },
}"#;

/// The doc with `marker` as the cursor (other markers removed) and the cursor offset.
fn at(marker: &str) -> (Analysis, usize) {
    let mut src = DOC.to_string();
    for m in ["‸PARAMS", "‸SENDS", "‸NOISE", "‸PAIR", "‸OFFSET", "‸VOICE", "‸SOURCE", "‸DEST", "‸BIND", "‸LSOURCE", "‸LDEST", "‸LPATCH", "‸GLOBAL", "‸BUS", "‸RKEY", "‸PKEY", "‸CTL", "‸DRIFT"] {
        if m != marker {
            src = src.replace(m, "");
        }
    }
    let offset = src.find(marker).unwrap();
    src = src.replace(marker, "");
    (Analysis::new(src), offset)
}

fn labels(items: &[Item]) -> Vec<&str> {
    items.iter().map(|i| i.label.as_str()).collect()
}

#[test]
fn param_keys_follow_the_synth_type() {
    let (a, off) = at("‸PARAMS");
    let items = complete(&a, off);
    let l = labels(&items);
    // dual reads osc/fm params and the filter; never modal ones; `level_db` is already there.
    assert!(l.contains(&"fm_index") && l.contains(&"osc_b_ratio") && l.contains(&"cutoff_hz"), "{l:?}");
    assert!(!l.contains(&"material") && !l.contains(&"level_db"), "{l:?}");
    let fm = items.iter().find(|i| i.label == "fm_index").unwrap();
    assert!(fm.insert.starts_with("fm_index: ") && fm.snippet);
    assert!(fm.detail.as_deref().unwrap().starts_with("number 0..1"));
    // A filtered-noise subtractive voice reads the band, not the filter.
    let (a, off) = at("‸NOISE");
    let l2: Vec<String> = complete(&a, off).into_iter().map(|i| i.label).collect();
    assert!(l2.contains(&"band_lo_hz".to_string()) && !l2.contains(&"cutoff_hz".to_string()), "{l2:?}");
    // A binaural pair (ADR 0047) reads the subtractive set plus its real-Hz root and spread.
    let (a, off) = at("‸PAIR");
    let items = complete(&a, off);
    let l3 = labels(&items);
    assert!(["root_hz", "spread_hz", "cutoff_hz", "shift_st", "voices_detune_cents"].iter().all(|k| l3.contains(k)), "{l3:?}");
    assert!(!l3.contains(&"band_lo_hz") && !l3.contains(&"fm_index") && !l3.contains(&"material"), "{l3:?}");
    let spread = items.iter().find(|i| i.label == "spread_hz").unwrap();
    assert!(spread.detail.as_deref().unwrap().starts_with("number 0..40"), "{:?}", spread.detail);
    assert!(!labels(&complete(&at("‸PARAMS").0, at("‸PARAMS").1)).contains(&"root_hz"), "a dual voice has no root_hz");
}

#[test]
fn ids_come_from_the_doc() {
    let (a, off) = at("‸SENDS");
    assert_eq!(labels(&complete(&a, off)), ["rv", "dry"]);
    let (a, off) = at("‸SOURCE");
    // A note route offers note modulators only.
    let items = complete(&a, off);
    assert_eq!(labels(&items), ["seq"]);
    assert_eq!(items[0].insert, "\"seq\"");
    assert_eq!(items[0].detail.as_deref(), Some("step_seq · note"));
    let (a, off) = at("‸BIND");
    assert_eq!(labels(&complete(&a, off)), ["seq"]);
    let (a, off) = at("‸VOICE");
    // The voice ids of the other steps first, then the next free one.
    assert_eq!(labels(&complete(&a, off)), ["v1", "v2", "v3"]);
}

#[test]
fn dest_paths_follow_the_route_kind_and_synth_types() {
    let (a, off) = at("‸DEST");
    let items = complete(&a, off);
    let l = labels(&items);
    assert!(l.contains(&"instruments.pad.params.fm_index"), "{l:?}");
    assert!(l.contains(&"instruments.hiss.params.band_hi_hz") && !l.contains(&"instruments.hiss.params.cutoff_hz"), "{l:?}");
    assert!(l.contains(&"buses.rv.return_gain_db") && l.contains(&"buses.rv.effects.verb.params.predelay_ms"), "{l:?}");
    assert!(l.contains(&"global.master_gain_db"));
    assert!(!l.iter().any(|p| p.ends_with(".notes")), "a mod route offers no note dests");
    let cut = items.iter().find(|i| i.label == "instruments.pad.params.cutoff_hz").unwrap();
    assert_eq!(cut.detail.as_deref(), Some("dual · Hz"));
    let spread = items.iter().find(|i| i.label == "instruments.bin.params.spread_hz").unwrap();
    assert_eq!(spread.detail.as_deref(), Some("binaural · Hz"));
    assert!(l.contains(&"instruments.bin.params.root_hz") && !l.contains(&"instruments.pad.params.root_hz"), "{l:?}");
}

/// A song layer's route entries complete like `matrix` ones: a full new route by its own kind, a
/// patch of a base route by the base route's kind.
#[test]
fn song_layer_routes_complete_sources_and_dests() {
    let (a, off) = at("‸LSOURCE");
    assert_eq!(labels(&complete(&a, off)), ["lfo", "seq"]);
    let (a, off) = at("‸LDEST");
    let items = complete(&a, off);
    let l = labels(&items);
    assert!(l.contains(&"instruments.pad.params.cutoff_hz") && l.contains(&"buses.rv.return_gain_db"), "{l:?}");
    assert!(!l.iter().any(|p| p.ends_with(".notes")), "a mod route offers no note dests");
    let (a, off) = at("‸LPATCH");
    assert_eq!(labels(&complete(&a, off)), ["seq"], "m_notes is a note route in the base matrix");
}

/// A song block's `control` (ADR 0055) completes its keys from the schema, and its `drift`
/// overrides from the doc's param paths, pinned by default.
#[test]
fn song_block_control_completes_keys_and_drift_paths() {
    let (a, off) = at("‸CTL");
    let l: Vec<String> = complete(&a, off).into_iter().map(|i| i.label).collect();
    for k in ["movement", "optimize", "max_drift", "max_change", "drift"] {
        assert!(l.contains(&k.to_string()), "{k}: {l:?}");
    }
    let (a, off) = at("‸DRIFT");
    let items = complete(&a, off);
    let level = items.iter().find(|i| i.label == "instruments.pad.params.level_db").unwrap_or_else(|| panic!("{:?}", labels(&items)));
    assert!(level.insert.ends_with(": \"hold\""), "{}", level.insert);
    assert!(level.detail.as_deref().unwrap().contains("hold"));
}

/// The D dorian offset table: in-scale tones nearest the current value, labelled
/// `note · degree · key`.
#[test]
fn offsets_are_scale_aware() {
    let (a, off) = at("‸OFFSET");
    let items = complete(&a, off);
    let table: Vec<(String, String)> = items.iter().take(7).map(|i| (i.label.clone(), i.detail.clone().unwrap())).collect();
    let want = [
        ("0", "D4 · 1 · D dorian"),
        ("2", "E4 · 2 · D dorian"),
        ("-2", "C4 · ♭7 · D dorian"),
        ("3", "F4 · ♭3 · D dorian"),
        ("-3", "B3 · 6 · D dorian"),
        ("5", "G4 · 4 · D dorian"),
        ("-5", "A3 · 5 · D dorian"),
    ];
    assert_eq!(table, want.map(|(a, b)| (a.to_string(), b.to_string())));
    assert!(items.iter().all(|i| i.kind == ItemKind::Note));
    assert!(!labels(&items).contains(&"4"), "F# is outside D dorian");
    assert_eq!(items.len(), 29); // 7 tones × 4 octaves + the top D
}

#[test]
fn harmony_values_and_enums() {
    let src = r#"{ harmony: { root: "D", mode: "" }, modulators: { x: { type: "" } } }"#;
    let a = Analysis::new(src);
    let items = complete(&a, src.find("mode: \"").unwrap() + 7);
    let dorian = items.iter().find(|i| i.label == "dorian").unwrap();
    assert_eq!(dorian.detail.as_deref(), Some("D E F G A B C · 1 2 ♭3 4 5 6 ♭7"));
    assert_eq!(dorian.replace.text(src), "\"\"");
    let items = complete(&a, src.find("type: \"").unwrap() + 7);
    assert_eq!(labels(&items), ["lfo", "step_seq"]);
}

#[test]
fn top_level_keys_put_required_ones_first() {
    let src = "{\n  \n}";
    let a = Analysis::new(src);
    let mut items = complete(&a, 4);
    items.sort_by(|x, y| x.sort.cmp(&y.sort));
    let first: Vec<&str> = items.iter().take(4).map(|i| i.label.as_str()).collect();
    assert!(first.contains(&"transport") && first.contains(&"harmony"), "{first:?}");
    assert!(items.iter().any(|i| i.label == "song"));
    let tr = items.iter().find(|i| i.label == "transport").unwrap();
    assert_eq!(tr.insert, "transport: { $0 }");
}

/// ADR 0054 U6: a value's snippet is in its unit, and a route offers the one depth key its dest takes.
#[test]
fn snippets_default_in_native_units_and_routes_take_their_depth_key() {
    let insert = |marker: &str, label: &str| {
        let (a, off) = at(marker);
        complete(&a, off).into_iter().find(|i| i.label == label).unwrap_or_else(|| panic!("no {label} at {marker}")).insert
    };
    assert_eq!(insert("‸NOISE", "level_db"), "level_db: ${1:-6.02}", "the engine's default level");
    assert_eq!(insert("‸GLOBAL", "master_gain_db"), "master_gain_db: ${1:0}");
    assert_eq!(insert("‸BUS", "return_gain_db"), "return_gain_db: ${1:0}", "0 dB, not the silence an omitted return is");
    assert_eq!(insert("‸SENDS", "rv"), "rv: ${1:-12}");
    for marker in ["‸RKEY", "‸PKEY"] {
        let (a, off) = at(marker);
        let items = complete(&a, off);
        let l = labels(&items);
        assert!(l.contains(&"depth_db") && l.contains(&"depth_slew_ms"), "{marker}: {l:?}");
        assert!(!l.iter().any(|k| k.starts_with("depth") && *k != "depth_db" && *k != "depth_slew_ms"), "{marker}: only the gain's depth key: {l:?}");
    }
}
