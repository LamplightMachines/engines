//! Which part of the loop-contract schema governs a node of the tree — the schema walk the checker
//! does, but driven by a path and tolerant of a half-typed doc.

use crate::cst::path::Seg;
use crate::cst::Node;
use serde_json::Value;
use engines_core::doc_value::{DocValue, Map};
use engines_core::schema::{branch_for, child_schema, item_schema, schema};

/// The schema at `segs` below the root (`None` once the path leaves the schema — inside a free-form
/// `meta`, say). Conditional branches (`allOf`/`if`) see the tree's current values.
pub fn schema_at(root: Option<&Node>, segs: &[Seg]) -> Option<&'static Value> {
    let mut s: &'static Value = schema();
    let mut node = root;
    for seg in segs {
        let v = node.map(|n| DocValue::from(n.shallow_value())).unwrap_or_default();
        s = branch_for(s, &v);
        match seg {
            Seg::Key(k) => {
                if k.starts_with("x_") {
                    return None;
                }
                let map = v.as_object().cloned().unwrap_or_default();
                s = child_schema(s, &map, k)?;
                node = node.and_then(|n| n.get(k));
            }
            Seg::Index(i) => {
                s = item_schema(s, &v)?;
                node = node.and_then(|n| n.items().get(*i)).map(|it| &it.node);
            }
        }
        if s == &Value::Bool(true) {
            return None;
        }
    }
    let v = node.map(|n| DocValue::from(n.shallow_value())).unwrap_or_default();
    Some(branch_for(s, &v))
}

/// The object view of `s` for `node`'s current values: `(named keys, schema for other keys)`.
pub fn keys_of(s: &'static Value, node: Option<&Node>) -> (Vec<(&'static str, &'static Value)>, Option<&'static Value>) {
    let map: Map = match node.map(|n| DocValue::from(n.shallow_value())) {
        Some(DocValue::Object(m)) => m,
        _ => Map::new(),
    };
    let (props, extra, _) = engines_core::schema::object_keys(s, &map);
    (props, extra)
}

/// A schema node's description, if any.
pub fn description(s: &Value) -> Option<&str> {
    s.get("description").and_then(Value::as_str)
}

/// `number 0..1`, `integer ≥ 1`, `"sine" | "triangle" | …`, `object` — a one-line type summary.
pub fn type_summary(s: &Value) -> String {
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        return e.iter().map(Value::to_string).collect::<Vec<_>>().join(" | ");
    }
    let t = match s.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | "),
        _ if s.get("oneOf").is_some() => "one of several shapes".into(),
        _ => return String::new(),
    };
    let num = |k: &str| s.get(k).and_then(Value::as_f64);
    let range = match (num("minimum"), num("maximum"), num("exclusiveMinimum")) {
        (Some(lo), Some(hi), _) => format!(" {lo}..{hi}"),
        (Some(lo), None, _) => format!(" ≥ {lo}"),
        (None, Some(hi), _) => format!(" ≤ {hi}"),
        (None, None, Some(lo)) => format!(" > {lo}"),
        _ => String::new(),
    };
    let default = s.get("default").map(|d| format!(" (default {d})")).unwrap_or_default();
    format!("{t}{range}{default}")
}
