//! The loop contract's JSON Schema, embedded, and a structural walker over a raw doc (ADR 0033).
//!
//! `contracts/loop_contract_v0.9.schema.json` is the authoritative list of keys, enums and ranges.
//! [`walk`] checks a parsed (not yet deserialized) doc against it and reports **located** problems —
//! the engine's serde structs silently drop unknown keys and default wrong-typed values, so this is
//! what turns a typo (`cutof`, `"sawtooth"`, `level: "0.5"`) into an error instead of silence.
//!
//! Supported subset (what the contract uses): `$ref`, `type`, `properties`, `additionalProperties`
//! (bool or schema), `items`, `enum`, `const`, `minimum`/`maximum`/`exclusiveMinimum`/
//! `exclusiveMaximum`, `allOf` (merged), `if`/`then`/`else` with `properties.<k>.const` + `required`
//! conditions, a property schema of `false` (forbidden), and `oneOf` (first branch matching the value's
//! JSON type). `pattern` is not checked here (the semantic checks in [`crate::check`] cover dest paths
//! and roots). Keys starting with `x_` are free-form annotations anywhere. A number may be
//! `-Infinity` only where the schema says `"x-silence": true` (a gain, a band's min — ADR 0054 P3);
//! `Infinity` and `NaN` never.

use crate::check::{did_you_mean, DocDiag};
use crate::doc_value::{DocValue, Map};
use serde_json::Value;
use std::sync::OnceLock;

const SCHEMA_SRC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../contracts/loop_contract_v0.9.schema.json"));

/// The embedded loop-contract schema.
pub fn schema() -> &'static Value {
    static S: OnceLock<Value> = OnceLock::new();
    S.get_or_init(|| serde_json::from_str(SCHEMA_SRC).expect("embedded loop schema parses"))
}

/// Check `doc` (the raw parsed JSON5 value) against the schema, appending diagnostics.
pub fn walk(doc: &DocValue, out: &mut Vec<DocDiag>) {
    walk_node(doc, schema(), "", out);
}

/// Follow `$ref`s (into the embedded schema's `$defs`) to the schema node they name.
pub fn resolve(s: &Value) -> &Value {
    let mut s = s;
    while let Some(r) = s.get("$ref").and_then(Value::as_str) {
        let Some(name) = r.strip_prefix("#/$defs/") else { break };
        match schema().get("$defs").and_then(|d| d.get(name)) {
            Some(t) => s = t,
            None => break,
        }
    }
    s
}

// ── navigation (for editor tooling: completion, hover) ──

/// The schema that governs value `v` under schema `s`: `$ref`s followed, and for a `oneOf` the first
/// branch whose `type` fits `v` (the walker's rule).
pub fn branch_for<'a>(s: &'a Value, v: &DocValue) -> &'a Value {
    let s = resolve(s);
    match s.get("oneOf").and_then(Value::as_array) {
        Some(branches) => branches
            .iter()
            .map(resolve)
            .find(|b| b.get("type").and_then(Value::as_str).is_none_or(|t| type_matches(v, t)))
            .unwrap_or(s),
        None => s,
    }
}

/// The keys an object `node` may hold under schema `s` (`allOf` and `if`/`then` applied for its
/// current values): the named properties with their schemas, the schema for any other key
/// (`additionalProperties` when it's a schema or `true`; `None` = no other keys allowed), and the
/// required keys.
pub fn object_keys<'a>(s: &'a Value, node: &Map) -> (Vec<(&'a str, &'a Value)>, Option<&'a Value>, Vec<&'a str>) {
    let view = object_view(branch_for(s, &DocValue::Object(node.clone())), node);
    let props = view.properties.into_iter().filter(|(_, ps)| *ps != &Value::Bool(false)).collect();
    let extra = view.additional.filter(|a| *a != &Value::Bool(false));
    (props, extra, view.required)
}

/// The schema of `key` in object `node` under `s`: its named property, else `additionalProperties`.
pub fn child_schema<'a>(s: &'a Value, node: &Map, key: &str) -> Option<&'a Value> {
    let (props, extra, _) = object_keys(s, node);
    props.iter().find(|(k, _)| *k == key).map(|(_, ps)| *ps).or(extra)
}

