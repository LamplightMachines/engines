use super::edit::{apply, TextEdit};
use super::path::{Seg, Target};
use super::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every tracked JSON / JSON5 doc in the repo (loop docs, templates, schemas).
fn tracked_docs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["contracts", "crates/engines_core/assets", "doc", "lab/tests/fixtures", "sandbox", "."] {
        let Ok(rd) = std::fs::read_dir(repo().join(dir)) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() && p.extension().is_some_and(|x| x == "json5" || x == "json") {
                out.push(p);
            }
        }
    }
    out.sort();
    assert!(out.len() >= 25, "found only {} docs", out.len());
    out
}

fn concat(src: &str, cst: &Cst) -> String {
    cst.tokens.iter().map(|t| t.span.text(src)).collect()
}

#[test]
fn every_tracked_doc_round_trips_losslessly_and_matches_json5() {
    let mut parsed = 0;
    for p in tracked_docs() {
        let src = std::fs::read_to_string(&p).unwrap();
        let cst = Cst::parse(&src);
        assert_eq!(concat(&src, &cst), src, "{}: tokens don't cover the source", p.display());
        match json5::from_str::<Value>(&src) {
            Ok(theirs) => {
                assert!(cst.errors.is_empty(), "{}: {:?}", p.display(), cst.errors);
                assert_eq!(cst.to_value().as_ref(), Some(&theirs), "{}: value differs from json5", p.display());
                parsed += 1;
            }
            // (The objectives template carries `__REVISION__` placeholders.)
            Err(e) => assert!(!cst.errors.is_empty(), "{}: json5 rejects it ({e}), the CST doesn't", p.display()),
        }
    }
    // 24 in the open set, where `sandbox/` and the closed contract are deleted.
    assert!(parsed >= 20, "only {parsed} docs parsed");
}

/// Edge cases of the JSON5 grammar: the CST agrees with `json5` on accept/reject and on the value.
#[test]
fn edge_cases_agree_with_json5() {
    let cases = [
        // numbers
        "{a: 1}", "{a: -1}", "{a: +1}", "{a: -0}", "{a: 0}", "{a: 1.5}", "{a: .5}", "{a: 5.}", "{a: -.5}", "{a: +.5}",
        "{a: 1e3}", "{a: 1E-3}", "{a: 2.5e+2}", "{a: 0x1F}", "{a: -0x10}", "{a: 0XaB}", "{a: Infinity}", "{a: -Infinity}",
        "{a: +Infinity}", "{a: NaN}", "{a: -NaN}", "{a: 18446744073709551615}", "{a: -9223372036854775808}",
        "{a: 1.7976931348623157e308}", "{a: 1e400}",
        // strings
        r#"{a: "x"}"#, "{a: 'x'}", r#"{a: 'it\'s'}"#, r#"{a: "q\"q"}"#, r#"{a: "\b\f\n\r\t\v\0"}"#, r#"{a: "\x41\u00e9"}"#,
        r#"{a: "\ud83d\ude00"}"#, r#"{a: "\q\-"}"#, "{a: \"line\\\ncontinued\"}", "{a: \"crlf\\\r\ncontinued\"}", r#"{a: "♭ é"}"#,
        "{a: \"\u{2028}\"}",
        // keys
        "{$a: 1, _b: 2, c3: 3}", r#"{"quoted key": 1, 'single': 2}"#, r"{\u0061bc: 1}", "{é: 1, ünïcode: 2}", "{true: 1, null: 2}",
        "{a: 1, a: 2}",
        // structure & trivia
        "{}", "[]", "[1, 2, 3,]", "{a: [1, {b: null}],}", "// c\n{/* x */a/* y */:/* z */1 // w\n}", "{a: 1} // tail", "{a: 1} /* unterminated",
        "\u{FEFF}{a: 1}", "{a:\u{A0}1}", "5", "\"top\"", "true",
        // rejects
        "", "{", "{a: 1", "{a 1}", "{a: 1 b: 2}", "{a: 01}", "{a: 1..2}", "{a: 0x}", "{a: -}", "{a: 'x}", "{a: \"x\ny\"}",
        r#"{a: "\1"}"#, r#"{a: "\01"}"#, r#"{a: "\x4"}"#, r#"{a: "\u12"}"#, r#"{a: "\ud83d"}"#, "{a: x}", "{1: 2}", "{a: 1}}", "{a: 1} x",
        "{,}", "{a: 1,,}", "[1 2]", "{a: 18446744073709551616}", "{a: -9223372036854775809}", "{a: /x}", "{a: #}", r"{\u0031: 1}",
    ];
    for src in cases {
        let theirs = json5::from_str::<Value>(src);
        let cst = Cst::parse(src);
        assert_eq!(concat(src, &cst), src, "lossless: {src:?}");
        match theirs {
            Ok(v) => {
                assert!(cst.errors.is_empty(), "{src:?}: json5 accepts, CST says {:?}", cst.errors);
                assert_eq!(cst.to_value(), Some(v), "{src:?}");
                // The engine's tree keeps `±Infinity`/`NaN` as numbers (compared as text: NaN ≠ NaN).
                let ours = cst.to_doc_value().map(|d| d.to_string());
                assert_eq!(ours, json5::from_str::<engines_core::doc_value::DocValue>(src).ok().map(|d| d.to_string()), "{src:?}");
            }
            Err(e) => assert!(!cst.errors.is_empty(), "{src:?}: json5 rejects ({e}), CST accepts as {:?}", cst.to_value()),
        }
    }
}

