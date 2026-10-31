//! CST → `serde_json::Value` (exactly what `json5::from_str::<Value>` yields), and a small inline
//! JSON5 writer for values an edit inserts.

use super::{Node, NodeKind, Num};
use serde_json::{Map, Number, Value};

pub(super) fn node_value(n: &Node) -> Value {
    match &n.kind {
        NodeKind::Object(members) => {
            // Like serde_json's own map visitor: a repeated key keeps the last value.
            let mut map = Map::new();
            for m in members {
                if let Some(v) = &m.value {
                    map.insert(m.key.name.clone(), node_value(v));
                }
            }
            Value::Object(map)
        }
        NodeKind::Array(items) => Value::Array(items.iter().map(|i| node_value(&i.node)).collect()),
        NodeKind::String(s) => Value::String(s.clone()),
        NodeKind::Number(num) => number_value(*num),
        NodeKind::Bool(b) => Value::Bool(*b),
        NodeKind::Null | NodeKind::Invalid => Value::Null,
    }
}

/// The doc tree the engine checks (`json5::from_str::<DocValue>`): like [`node_value`], except that
/// a non-finite number (`-Infinity`) stays a number.
pub(super) fn node_doc_value(n: &Node) -> engines_core::doc_value::DocValue {
    use engines_core::doc_value::DocValue;
    match &n.kind {
        NodeKind::Object(members) => {
            let mut map = engines_core::doc_value::Map::new();
            for m in members {
                if let Some(v) = &m.value {
                    map.insert(m.key.name.clone(), node_doc_value(v));
                }
            }
            DocValue::Object(map)
        }
        NodeKind::Array(items) => DocValue::Array(items.iter().map(|i| node_doc_value(&i.node)).collect()),
        NodeKind::Number(Num::Float(f)) => DocValue::from(*f),
        _ => DocValue::from(node_value(n)),
    }
}

/// `json5` hands integers to serde as `u64`/`i64` (in range — the parser rejects the rest) and
/// everything else as `f64`; serde_json maps a non-finite float to `null`.
fn number_value(num: Num) -> Value {
    match num {
        Num::UInt(u) => u64::try_from(u).map(|u| Value::Number(u.into())).unwrap_or(Value::Null),
        Num::Int(i) => i64::try_from(i).map(|i| Value::Number(i.into())).unwrap_or(Value::Null),
        Num::Float(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
    }
}

/// A value as compact one-line JSON5 in the house style: bare identifier keys, `{ a: 1, b: "x" }`.
pub fn to_json5(v: &Value) -> String {
    match v {
        Value::Object(map) if map.is_empty() => "{}".into(),
        Value::Object(map) => {
            let members: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", key_text(k), to_json5(v))).collect();
            format!("{{ {} }}", members.join(", "))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(to_json5).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

/// A key written bare when it's a plain identifier, else double-quoted.
pub(crate) fn key_text(k: &str) -> String {
    if is_identifier(k) { k.to_string() } else { Value::String(k.to_string()).to_string() }
}

/// An ASCII-safe JSON5 identifier (what's safe to write unquoted).
pub(crate) fn is_identifier(k: &str) -> bool {
    let mut chars = k.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}