/// The schema of an array's items under `s`.
pub fn item_schema<'a>(s: &'a Value, v: &DocValue) -> Option<&'a Value> {
    branch_for(s, v).get("items")
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_string() } else { format!("{path}.{key}") }
}

fn type_name(v: &DocValue) -> &'static str {
    match v {
        DocValue::Null => "null",
        DocValue::Bool(_) => "boolean",
        DocValue::Number(n) if n.is_i64() || n.is_u64() => "integer",
        DocValue::Number(_) => "number",
        DocValue::String(_) => "string",
        DocValue::Array(_) => "array",
        DocValue::Object(_) => "object",
    }
}

fn type_matches(v: &DocValue, t: &str) -> bool {
    let actual = type_name(v);
    actual == t || (t == "number" && actual == "integer")
}

/// The object view of a schema node with `allOf` and `if`/`then`/`else` applied for this value:
/// the merged `properties` (a later branch's property schema replaces an earlier one), the
/// strictest `additionalProperties`, and the union of every applicable branch's `required` (so a
/// conditional requirement — an LFO's `rate_hz` — is enforced, not just the top-level list).
struct ObjectView<'a> {
    properties: Vec<(&'a str, &'a Value)>,
    additional: Option<&'a Value>,
    required: Vec<&'a str>,
}

fn object_view<'a>(s: &'a Value, node: &Map) -> ObjectView<'a> {
    let mut view = ObjectView { properties: Vec::new(), additional: None, required: Vec::new() };
    merge_into(&mut view, s, node);
    view
}

fn merge_into<'a>(view: &mut ObjectView<'a>, s: &'a Value, node: &Map) {
    let s = resolve(s);
    if let Some(props) = s.get("properties").and_then(Value::as_object) {
        for (k, ps) in props {
            if let Some(slot) = view.properties.iter_mut().find(|(pk, _)| *pk == k.as_str()) {
                slot.1 = ps;
            } else {
                view.properties.push((k.as_str(), ps));
            }
        }
    }
    if let Some(req) = s.get("required").and_then(Value::as_array) {
        for k in req.iter().filter_map(Value::as_str) {
            if !view.required.contains(&k) {
                view.required.push(k);
            }
        }
    }
    if let Some(ap) = s.get("additionalProperties") {
        // `false` is the strictest; otherwise the first schema/true wins.
        if ap == &Value::Bool(false) || view.additional.is_none() {
            view.additional = Some(ap);
        }
    }
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        for branch in all {
            if let Some(cond) = branch.get("if") {
                let pick = if condition_holds(cond, node) { branch.get("then") } else { branch.get("else") };
                if let Some(p) = pick {
                    merge_into(view, p, node);
                }
            } else {
                merge_into(view, branch, node);
            }
        }
    }
}