#[test]
fn a_half_typed_doc_still_has_a_tree() {
    // A key being typed (no colon yet) between two complete members.
    let src = "{ params: { wave: \"sine\", cut\n  resonance: 0.1 } }";
    let cst = Cst::parse(src);
    assert_eq!(cst.errors.len(), 1, "{:?}", cst.errors);
    let params = cst.root.as_ref().unwrap().get("params").unwrap();
    let keys: Vec<&str> = params.members().iter().map(|m| m.key.name.as_str()).collect();
    assert_eq!(keys, ["wave", "cut", "resonance"]);
    assert!(params.member("cut").unwrap().value.is_none());

    // A value not typed yet: the next line's key isn't swallowed as the value.
    let src = "{ cutoff: \n  resonance: 0.1 }";
    let cst = Cst::parse(src);
    let root = cst.root.as_ref().unwrap();
    assert!(root.member("cutoff").unwrap().value.is_none());
    assert_eq!(root.get("resonance").and_then(Node::as_f64), Some(0.1));

    // Missing comma: both members kept; unclosed: closes at the end.
    let cst = Cst::parse("{ a: 1 b: 2");
    let root = cst.root.as_ref().unwrap();
    assert_eq!(root.members().len(), 2);
    assert!(cst.errors.iter().any(|e| e.message.contains("expected `,`")));
    assert!(cst.errors.iter().any(|e| e.message.contains("unclosed")));

    // An unquoted word value is kept (as Invalid) with a quoting hint.
    let cst = Cst::parse("{ wave: saw_soft }");
    assert!(matches!(cst.root.as_ref().unwrap().get("wave").unwrap().kind, NodeKind::Invalid));
    assert!(cst.errors[0].message.contains("\"saw_soft\""), "{:?}", cst.errors);
}

const DOC: &str = r#"// header
{
  harmony: { root: "D", mode: "dorian" }, // the key
  instruments: {
    pad: {
      params: { wave: "saw_soft", cutoff: 0.5 },
    },
  },
  modulators: {
    seq: { type: "step_seq", steps: [ { step: 0, voicing: { v1: { offset: 7 } } }, { step: 2 } ] },
  },
  safe_bounds: { "global.master_gain": { min: 0.2, max: 0.2 }, "global": { min: 0 } },
}
"#;

