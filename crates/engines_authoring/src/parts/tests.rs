use super::*;
use crate::cst::Node;
use std::path::Path;

fn repo(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The docs every part must merge into cleanly: the house-style test drone and song, the
/// annotated template, and two
/// fixtures written as strict JSON (quoted keys, another layout).
fn reference_docs() -> Vec<(&'static str, String)> {
    [
        "lab/tests/fixtures/loop_drone_d_lydian.json5",
        "lab/tests/fixtures/song_lydian_tide.json5",
        "doc/loop_contract_v0.9.json5",
        "lab/tests/fixtures/loop_orrery_dorian.json",
        "lab/tests/fixtures/loop_buses_shimmer.json",
    ]
    .into_iter()
    .map(|p| (p, repo(p)))
    .collect()
}

/// A small D dorian doc in the house layout, with a bus.
const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 8 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: {
    // the only voice
    pad: {
      note_engine: { polyphony: 2 },
      params: { wave: "saw_soft", level_db: -10.46, cutoff_hz: 800 },
      sends_db: { rv: -7.96 },
    },
  },
  modulators: {
    seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] },
  },
  matrix: {
    m_pad: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 }, // the pad's notes
  },
  buses: {
    rv: { return_gain_db: -7.96, effects: { verb: { type: "reverb", params: { room: 0.7 } } } },
  },
}
"#;

fn comments(src: &str) -> Vec<String> {
    Cst::parse(src).tokens.iter().filter(|t| matches!(t.kind, TokenKind::LineComment | TokenKind::BlockComment)).map(|t| t.span.text(src).to_string()).collect()
}

/// `a` is a subsequence of `b`.
fn subsequence(a: &[String], b: &[String]) -> bool {
    let mut it = b.iter();
    a.iter().all(|x| it.any(|y| y == x))
}

/// Every object's keys are unique.
fn unique_keys(node: &Node, path: &str) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for m in node.members() {
        if !seen.insert(&m.key.name) {
            return Err(format!("{path}: `{}` twice", m.key.name));
        }
        if let Some(v) = &m.value {
            unique_keys(v, &format!("{path}.{}", m.key.name))?;
        }
    }
    for (i, it) in node.items().iter().enumerate() {
        unique_keys(&it.node, &format!("{path}[{i}]"))?;
    }
    Ok(())
}

