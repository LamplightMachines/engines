use super::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Every shape serde can hand a serializer, finite throughout.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Shapes {
    a: f32,
    b: f64,
    i: i64,
    u: u8,
    s: String,
    c: char,
    o: Option<f32>,
    none: Option<String>,
    unit: (),
    v: Vec<f32>,
    empty_v: Vec<u8>,
    m: BTreeMap<String, f64>,
    empty_m: BTreeMap<String, f64>,
    int_keys: BTreeMap<u32, String>,
    e: Vec<E>,
    nested: Inner,
    t: (u8, String),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Inner {
    #[serde(skip_serializing_if = "Option::is_none")]
    gone: Option<u8>,
    deep: Vec<Vec<BTreeMap<String, f32>>>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum E {
    Unit,
    Newtype(f32),
    Tuple(u8, String),
    Struct { x: f32, y: Vec<u8> },
    Empty {},
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Tagged {
    Lfo { rate_hz: f32 },
    Step { values: Vec<f32> },
}

fn shapes() -> Shapes {
    Shapes {
        a: 0.3,
        b: 1e-7,
        i: -42,
        u: 7,
        s: "quote \" and \\ and \u{1} and é".into(),
        c: '\n',
        o: Some(0.1),
        none: None,
        unit: (),
        v: vec![0.0, -0.5, 1.0, 123456.79],
        empty_v: vec![],
        m: BTreeMap::from([("z".into(), 2.5), ("a".into(), -0.0)]),
        empty_m: BTreeMap::new(),
        int_keys: BTreeMap::from([(3, "c".into()), (10, "ten".into())]),
        e: vec![E::Unit, E::Newtype(0.25), E::Tuple(1, "x".into()), E::Struct { x: 2.0, y: vec![] }, E::Empty {}],
        nested: Inner { gone: None, deep: vec![vec![], vec![BTreeMap::from([("k".into(), 0.7)])]] },
        t: (9, "nine".into()),
    }
}

/// The repo's docs, as the engine reads them: the template and every lab fixture.
fn repo_docs() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = vec![("template".to_string(), crate::contract::LOOP_TEMPLATE_JSON5.to_string())];
    let mut fixtures: Vec<_> = std::fs::read_dir(root.join("lab/tests/fixtures")).unwrap().map(|e| e.unwrap().path()).collect();
    fixtures.sort();
    for p in fixtures.into_iter().filter(|p| p.extension().is_some_and(|e| e == "json" || e == "json5")) {
        let text = std::fs::read_to_string(&p).unwrap();
        if crate::loop_doc::LoopDoc::from_json_str(&text).is_ok() {
            out.push((p.display().to_string(), text));
        }
    }
    assert!(out.len() > 10, "found the fixtures");
    out
}

#[test]
fn writes_exactly_what_serde_json_writes_for_finite_values() {
    let s = shapes();
    assert_eq!(to_string(&s).unwrap(), serde_json::to_string(&s).unwrap());
    assert_eq!(to_string_pretty(&s).unwrap(), serde_json::to_string_pretty(&s).unwrap());
    for t in [Tagged::Lfo { rate_hz: 0.05 }, Tagged::Step { values: vec![] }] {
        assert_eq!(to_string_pretty(&t).unwrap(), serde_json::to_string_pretty(&t).unwrap());
    }
    let v = json!({ "a": [], "b": {}, "c": [[], {}, [1, [2, {}]]], "d": null, "e": 1.5, "f": -3 });
    assert_eq!(to_string_pretty(&v).unwrap(), serde_json::to_string_pretty(&v).unwrap());
    assert_eq!(to_string(&v).unwrap(), serde_json::to_string(&v).unwrap());
    for (name, text) in repo_docs() {
        let doc = crate::loop_doc::LoopDoc::from_json_str(&text).unwrap();
        if text.contains("-Infinity") {
            // …where serde_json writes `null`, the writer says `-Infinity`; the rest is identical.
            let ours = to_string(&doc).unwrap();
            assert_eq!(ours.replace("-Infinity", "null"), serde_json::to_string(&doc).unwrap(), "{name}");
            continue;
        }
        assert_eq!(to_string(&doc).unwrap(), serde_json::to_string(&doc).unwrap(), "{name}");
        assert_eq!(to_string_pretty(&doc).unwrap(), serde_json::to_string_pretty(&doc).unwrap(), "{name}");
        let tree = to_doc_value(&doc).unwrap();
        assert_eq!(to_string_pretty(&tree).unwrap(), serde_json::to_string_pretty(&serde_json::to_value(&doc).unwrap()).unwrap(), "{name}: the tree writes like serde_json's");
    }
}

#[test]
fn trees_match_serde_json_and_read_back_the_same() {
    let s = shapes();
    let tree = to_doc_value(&s).unwrap();
    assert_eq!(tree, serde_json::to_value(&s).unwrap(), "the same tree as serde_json::to_value");
    assert_eq!(tree.to_json(), serde_json::to_value(&s).unwrap());
    assert_eq!(from_doc_value::<Shapes>(tree).unwrap(), s);
    let t = Tagged::Lfo { rate_hz: 0.05 };
    assert_eq!(from_doc_value::<Tagged>(to_doc_value(&t).unwrap()).unwrap(), t, "an internally tagged enum");
    for (name, text) in repo_docs().into_iter().filter(|(_, t)| !t.contains("-Infinity")) {
        let raw: DocValue = json5::from_str(&text).unwrap();
        assert_eq!(raw, json5::from_str::<serde_json::Value>(&text).unwrap(), "{name}: parses to json5's own value");
        let theirs: crate::loop_doc::LoopDoc = serde_json::from_value(raw.to_json()).unwrap();
        let ours: crate::loop_doc::LoopDoc = from_doc_value(raw).unwrap();
        assert_eq!(serde_json::to_string(&ours).unwrap(), serde_json::to_string(&theirs).unwrap(), "{name}: deserializes the same");
    }
}

#[test]
fn errors_read_like_serde_jsons() {
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct S {
        a: f32,
        n: u8,
        e: Option<E>,
    }
    for bad in [json!({ "a": "x", "n": 1 }), json!({ "a": 1, "n": -1 }), json!({ "a": 1, "n": 300 }), json!({ "a": 1, "n": 1.5 }), json!({ "n": 1 }), json!({ "a": 1, "n": 1, "z": 0 }), json!({ "a": 1, "n": 1, "e": "Nope" }), json!({ "a": 1, "n": 1, "e": { "Unit": null, "Newtype": 1 } }), json!({ "a": 1, "n": 1, "e": 3 }), json!([1, 2])] {
        let theirs = serde_json::from_value::<S>(bad.clone()).unwrap_err().to_string();
        let ours = from_doc_value::<S>(DocValue::from(bad.clone())).unwrap_err().to_string();
        assert_eq!(ours, theirs, "{bad}");
    }
}

#[test]
fn minus_infinity_survives_parse_edit_and_write() {
    let text = "{ level: -Infinity, sends: { rv: -Infinity }, x: 0.5, n: 3, list: [-Infinity, 1] }";
    let v: DocValue = json5::from_str(text).unwrap();
    assert_eq!(v["level"].as_f64(), Some(f64::NEG_INFINITY), "not null");
    assert!(v["level"].is_non_finite() && !v["x"].is_non_finite());
    assert_eq!(json5::from_str::<serde_json::Value>(text).unwrap()["level"], serde_json::Value::Null, "what serde_json makes of it");
    assert_eq!(to_string(&v).unwrap(), r#"{"level":-Infinity,"list":[-Infinity,1],"n":3,"sends":{"rv":-Infinity},"x":0.5}"#);
    assert_eq!(v.to_string(), to_string(&v).unwrap(), "Display is the compact text");

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Gain {
        db: f32,
        wide: f64,
        list: Vec<f32>,
    }
    let g = Gain { db: f32::NEG_INFINITY, wide: f64::NEG_INFINITY, list: vec![-6.0, f32::NEG_INFINITY] };
    let tree = to_doc_value(&g).unwrap();
    assert_eq!(tree["db"].as_f64(), Some(f64::NEG_INFINITY));
    assert_eq!(from_doc_value::<Gain>(tree).unwrap(), g, "through a tree");
    let pretty = to_string_pretty(&g).unwrap();
    assert!(pretty.contains("\"db\": -Infinity"), "{pretty}");
    assert_eq!(json5::from_str::<Gain>(&pretty).unwrap(), g, "the writer's text reads back through json5");
    assert_eq!(json5::from_str::<DocValue>(&pretty).unwrap(), to_doc_value(&g).unwrap());
}

#[test]
fn plus_infinity_and_nan_are_refused_by_the_writer() {
    for bad in [f64::INFINITY, f64::NAN] {
        let err = to_string(&json5::from_str::<DocValue>(&format!("{{ a: [{}] }}", if bad.is_nan() { "NaN" } else { "Infinity" })).unwrap()).unwrap_err();
        assert!(err.to_string().contains(if bad.is_nan() { "NaN" } else { "Infinity" }), "{err}");
        assert!(to_string(&bad).is_err() && to_string(&(bad as f32)).is_err() && to_string_pretty(&vec![bad]).is_err());
    }
    // …though a tree still holds them (strict check is what refuses a doc that has them).
    assert!(json5::from_str::<DocValue>("{ a: NaN, b: Infinity }").unwrap()["b"].is_non_finite());
}

#[test]
fn the_api_reads_like_serde_jsons() {
    let mut v = DocValue::from(json!({ "a": { "b": [1, 2.5, "x", true, null] }, "n": -3, "u": 4 }));
    assert_eq!(v["a"]["b"][1].as_f64(), Some(2.5));
    assert_eq!(v["a"]["b"][2], "x");
    assert_eq!(v["a"]["b"][3], true);
    assert!(v["a"]["b"][4].is_null() && v["missing"]["deeper"].is_null() && v["a"]["b"][99].is_null());
    assert_eq!((v["n"].as_i64(), v["n"].as_u64(), v["u"].as_u64(), v["u"].as_i64()), (Some(-3), None, Some(4), Some(4)));
    assert!(v["u"].is_u64() && v["n"].is_i64() && !v["n"].is_u64() && v["a"]["b"][1].is_f64());
    assert_eq!(v.get("a").and_then(|a| a.get("b")).and_then(|b| b.get(0)), Some(&DocValue::from(1u64)));
    v["new"] = DocValue::from(f32::NEG_INFINITY);
    v["a"]["b"].as_array_mut().unwrap().push("y".into());
    assert_eq!(v["new"].as_f64(), Some(f64::NEG_INFINITY));
    assert_eq!(v["a"]["b"].as_array().unwrap().len(), 6);
    assert_eq!(DocValue::from(0.3f32), serde_json::to_value(0.3f32).unwrap(), "an f32 widens like serde_json's");
    assert_eq!(v.get_mut("u").map(DocValue::take), Some(DocValue::from(4u64)));
    assert!(v["u"].is_null());
}