#[test]
fn paths_resolve_to_nodes_dotted_keys_included() {
    let cst = Cst::parse(DOC);
    let f = cst.find("instruments.pad.params.cutoff");
    assert!(f.complete);
    assert_eq!(f.focus().unwrap().text(DOC), "0.5");
    // A container's focus is its key.
    assert_eq!(cst.find("instruments.pad").focus().unwrap().text(DOC), "pad");
    // Array indices.
    let f = cst.find("modulators.seq.steps[0].voicing.v1.offset");
    assert!(f.complete);
    assert_eq!(f.node.unwrap().as_f64(), Some(7.0));
    assert_eq!(cst.find("modulators.seq.steps[1]").focus().unwrap().text(DOC), "{");
    // The longest key wins: `global.master_gain` over `global`.
    let f = cst.find("safe_bounds.global.master_gain.max");
    assert!(f.complete);
    assert_eq!(f.segs[1], Seg::Key("global.master_gain".into()));
    // An unknown tail stops at the deepest prefix.
    let f = cst.find("instruments.pad.params.cutof");
    assert!(!f.complete);
    assert_eq!(path::render(&f.segs), "instruments.pad.params");
    assert!(cst.find("").complete);
}

#[test]
fn segment_paths_resolve_without_splitting_keys() {
    let cst = Cst::parse(DOC);
    let key = |s: &str| Seg::Key(s.into());
    let f = cst.find_segs(&[key("instruments"), key("pad")]);
    assert!(f.complete);
    assert_eq!(f.member.unwrap().key.span.text(DOC), "pad");
    // A dotted key is one segment — and `global` alone is the other key.
    let f = cst.find_segs(&[key("safe_bounds"), key("global.master_gain"), key("max")]);
    assert!(f.complete);
    assert_eq!(f.node.unwrap().as_f64(), Some(0.2));
    assert_eq!(cst.find_segs(&[key("safe_bounds"), key("global")]).member.unwrap().key.span.text(DOC), "\"global\"");
    // An array item: no member, the item's node.
    let f = cst.find_segs(&[key("modulators"), key("seq"), key("steps"), Seg::Index(1)]);
    assert!(f.complete && f.member.is_none());
    assert_eq!(f.node.unwrap().span.text(DOC), "{ step: 2 }");
    // A missing step stops at the deepest prefix; a key on an array, an index on an object too.
    let f = cst.find_segs(&[key("instruments"), key("nope"), key("params")]);
    assert!(!f.complete);
    assert_eq!(f.segs, [key("instruments")]);
    assert!(!cst.find_segs(&[key("modulators"), Seg::Index(0)]).complete);
    assert!(!cst.find_segs(&[key("modulators"), key("seq"), key("steps"), key("x")]).complete);
    assert!(cst.find_segs(&[]).complete);
}