/// An `if` of the forms the contract uses: `properties.<k>.const` (+ optional `required`).
fn condition_holds(cond: &Value, node: &Map) -> bool {
    if let Some(req) = cond.get("required").and_then(Value::as_array) {
        if !req.iter().filter_map(Value::as_str).all(|k| node.contains_key(k)) {
            return false;
        }
    }
    if let Some(props) = cond.get("properties").and_then(Value::as_object) {
        for (k, ps) in props {
            if let Some(c) = ps.get("const") {
                // An absent key fails a const condition only when `required` asked for it; JSON
                // Schema treats a missing property as passing — mirror that.
                if let Some(v) = node.get(k) {
                    if v != c {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn walk_node(v: &DocValue, s: &Value, path: &str, out: &mut Vec<DocDiag>) {
    let s = resolve(s);
    if s == &Value::Bool(true) {
        return;
    }
    if let Some(branches) = s.get("oneOf").and_then(Value::as_array) {
        let fits = |b: &Value| {
            let b = resolve(b);
            b.get("type").and_then(Value::as_str).is_none_or(|t| type_matches(v, t))
        };
        if let Some(b) = branches.iter().find(|b| fits(b)) {
            walk_node(v, b, path, out);
        } else {
            out.push(DocDiag::error(path, "type", format!("unexpected {}", type_name(v))));
        }
        return;
    }
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_matches(v, t),
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).any(|t| type_matches(v, t)),
            _ => true,
        };
        if !ok {
            let want = match t {
                Value::String(t) => t.clone(),
                Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" or "),
                _ => String::new(),
            };
            let found = if v.is_null() { "null".to_string() } else { format!("{} {}", type_name(v), short(v)) };
            out.push(DocDiag::error(path, "type", format!("expected {want}, found {found}")));
            return;
        }
        // `-Infinity` is silence, only on a gain (`x-silence`); `Infinity` and `NaN` mean nothing.
        if v.is_non_finite() {
            if v.as_f64() == Some(f64::NEG_INFINITY) && s.get("x-silence") == Some(&Value::Bool(true)) {
                return;
            }
            let why = if v.as_f64() == Some(f64::NEG_INFINITY) { " (-Infinity, silence, is only for a gain: a `*_db` level, a send or a return)" } else { "" };
            out.push(DocDiag::error(path, "type", format!("expected a finite number, found {v}{why}")));
            return;
        }
    }
    if let Some(c) = s.get("const") {
        if v != c {
            out.push(DocDiag::error(path, "enum", format!("must be {c}, found {}", short(v))));
        }
    }
    if let Some(options) = s.get("enum").and_then(Value::as_array) {
        if !options.iter().any(|o| v == o) {
            let names: Vec<&str> = options.iter().filter_map(Value::as_str).collect();
            let hint = v.as_str().and_then(|x| did_you_mean(x, &names)).map(|m| format!(" — did you mean \"{m}\"?")).unwrap_or_default();
            out.push(DocDiag::error(path, "enum", format!("unknown value {} (expected one of: {}){hint}", short(v), names.join(", "))));
        }
    }
    if let Some(x) = v.as_f64() {
        let bound = |key: &str, bad: &dyn Fn(f64, f64) -> bool, words: &str| {
            s.get(key).and_then(Value::as_f64).filter(|&b| bad(x, b)).map(|b| format!("{x} is {words} {b}"))
        };
        let msg = bound("minimum", &|x, b| x < b, "below the minimum")
            .or_else(|| bound("maximum", &|x, b| x > b, "above the maximum"))
            .or_else(|| bound("exclusiveMinimum", &|x, b| x <= b, "not above"))
            .or_else(|| bound("exclusiveMaximum", &|x, b| x >= b, "not below"));
        if let Some(m) = msg {
            out.push(DocDiag::error(path, "range", m));
        }
    }
    match v {
        DocValue::Object(map) => {
            let view = object_view(s, map);
            for (k, child) in map {
                let child_path = join(path, k);
                if let Some((_, ps)) = view.properties.iter().find(|(pk, _)| *pk == k.as_str()) {
                    if ps == &&Value::Bool(false) {
                        out.push(DocDiag::error(&child_path, "not-allowed", format!("`{k}` is not allowed here")));
                    } else {
                        walk_node(child, ps, &child_path, out);
                    }
                    continue;
                }
                // `x_*` is a free-form annotation in a record — but in an id-keyed map (instruments,
                // modulators, matrix, …) it's an *id* the engine plays, so it's checked like any other.
                if k.starts_with("x_") && !matches!(view.additional, Some(Value::Object(_))) {
                    continue;
                }
                match view.additional {
                    Some(Value::Bool(false)) => {
                        let known: Vec<&str> = view.properties.iter().map(|(pk, _)| *pk).collect();
                        let v08 = crate::params::v09_name(k).filter(|n| known.contains(n)).map(|n| format!(" — schema v0.9 calls it `{n}` (in its unit; `engines-migrate` converts a v0.8 doc)"));
                        let hint = v08.or_else(|| did_you_mean(k, &known).map(|m| format!(" — did you mean `{m}`?"))).unwrap_or_default();
                        out.push(DocDiag::error(&child_path, "unknown-key", format!("unknown key `{k}`{hint}")));
                    }
                    Some(ap @ Value::Object(_)) => walk_node(child, ap, &child_path, out),
                    _ => {}
                }
            }
            for k in &view.required {
                if !map.contains_key(*k) {
                    out.push(DocDiag::error(path, "missing", format!("missing required key `{k}`")));
                }
            }
        }
        DocValue::Array(items) => {
            if let Some(is) = s.get("items") {
                for (i, item) in items.iter().enumerate() {
                    walk_node(item, is, &format!("{path}[{i}]"), out);
                }
            }
        }
        _ => {}
    }
}

fn short(v: &DocValue) -> String {
    let s = v.to_string();
    if s.chars().count() > 40 { format!("{}…", s.chars().take(40).collect::<String>()) } else { s }
}

/// Check `v` against a schema from `contracts/` (a small subset of JSON Schema; returns the problems, empty when it
/// conforms). Shared by the capability catalog and the MIDI analysis contract tests. The subset: `$ref` (into `$defs`), `const`, `enum`,
/// `type`, `oneOf`, `required`, `properties`, `additionalProperties`, `items`, `minItems`/`maxItems`,
/// `minimum`/`maximum`. (`pattern` isn't checked: the hash's format has its own test.)
fn validate_at(v: &Value, s: &Value, root: &Value, path: &str, errs: &mut Vec<String>) {
    let s = match s.get("$ref").and_then(Value::as_str) {
        Some(r) => &root["$defs"][r.strip_prefix("#/$defs/").unwrap()],
        None => s,
    };
    if let Some(c) = s.get("const") {
        if v != c {
            errs.push(format!("{path}: {v} ≠ const {c}"));
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            errs.push(format!("{path}: {v} not in {e:?}"));
        }
    }
    if let Some(t) = s.get("type") {
        let types: Vec<&str> = match t {
            Value::String(t) => vec![t.as_str()],
            _ => t.as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect(),
        };
        let fits = |t: &str| match t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "number" => v.is_number(),
            "integer" => v.is_i64() || v.is_u64(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            _ => false,
        };
        if !types.iter().any(|t| fits(t)) {
            errs.push(format!("{path}: {v} isn't {types:?}"));
            return;
        }
    }
    if let Some(branches) = s.get("oneOf").and_then(Value::as_array) {
        let ok = branches.iter().filter(|b| {
            let mut e = Vec::new();
            validate_at(v, b, root, path, &mut e);
            e.is_empty()
        });
        if ok.count() != 1 {
            errs.push(format!("{path}: {v} matches not exactly one of {branches:?}"));
        }
    }
    if let Some(n) = v.as_f64() {
        if s.get("minimum").and_then(Value::as_f64).is_some_and(|m| n < m) || s.get("maximum").and_then(Value::as_f64).is_some_and(|m| n > m) {
            errs.push(format!("{path}: {n} out of range"));
        }
    }
    if let Some(items) = v.as_array() {
        let n = items.len() as u64;
        if s.get("minItems").and_then(Value::as_u64).is_some_and(|m| n < m) || s.get("maxItems").and_then(Value::as_u64).is_some_and(|m| n > m) {
            errs.push(format!("{path}: {n} items"));
        }
        if let Some(is) = s.get("items") {
            for (i, it) in items.iter().enumerate() {
                validate_at(it, is, root, &format!("{path}[{i}]"), errs);
            }
        }
    }
    if let Some(obj) = v.as_object() {
        for r in s.get("required").and_then(Value::as_array).into_iter().flatten() {
            if !obj.contains_key(r.as_str().unwrap()) {
                errs.push(format!("{path}: missing {r}"));
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        for (k, val) in obj {
            let sub = format!("{path}.{k}");
            match (props.and_then(|p| p.get(k)), s.get("additionalProperties")) {
                (Some(ps), _) => validate_at(val, ps, root, &sub, errs),
                (None, Some(Value::Bool(false))) => errs.push(format!("{sub}: not allowed")),
                (None, Some(ap)) if ap.is_object() => validate_at(val, ap, root, &sub, errs),
                _ => {}
            }
        }
    }
}

/// [`validate_at`] from the document root: `s` is the root schema too, so `$ref`s resolve into its `$defs`.
pub fn validate_subset(v: &Value, schema: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    validate_at(v, schema, schema, "", &mut errs);
    errs
}