/// Everything a merged doc must be: parses (CST and the engine's json5), checks clean, kept every
/// comment of `base`, unique keys everywhere, the added paths exist, and the editor's diff
/// reproduces it.
fn assert_sound(what: &str, base: &str, a: &Added) {
    let cst = Cst::parse(&a.text);
    assert!(cst.errors.is_empty(), "{what}: syntax {:?}\n{}", cst.errors, a.text);
    assert!(json5::from_str::<serde_json::Value>(&a.text).is_ok(), "{what}: json5 refuses it");
    let errors: Vec<String> = check_str(&a.text).diags.iter().filter(|d| d.is_error()).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{what}: {errors:#?}");
    assert!(subsequence(&comments(base), &comments(&a.text)), "{what}: a comment of the doc went missing");
    unique_keys(cst.root.as_ref().unwrap(), "$").unwrap_or_else(|e| panic!("{what}: {e}"));
    for p in &a.added {
        assert!(cst.find(p).complete, "{what}: `{p}` isn't in the result");
    }
    assert_eq!(apply(base, &diff_edits(base, &a.text)), a.text, "{what}: the diff doesn't reproduce it");
}

#[test]
fn the_library_is_well_formed() {
    let parts = library();
    let names: Vec<&str> = parts.iter().map(|p| &*p.name).collect();
    assert_eq!(names, ["bass", "binaural", "breath", "drone", "halo", "lead", "partials", "pulse", "shimmer"]);
    let readme = repo("crates/engines_authoring/assets/parts/README.md");
    let key = Key::new("D", "dorian");
    for p in &parts {
        assert!(!p.summary.is_empty() && p.summary.len() < 110, "{}: a one-line summary", p.name);
        let text = instantiate(&p.source, "x", Some("rv"), Some("pad"), key.as_ref()).unwrap_or_else(|e| panic!("{}: {e}", p.name));
        let cst = Cst::parse(&text);
        assert!(cst.errors.is_empty(), "{}: {:?}", p.name, cst.errors);
        let root = cst.root.unwrap();
        for m in root.members() {
            assert!(["part", "patch"].contains(&m.key.name.as_str()) || SECTIONS.contains(&m.key.name.as_str()), "{}: unknown section `{}`", p.name, m.key.name);
        }
        assert!(matches!(p.target.as_deref(), None | Some("any")) || ["cutoff_hz", "level_db", "resonance"].contains(&p.target.as_deref().unwrap()));
        let _ = readme.as_str();
    }
}

#[test]
fn every_part_merges_into_every_reference_doc() {
    for (doc_name, doc) in reference_docs() {
        for part in library() {
            let what = format!("{} into {doc_name}", part.name);
            let a = add(&doc, &part, &Options::default()).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_sound(&what, &doc, &a);
            assert!(a.warnings.iter().all(|w| !w.is_error()));
            // Again: a fresh id, still sound.
            let b = add(&a.text, &part, &Options::default()).unwrap_or_else(|e| panic!("{what} twice: {e}"));
            assert_ne!(a.id, b.id, "{what}: the second copy gets its own id");
            assert!(b.id.starts_with(&a.id) || b.id.starts_with(&*part.name), "{what}: {} → {}", a.id, b.id);
            assert_sound(&format!("{what} twice"), &a.text, &b);
        }
    }
}

/// A part whose offsets are raw semitones in `key` (what `engines-midi parts` writes, ADR 0053).
fn keyed(key: &str) -> Result<Part, String> {
    from_source(
        "cell",
        format!(
            "{{\n  part: {{ summary: \"a keyed cell\", key: \"{key}\" }},\n  sequence_pools: {{\n    $id: {{ fit: \"literal\", length: 4, notes: [ {{ t: 0, offset: 3 }} ] }},\n  }},\n}}\n"
        ),
    )
}

#[test]
fn a_keyed_part_joins_only_a_doc_in_its_key() {
    // DOC is in D dorian.
    let a = add(DOC, &keyed("D dorian").unwrap(), &Options::default()).unwrap();
    assert_sound("keyed, same key", DOC, &a);
    // The same notes on another root: offsets from A would land a fifth away.
    let e = add(DOC, &keyed("A aeolian").unwrap(), &Options::default()).unwrap_err();
    assert!(e.contains("written in A aeolian, and the doc is in D dorian") && e.contains("same notes on another root") && e.contains("--key \"D dorian\""), "{e}");
    // Same root, other notes: out of key.
    let e = add(DOC, &keyed("D mixolydian").unwrap(), &Options::default()).unwrap_err();
    assert!(e.contains("written in D mixolydian") && !e.contains("same notes"), "{e}");
    // No harmony to check against.
    let bare = DOC.replace("  harmony: { root: \"D\", mode: \"dorian\" },\n", "");
    let e = add(&bare, &keyed("D dorian").unwrap(), &Options::default()).unwrap_err();
    assert!(e.contains("needs `harmony.root`"), "{e}");
    // Not a key at all: refused when it's read.
    assert!(keyed("H lydian").unwrap_err().contains("isn't a key"));
    assert!(keyed("dorian").unwrap_err().contains("isn't a key"));
    // The library's parts carry no key.
    assert!(library().iter().all(|p| p.key.is_none()));
}

#[test]
fn parts_fit_the_doc_layout() {
    let a = add(DOC, &get("pulse").unwrap(), &Options::default()).unwrap();
    assert_sound("pulse", DOC, &a);
    assert_eq!((a.id.as_str(), a.bus.as_deref()), ("pulse", Some("rv")));
    // Appended after the last member, at the members' indentation, with its comments and a blank
    // line before a commented block.
    assert!(a.text.contains("      sends_db: { rv: -7.96 },\n    },\n\n    // The pluck: three resonator modes"), "{}", a.text);
    assert!(a.text.contains("\n    pulse: {\n      enabled: true, pitched: true, type: \"modal\",\n"));
    assert!(a.text.contains("      sends_db: { rv: -12.04 },\n    },\n  },\n  modulators: {"), "the instrument closes its section: {}", a.text);
    // The doc's trailing comment stays on its line; the new route goes after it.
    assert!(a.text.contains("voices: 1 }, // the pad's notes\n"));
    let block = DOC.replace("// the pad's notes", "/* the pad's notes */");
    let b = add(&block, &get("pulse").unwrap(), &Options::default()).unwrap();
    assert_sound("pulse, a block comment", &block, &b);
    assert!(b.text.contains("voices: 1 }, /* the pad's notes */\n"), "a same-line block comment trails too: {}", b.text);
    // `sequence_pools` didn't exist: created after `modulators`, in the doc's layout.
    let pools = a.text.find("\n\n  sequence_pools: {\n").expect("a new section");
    assert!(a.text.find("\n  modulators: {").unwrap() < pools && pools < a.text.find("\n  matrix: {").unwrap());
    // No `safe_bounds` in the doc ⇒ the part's frozen level isn't added.
    assert!(!a.text.contains("safe_bounds"));
}

#[test]
fn degrees_follow_the_mode() {
    let a = add(DOC, &get("halo").unwrap(), &Options::default()).unwrap();
    // D dorian: the fifth below (−5), the root, the minor third (3), the fifth (7).
    for (v, o) in [("v1", -5), ("v2", 0), ("v3", 3), ("v4", 7)] {
        assert!(a.text.contains(&format!("{v}: {{ offset: {o} }}")), "{v} {o}: {}", a.text);
    }
    let lydian = DOC.replace("dorian", "lydian");
    let a = add(&lydian, &get("halo").unwrap(), &Options::default()).unwrap();
    assert!(a.text.contains("v3: { offset: 4 }"), "the major third in lydian");
    let drone = add(DOC, &get("drone").unwrap(), &Options::default()).unwrap();
    assert!(drone.text.contains("d: { offset: -24 }"), "$deg(1, -2)");
    assert_eq!(fill_degrees("x: \"$deg(9)\", y: '$deg(2, 1)'", Key::new("C", "major").as_ref()).unwrap(), "x: 14, y: 14");
    assert!(fill_degrees("\"$deg(0)\"", Key::new("C", "major").as_ref()).is_err());
    assert!(fill_degrees("\"$deg(1)\"", None).is_err(), "no key, no degrees");
}

#[test]
fn a_doc_without_a_bus_drops_the_send() {
    let start = DOC.find("  buses: {").unwrap();
    let end = DOC[start..].find("\n  },\n").unwrap() + start + 5;
    let no_bus = format!("{}{}", &DOC[..start], &DOC[end..]).replace("      sends_db: { rv: -7.96 },\n", "");
    assert!(check_str(&no_bus).errors().next().is_none(), "{no_bus}");
    let a = add(&no_bus, &get("pulse").unwrap(), &Options::default()).unwrap();
    assert_sound("pulse, no bus", &no_bus, &a);
    assert_eq!(a.bus, None);
    assert!(a.text.contains("sends_db: {}") && !a.text.contains('$'), "{}", a.text);
}

#[test]
fn sends_never_default_to_an_external_return_bus() {
    // A DAW return first: it takes no sends, so `$bus` goes to the first bus that does.
    let daw = DOC.replace("  buses: {\n", "  buses: {\n    daw: { type: \"external_return\", return_gain_db: -1.94 },\n");
    assert!(check_str(&daw).errors().next().is_none(), "{:?}", check_str(&daw).diags);
    let a = add(&daw, &get("pulse").unwrap(), &Options::default()).unwrap();
    assert_sound("pulse, a DAW return first", &daw, &a);
    assert_eq!(a.bus.as_deref(), Some("rv"));
    // …so the editor still offers every part.
    assert_eq!(crate::lsp::actions::add_part_actions(&crate::lsp::Analysis::new(daw.as_str()), 0).len(), library().len());
    // Asked for by name: refused, saying why.
    let e = add(&daw, &get("pulse").unwrap(), &Options { bus: Some("daw".into()), ..Default::default() }).unwrap_err();
    assert!(e.contains("`daw` is an external_return bus — it takes no sends"), "{e}");
    // Only a DAW return: the sends are dropped, as with no bus at all.
    let only = daw.replace("    rv: { return_gain_db: -7.96, effects: { verb: { type: \"reverb\", params: { room: 0.7 } } } },\n", "").replace("      sends_db: { rv: -7.96 },\n", "");
    assert!(check_str(&only).errors().next().is_none(), "{:?}\n{only}", check_str(&only).diags);
    let a = add(&only, &get("pulse").unwrap(), &Options::default()).unwrap();
    assert_sound("pulse, only a DAW return", &only, &a);
    assert_eq!(a.bus, None);
    assert!(a.text.contains("sends_db: {}") && !a.text.contains('$'), "{}", a.text);
}

#[test]
fn errors_the_doc_already_has_are_reported_not_blamed_on_the_part() {
    // The editor adds a part to a buffer with an unrelated mistake (nothing is written; the
    // squiggles say what to fix) — `engines-part` refuses on `existing_errors` instead.
    let typo = DOC.replace("cutoff_hz: 800", "cutof_hz: 800");
    let a = add(&typo, &get("drone").unwrap(), &Options::default()).unwrap();
    assert_eq!(a.existing_errors.len(), 1);
    assert!(a.existing_errors[0].to_string().contains("cutof"));
    assert!(a.text.contains("drone_note"));
    assert!(add(DOC, &get("drone").unwrap(), &Options::default()).unwrap().existing_errors.is_empty());
}

#[test]
fn an_existing_error_whose_hint_changes_is_still_existing() {
    // `pulsee` has no near id until a `pulse` is added — then the same error suggests it.
    let typo = DOC.replace("\"instruments.pad.notes\"", "\"instruments.pulsee.notes\"");
    let before: Vec<String> = check_str(&typo).errors().map(|d| d.to_string()).collect();
    assert!(before.len() == 1 && before[0].contains("unknown instrument `pulsee`") && !before[0].contains("did you mean"), "{before:?}");
    let a = add(&typo, &get("pulse").unwrap(), &Options::default()).unwrap_or_else(|e| panic!("blamed on the part: {e}"));
    assert_eq!(a.existing_errors.len(), 1);
    assert!(a.existing_errors[0].to_string().contains("did you mean `pulse`"), "{:?}", a.existing_errors);
    assert!(a.warnings.iter().all(|w| !w.is_error()));
    // …so the editor still offers it, like every other part.
    assert_eq!(crate::lsp::actions::add_part_actions(&crate::lsp::Analysis::new(typo.as_str()), 0).len(), library().len());
    // Doc-wide findings (no path) are told apart by their message; a second copy of one is fresh.
    let lint = |m: &str| DocDiag::warning("", "lint", m);
    assert!(same_finding(&lint("a"), &lint("a")) && !same_finding(&lint("a"), &lint("b")));
    let dangling = |m: &str| DocDiag::error("matrix.m.dest", "dangling", m);
    assert!(same_finding(&dangling("unknown instrument `x`"), &dangling("unknown instrument `x` — did you mean `y`?")));
    assert!(!same_finding(&dangling("x"), &DocDiag::warning("matrix.m.dest", "dangling", "x")));
}

#[test]
fn targets_buses_and_ids_can_be_chosen() {
    let breath = get("breath").unwrap();
    let a = add(DOC, &breath, &Options { target: Some("pad".into()), id: Some("sway".into()), ..Default::default() }).unwrap();
    assert!(a.text.contains("m_sway: { source: \"sway\", dest: \"instruments.pad.params.cutoff_hz\""), "{}", a.text);
    assert!(a.text.contains("// ±0.66 oct on pad's cutoff_hz"), "the placeholder in a comment reads naturally too");
    assert_eq!(a.bus, None, "breath sends nowhere");
    // An id that's taken, or isn't one; a target or bus the doc lacks; a target without the param.
    let e = |o: Options| add(DOC, &breath, &o).unwrap_err();
    assert!(e(Options { id: Some("seq".into()), ..Default::default() }).contains("taken"));
    assert!(e(Options { id: Some("2x".into()), ..Default::default() }).contains("can't be an id"));
    assert!(e(Options { target: Some("nope".into()), ..Default::default() }).contains("no instrument `nope`"));
    assert!(add(DOC, &get("pulse").unwrap(), &Options { bus: Some("verb".into()), ..Default::default() }).unwrap_err().contains("no bus `verb`"));
    let modal = DOC.replace("params: { wave: \"saw_soft\", level_db: -10.46, cutoff_hz: 800 }", "params: { level_db: -10.46 }").replace("note_engine", "type: \"modal\", note_engine");
    assert!(check_str(&modal).errors().next().is_none(), "{:?}", check_str(&modal).diags);
    assert!(add(&modal, &breath, &Options::default()).unwrap_err().contains("needs an instrument with a `cutoff_hz`; the doc has none"));
    assert!(add(&modal, &breath, &Options { target: Some("pad".into()), ..Default::default() }).unwrap_err().contains("`pad` has none"));
    assert!(add("{ broken: ", &breath, &Options::default()).unwrap_err().contains("syntax error"));
}

#[test]
fn a_patch_sets_values_in_existing_objects() {
    let a = add(DOC, &get("shimmer").unwrap(), &Options::default()).unwrap();
    assert_sound("shimmer", DOC, &a);
    assert_eq!(a.target.as_deref(), Some("pad"));
    assert!(a.text.contains("sends_db: { rv: -7.96, shimmer: -7.96 }"), "{}", a.text);
    // An instrument without `sends_db` gets one.
    let dry = DOC.replace("      sends_db: { rv: -7.96 },\n", "");
    let a = add(&dry, &get("shimmer").unwrap(), &Options::default()).unwrap();
    assert_sound("shimmer, dry pad", &dry, &a);
    assert!(a.text.contains("sends_db: { shimmer: -7.96 }"), "{}", a.text);
}

#[test]
fn a_crlf_doc_stays_crlf() {
    let crlf = DOC.replace('\n', "\r\n");
    // The pad without `sends_db`: shimmer's patch opens a new multi-line member in it.
    let dry = DOC.replace("      sends_db: { rv: -7.96 },\n", "").replace('\n', "\r\n");
    for (what, doc) in [("CRLF", &crlf), ("CRLF, dry pad", &dry)] {
        for part in library() {
            let what = format!("{} into a {what} doc", part.name);
            let a = add(doc, &part, &Options::default()).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_sound(&what, doc, &a);
            assert_eq!(a.text.matches('\n').count(), a.text.matches("\r\n").count(), "{what}: a bare LF in {:?}", a.text);
            assert_eq!(a.text.matches('\r').count(), a.text.matches("\r\n").count(), "{what}: a bare CR in {:?}", a.text);
        }
    }
    // The case found in review: `add drone` into a CRLF doc left LF-only lines among the CRLF ones.
    let a = add(&crlf, &get("drone").unwrap(), &Options::default()).unwrap();
    assert!(check_str(&a.text).errors().next().is_none());
    assert!(a.text.split_inclusive('\n').all(|l| l.ends_with("\r\n")), "{:?}", a.text);
    assert!(a.text.contains("\r\n\r\n    // "), "the blank line before a commented block is CRLF too: {:?}", a.text);
}

#[test]
fn one_line_and_empty_sections() {
    let one_line = r#"{ schema_version: "0.9", transport: { bpm: 60, master_steps: 4 }, harmony: { root: "A", mode: "aeolian" }, instruments: { pad: { note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -10.46 } } }, modulators: {}, matrix: { m: { source: "s", dest: "instruments.pad.notes", kind: "note", voices: 1 } } }"#;
    let one_line = one_line.replace("modulators: {}", r#"modulators: { s: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v: { offset: 0 } } } ] } }"#);
    for part in library() {
        let a = add(&one_line, &part, &Options::default()).unwrap_or_else(|e| panic!("{}: {e}", part.name));
        assert_sound(&format!("{} one-line", part.name), &one_line, &a);
        assert_eq!(a.text.lines().count(), 1, "{}: stays one line: {}", part.name, a.text);
    }
    let empty = DOC.replace("  modulators: {\n    seq: { type: \"step_seq\", output: \"note\", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] },\n  },", "  modulators: {},").replace("source: \"seq\"", "source: \"lfo\"");
    let empty = empty.replace("  matrix: {\n    m_pad: { source: \"lfo\", dest: \"instruments.pad.notes\", kind: \"note\", voices: 1 }, // the pad's notes\n  },", "  matrix: {\n  },");
    assert!(check_str(&empty).errors().next().is_none(), "{:?}\n{empty}", check_str(&empty).diags);
    let a = add(&empty, &get("drone").unwrap(), &Options::default()).unwrap();
    assert_sound("drone, empty sections", &empty, &a);
    assert!(a.text.contains("  modulators: {\n    // One held note"), "{}", a.text);
    assert!(a.text.contains("  matrix: {\n    // The drone's note route.\n    m_drone:"), "{}", a.text);
}

#[test]
fn safe_bounds_follow_the_doc() {
    let frozen = DOC.replace("  global: { master_gain_db: -6.02 },\n", "  global: { master_gain_db: -6.02 },\n  safe_bounds: {\n    \"global.master_gain_db\": { min: -6.02, max: -6.02 },\n  },\n");
    let a = add(&frozen, &get("pulse").unwrap(), &Options::default()).unwrap();
    assert_sound("pulse, frozen doc", &frozen, &a);
    assert!(a.text.contains("    \"global.master_gain_db\": { min: -6.02, max: -6.02 },\n    \"instruments.pulse.params.level_db\": { min: -13.98, max: -13.98 },\n"), "{}", a.text);
    // A pulse removed by hand leaves its band behind (only a warning: unused); adding pulse again
    // keeps that authored band — no second key for json5 to silently let win.
    let left = frozen.replace("    \"global.master_gain_db\": { min: -6.02, max: -6.02 },\n", "    \"global.master_gain_db\": { min: -6.02, max: -6.02 },\n    \"instruments.pulse.params.level_db\": { min: -10.46, max: -10.46 },\n");
    assert!(check_str(&left).errors().next().is_none(), "{:?}", check_str(&left).diags);
    let a = add(&left, &get("pulse").unwrap(), &Options::default()).unwrap();
    assert_eq!(a.id, "pulse");
    assert_sound("pulse, a band left behind", &left, &a);
    assert_eq!(a.text.matches("\"instruments.pulse.params.level_db\"").count(), 1, "{}", a.text);
    assert!(a.text.contains("\"instruments.pulse.params.level_db\": { min: -10.46, max: -10.46 }"), "{}", a.text);
    assert!(!a.added.iter().any(|p| p.starts_with("safe_bounds.")), "{:?}", a.added);
}

#[test]
fn diff_edits_are_minimal() {
    let old = "a\nb\nc\nd\n";
    for new in ["a\nb\nX\nc\nd\n", "a\nc\nd\n", "Z\na\nb\nc\nd\nY\n", "a\nB\nc\nd", "", "a\nb\nc\nd\n"] {
        let edits = diff_edits(old, new);
        assert_eq!(apply(old, &edits), new, "{new:?}");
    }
    let e = diff_edits(old, "a\nb\nX\nc\nd\n");
    assert_eq!(e, vec![TextEdit::new(crate::cst::Span::new(4, 4), "X\n")], "one insertion");
    assert!(diff_edits(old, old).is_empty());
}

/// The engine renders each house piece with each part in it — raw, and from the controller's
/// operating point (what plays) — without clipping (no master limiter, ADR 0021). At 24 kHz: a
/// debug build renders slowly, and the parts' energy sits far below either Nyquist.
#[test]
fn parts_render_without_clipping() {
    use engines_core::controller::Controller;
    use engines_core::engine::{Engine, PhaseSnapshot};
    use engines_core::loop_doc::LoopDoc;
    let peak = |doc: &LoopDoc| {
        let r = Engine::new(24_000.0).render(doc, &PhaseSnapshot::empty());
        assert!(r.audio.iter().all(|x| x.is_finite()));
        r.audio.iter().fold(0.0f32, |a, &b| a.max(b.abs()))
    };
    let docs = reference_docs().into_iter().filter(|(p, _)| p.contains("/house/")).collect::<Vec<_>>();
    std::thread::scope(|s| {
        for (doc_name, doc) in &docs {
            for part in library() {
                s.spawn(move || {
                    let a = add(doc, &part, &Options::default()).unwrap();
                    let mut d = LoopDoc::from_json_str(&a.text).unwrap();
                    d.song = None;
                    let raw = peak(&d);
                    let live = peak(Controller::new(d, Default::default()).operating_point());
                    assert!(raw <= 1.0 && live <= 1.0, "{} in {doc_name} clips: raw {raw:.3}, as played {live:.3} FS", part.name);
                    eprintln!("{:>9} in {doc_name}: peak {raw:.3} raw, {live:.3} as played", part.name);
                });
            }
        }
    });
}