#[test]
fn the_cursor_knows_what_it_is_on() {
    let cst = Cst::parse(DOC);
    let at = |needle: &str, delta: usize| DOC.find(needle).unwrap() + delta;
    // On a key.
    let c = cst.locate(DOC, at("cutoff", 3));
    assert!(matches!(c.target, Target::Key { member, .. } if member.key.name == "cutoff"));
    assert_eq!(path::render(&c.segs), "instruments.pad.params.cutoff");
    // On a value (inside the string).
    let c = cst.locate(DOC, at("\"saw_soft\"", 2));
    assert!(matches!(c.target, Target::Value { node: Some(n), .. } if n.as_str() == Some("saw_soft")));
    assert_eq!(path::render(&c.segs), "instruments.pad.params.wave");
    // In an array item's object.
    let c = cst.locate(DOC, at("offset: 7", 8));
    assert_eq!(path::render(&c.segs), "modulators.seq.steps[0].voicing.v1.offset");
    // Between members: an object slot.
    let c = cst.locate(DOC, at("pad: {", 6) + 1);
    assert!(matches!(c.target, Target::ObjectSlot { .. }));
    assert_eq!(path::render(&c.segs), "instruments.pad");
    // Between array items.
    let c = cst.locate(DOC, at("} }, {", 4));
    assert!(matches!(c.target, Target::ArraySlot { index: 1, .. }), "{:?}", c.target);
    // In a comment: nothing.
    assert!(matches!(cst.locate(DOC, at("the key", 2)).target, Target::Nothing));
    assert!(matches!(cst.locate(DOC, 3).target, Target::Nothing));
    // After `key: ` with no value yet.
    let src = "{ params: { cutoff: } }";
    let cst = Cst::parse(src);
    let c = cst.locate(src, src.find(": }").unwrap() + 1);
    assert!(matches!(c.target, Target::Value { node: None, member: Some(m) } if m.key.name == "cutoff"), "{:?}", c.target);
    // While typing a new key.
    let src = "{ params: { cutoff: 0.5, re } }";
    let cst = Cst::parse(src);
    let c = cst.locate(src, src.find("re }").unwrap() + 2);
    assert!(matches!(c.target, Target::Key { member, .. } if member.key.name == "re"));
}

fn edited(src: &str, edits: Option<Vec<TextEdit>>) -> String {
    let out = apply(src, &edits.expect("edit applies"));
    let cst = Cst::parse(&out);
    assert!(cst.errors.is_empty(), "edit broke the doc:\n{out}\n{:?}", cst.errors);
    out
}

#[test]
fn edits_keep_every_comment_and_the_layout() {
    let cst = Cst::parse(DOC);
    // Replace a value.
    let out = edited(DOC, cst.replace_value("instruments.pad.params.cutoff", "0.62"));
    assert!(out.contains("cutoff: 0.62 }") && out.contains("// header") && out.contains("// the key"));
    // Rename a key (bare stays bare; a non-identifier gets quoted).
    let out = edited(DOC, cst.rename_key("instruments.pad", "halo"));
    assert!(out.contains("    halo: {"));
    assert!(edited(DOC, cst.rename_key("instruments.pad", "my pad")).contains("\"my pad\": {"));
    assert!(edited(DOC, cst.rename_key("safe_bounds.global", "x")).contains("\"x\": { min: 0 }"));
    // Insert into a multi-line object: a new line at the members' indentation, after the trailing comment.
    let out = edited(DOC, cst.insert_member(DOC, "", "transport", "{ bpm: 60 }"));
    assert!(out.contains("  safe_bounds: {") && out.ends_with("  transport: { bpm: 60 },\n}\n"), "{out}");
    let out = edited(DOC, cst.insert_member(DOC, "instruments.pad", "sends", "{ rv: 0.4 }"));
    assert!(out.contains("      params: { wave: \"saw_soft\", cutoff: 0.5 },\n      sends: { rv: 0.4 },\n    },"), "{out}");
    // …after a trailing line comment, not before it.
    let src = "{\n  a: 1, // one\n  b: 2 // two\n}";
    let out = edited(src, Cst::parse(src).insert_member(src, "", "c", "3"));
    assert_eq!(out, "{\n  a: 1, // one\n  b: 2, // two\n  c: 3,\n}");
    // Insert into a one-line object, an empty one, an empty multi-line one.
    let out = edited(DOC, cst.insert_member(DOC, "instruments.pad.params", "level", "0.3"));
    assert!(out.contains("{ wave: \"saw_soft\", cutoff: 0.5, level: 0.3 }"), "{out}");
    assert_eq!(edited("{a: {}}", Cst::parse("{a: {}}").insert_member("{a: {}}", "a", "b", "1")), "{a: { b: 1 }}");
    let src = "{\n  a: {\n  },\n}";
    assert_eq!(edited(src, Cst::parse(src).insert_member(src, "a", "b", "1")), "{\n  a: {\n    b: 1,\n  },\n}");
    // set_member replaces an existing value.
    let out = edited(DOC, cst.set_member(DOC, "harmony", "mode", "\"aeolian\""));
    assert!(out.contains("mode: \"aeolian\" }, // the key"));
    // Remove a member on its own line (its line goes, the comments around stay).
    let src = "{\n  // about a\n  a: 1, // one\n  b: 2,\n}";
    assert_eq!(edited(src, Cst::parse(src).remove_member(src, "a")), "{\n  // about a\n  b: 2,\n}");
    // Remove inline members: first, middle, last, only.
    let src = "{ a: 1, b: 2, c: 3 }";
    let cst2 = Cst::parse(src);
    assert_eq!(edited(src, cst2.remove_member(src, "a")), "{ b: 2, c: 3 }");
    assert_eq!(edited(src, cst2.remove_member(src, "b")), "{ a: 1, c: 3 }");
    assert_eq!(edited(src, cst2.remove_member(src, "c")), "{ a: 1, b: 2 }");
    assert_eq!(edited("{ a: 1 }", Cst::parse("{ a: 1 }").remove_member("{ a: 1 }", "a")), "{}");
    // A path that doesn't exist edits nothing.
    assert!(cst.replace_value("instruments.nope", "1").is_none());
}

#[test]
fn inserted_lines_use_the_docs_line_ending() {
    for nl in ["\r\n", "\r"] {
        // A multi-line object (a CR-only one too: it spans lines), and an empty multi-line one.
        let src = "{\n  a: 1, // one\n  b: { /* none yet */\n  },\n}".replace('\n', nl);
        let cst = Cst::parse(&src);
        let out = edited(&src, cst.insert_member(&src, "", "c", "3"));
        assert_eq!(out, "{\n  a: 1, // one\n  b: { /* none yet */\n  },\n  c: 3,\n}".replace('\n', nl), "{nl:?}");
        let src = "{\n  a: {\n  },\n}".replace('\n', nl);
        let out = edited(&src, Cst::parse(&src).insert_member(&src, "a", "b", "1"));
        assert_eq!(out, "{\n  a: {\n    b: 1,\n  },\n}".replace('\n', nl), "{nl:?}");
    }
    // A same-line block comment trails its member: the new one goes after it.
    let src = "{\n  a: 1, /* one */\n}";
    assert_eq!(edited(src, Cst::parse(src).insert_member(src, "", "b", "2")), "{\n  a: 1, /* one */\n  b: 2,\n}");
}

#[test]
fn to_json5_writes_house_style() {
    let v: Value = serde_json::json!({ "type": "lfo", "rate_hz": 0.05, "my key": [1, "x"] });
    let s = to_json5(&v);
    assert_eq!(json5::from_str::<Value>(&s).unwrap(), v);
    assert!(s.contains("type: \"lfo\"") && s.contains("\"my key\": [1, \"x\"]"), "{s}");
}

#[test]
fn token_at_prefers_the_token_starting_at_the_cursor() {
    let src = "{ a: 1 }";
    let cst = Cst::parse(src);
    assert_eq!(cst.token_at(2).unwrap().1.kind, TokenKind::Word); // between ` ` and `a`
    assert_eq!(cst.token_at(3).unwrap().1.kind, TokenKind::Word); // right after `a`
}

#[test]
fn member_order_is_the_texts_order() {
    let src = "{ instruments: { sub: {}, crank: {}, 'partials': {}, sub: {} }, buses: { rv: {}, dly: {} } }";
    assert_eq!(super::member_order(src, "instruments"), ["sub", "crank", "partials"]);
    assert_eq!(super::member_order(src, "buses"), ["rv", "dly"]);
    assert!(super::member_order("{ global: {} }", "instruments").is_empty());
    assert!(super::member_order("not json5", "instruments").is_empty());
}
