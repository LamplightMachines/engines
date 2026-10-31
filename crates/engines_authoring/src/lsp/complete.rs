//! Completion: what may go at the cursor, from the schema and from the doc itself.
//!
//! - **keys** — the schema's keys for the object (required first), params filtered to what the
//!   instrument's synth type reads, send keys from the doc's buses, voice ids, bound param paths;
//! - **values** — enum values, booleans, `harmony.root`/`mode`, modulator ids for a `source` or a
//!   bind, every valid `dest` path for the route's kind (in `matrix` or a song layer's route), pool /
//!   song-layer ids;
//! - **note offsets** — the scale's tones around the current value, labelled
//!   `7 · A4 · 5 · D dorian` (offset · note · degree · key).

use super::music;
use super::refs::IdKind;
use super::schema_nav::{description, keys_of, schema_at, type_summary};
use super::Analysis;
use crate::cst::path::{Seg, Target};
use crate::cst::{Node, NodeKind, Span};
use serde_json::Value;
use engines_core::check::{param_applies, LFO_WAVES, MODES};
use engines_core::params::units;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Key,
    Value,
    /// An id (modulator, bus, pool, …) or a path built from ids.
    Id,
    /// A note offset.
    Note,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: String,
    pub kind: ItemKind,
    pub detail: Option<String>,
    pub doc: Option<String>,
    /// What replaces `replace` (a snippet when `snippet`).
    pub insert: String,
    pub snippet: bool,
    pub replace: Span,
    /// Lower sorts first.
    pub sort: String,
}

impl Item {
    fn new(label: impl Into<String>, kind: ItemKind, insert: impl Into<String>, replace: Span) -> Item {
        Item { label: label.into(), kind, detail: None, doc: None, insert: insert.into(), snippet: false, replace, sort: String::new() }
    }
    fn detail(mut self, d: impl Into<String>) -> Item {
        self.detail = Some(d.into());
        self
    }
    fn doc(mut self, d: Option<&str>) -> Item {
        self.doc = d.map(str::to_string);
        self
    }
    fn sort(mut self, s: impl Into<String>) -> Item {
        self.sort = s.into();
        self
    }
    fn snippet(mut self) -> Item {
        self.snippet = true;
        self
    }
}

pub fn complete(a: &Analysis, offset: usize) -> Vec<Item> {
    let cursor = a.cst.locate(&a.text, offset);
    let segs = cursor.segs;
    match cursor.target {
        Target::Key { object, member } => {
            let parent = &segs[..segs.len() - 1];
            let quote = member.key.quote;
            key_items(a, parent, object, Some(member.key.name.as_str()), member.key.span, quote, member.value.is_none())
        }
        Target::ObjectSlot { object } => key_items(a, &segs, object, None, Span::new(offset, offset), None, true),
        Target::Value { node, .. } => {
            let replace = match node {
                Some(n) if !n.is_container() => n.span,
                _ => Span::new(offset, offset),
            };
            let quote = node.filter(|n| matches!(n.kind, NodeKind::String(_))).and_then(|n| a.text[n.span.start..].chars().next());
            value_items(a, &segs, node, replace, quote.unwrap_or('"'))
        }
        Target::ArraySlot { index, .. } => {
            let mut item_segs = segs.clone();
            item_segs.push(Seg::Index(index));
            value_items(a, &item_segs, None, Span::new(offset, offset), '"')
        }
        Target::Nothing => Vec::new(),
    }
}

fn key_name(s: &Seg) -> &str {
    match s {
        Seg::Key(k) => k,
        Seg::Index(_) => "",
    }
}

fn keys(segs: &[Seg]) -> Vec<&str> {
    segs.iter().map(key_name).collect()
}

/// A key completion's insert text: `key: <value placeholder>` — the value part only when the member
/// has no value yet.
fn key_insert(key: &str, quote: Option<char>, with_value: bool, schema: Option<&Value>) -> (String, bool) {
    let k = crate::cst::edit::key_as(key, quote);
    if !with_value {
        return (k, false);
    }
    let value = schema.map(|s| value_snippet_for(key, s)).unwrap_or_else(|| "$0".into());
    (format!("{k}: {value}"), true)
}

/// The placeholder for `key`'s value: the schema's, else the engine's default in its unit
/// (`level_db: -6.02`); a bus return starts at 0 dB, since leaving it out is silence.
fn value_snippet_for(key: &str, s: &Value) -> String {
    let resolved = engines_core::schema::resolve(s);
    let number = matches!(resolved.get("type").and_then(Value::as_str), Some("number" | "integer"));
    if number && resolved.get("default").is_none() {
        if key == "return_gain_db" {
            return "${1:0}".into();
        }
        if let Some(d) = engines_core::params::default_num(key).filter(|d| d.is_finite()) {
            return format!("${{1:{}}}", (d * 100.0).round() / 100.0);
        }
    }
    value_snippet(s)
}

/// The placeholder for a value of schema `s`: `{ $0 }`, `[ $0 ]`, an enum choice, a default…
fn value_snippet(s: &Value) -> String {
    let s = engines_core::schema::resolve(s);
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        let strs: Vec<&str> = e.iter().filter_map(Value::as_str).collect();
        if strs.len() == e.len() {
            return format!("\"${{1|{}|}}\"", strs.join(","));
        }
    }
    match s.get("type").and_then(Value::as_str) {
        Some("object") => "{ $0 }".into(),
        Some("array") => "[$0]".into(),
        Some("boolean") => "${1|true,false|}".into(),
        Some("string") => "\"$1\"".into(),
        Some("number" | "integer") => match s.get("default").and_then(Value::as_f64) {
            Some(d) if d.fract() == 0.0 => format!("${{1:{}}}", d as i64),
            Some(d) => format!("${{1:{d}}}"),
            None => "$1".into(),
        },
        _ => "$0".into(),
    }
}

fn key_items(a: &Analysis, segs: &[Seg], object: &Node, typing: Option<&str>, replace: Span, quote: Option<char>, with_value: bool) -> Vec<Item> {
    let existing: Vec<&str> = object.members().iter().map(|m| m.key.name.as_str()).filter(|k| Some(*k) != typing).collect();
    let fresh = |k: &str| !existing.contains(&k);
    let ks = keys(segs);
    let id_key = |label: &str, insert_value: &str, detail: String| {
        let k = crate::cst::edit::key_as(label, quote.or_else(|| (!crate::cst::is_identifier(label)).then_some('"')));
        let insert = if with_value { format!("{k}: {insert_value}") } else { k };
        Item::new(label, ItemKind::Id, insert, replace).detail(detail).snippet()
    };
    match ks.as_slice() {
        // Id-keyed maps: the key is a new name of the author's choosing.
        ["instruments"] | ["modulators"] | ["matrix"] | ["buses"] | ["sequence_pools"] | ["song", "matrices"] | ["meta", ..] => Vec::new(),
        ["instruments", _, "sends_db"] => a.refs.ids(IdKind::Bus, None).into_iter().filter(|b| fresh(b)).map(|b| id_key(b, "${1:-12}", "send to bus, dB".into())).collect(),
        ["safe_bounds" | "constraints"] => numeric_paths(a)
            .into_iter()
            .filter(|(p, _)| fresh(p))
            .map(|(p, now)| {
                let detail = format!("now {}", units::fmt_authored(&p, now as f32));
                id_key(&p, "{ min: ${1}, max: ${2} }", detail)
            })
            .collect(),
        // A song block's drift overrides (ADR 0055): the same param paths, pinned by default.
        ["song", "sequence", _, "control", "drift"] => numeric_paths(a)
            .into_iter()
            .filter(|(p, _)| fresh(p))
            .map(|(p, now)| {
                let detail = format!("now {} — \"hold\" pins it in this block", units::fmt_authored(&p, now as f32));
                id_key(&p, "\"hold\"", detail)
            })
            .collect(),
        ["song", "matrices", _] => a.refs.ids(IdKind::Route, None).into_iter().filter(|r| fresh(r)).map(|r| id_key(r, "{ $0 }", "route override (null removes it)".into())).collect(),
        ["modulators", m, "steps", _, "voicing"] => {
            // Voice ids used elsewhere in this modulator, then the next free `vN`.
            let mut ids: Vec<String> = Vec::new();
            for step in a.node(&format!("modulators.{m}.steps")).map(Node::items).unwrap_or_default() {
                for v in step.node.get("voicing").map(Node::members).unwrap_or_default() {
                    if !ids.contains(&v.key.name) {
                        ids.push(v.key.name.clone());
                    }
                }
            }
            let next = (1..).map(|n| format!("v{n}")).find(|v| !ids.contains(v)).expect("unbounded");
            ids.push(next);
            ids.into_iter().filter(|v| fresh(v)).enumerate().map(|(i, v)| id_key(&v, "{ offset: ${1:0} }", "voice".into()).sort(format!("{i:03}"))).collect()
        }
        _ => schema_keys(a, segs, object, replace, quote, with_value, &existing),
    }
}

fn schema_keys(a: &Analysis, segs: &[Seg], object: &Node, replace: Span, quote: Option<char>, with_value: bool, existing: &[&str]) -> Vec<Item> {
    let Some(s) = schema_at(a.root(), segs) else { return Vec::new() };
    let (props, _) = keys_of(s, Some(object));
    let required: Vec<&str> = s.get("required").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    // Params: only what this instrument's synth type reads.
    let ks = keys(segs);
    let synth = match ks.as_slice() {
        ["instruments", inst, "params"] => {
            let i = a.node(&format!("instruments.{inst}"));
            let kind = i.and_then(|i| i.get("type")).and_then(Node::as_str).unwrap_or("subtractive").to_string();
            let noise = object.get("wave").or(object.get("source")).and_then(Node::as_str) == Some("filtered_noise");
            let pitched = i.and_then(|i| i.get("pitched")).and_then(Node::as_bool) == Some(true);
            Some((kind, noise, pitched))
        }
        _ => None,
    };
    // A route's depth: only the key in its dest's unit (`depth_oct` for a cutoff), once the dest is written.
    let depth_key = match ks.as_slice() {
        ["matrix", id] | ["song", "matrices", _, id] => object
            .get("dest")
            .or_else(|| a.node(&format!("matrix.{id}")).and_then(|b| b.get("dest")))
            .and_then(Node::as_str)
            .map(|d| engines_core::loop_doc::depth_unit_for(d).key()),
        _ => None,
    };
    props
        .into_iter()
        .filter(|(k, _)| !existing.contains(k))
        .filter(|(k, _)| synth.as_ref().is_none_or(|(kind, noise, pitched)| param_applies(kind, *noise, *pitched, k)))
        .filter(|(k, _)| depth_key.is_none_or(|want| engines_core::params::DepthUnit::from_key(k).is_none() || *k == want))
        .map(|(k, ps)| {
            let (insert, snippet) = key_insert(k, quote, with_value, Some(ps));
            let mut item = Item::new(k, ItemKind::Key, insert, replace).detail(type_summary(engines_core::schema::resolve(ps))).doc(description(engines_core::schema::resolve(ps)));
            item.snippet = snippet;
            item.sort(if required.contains(&k) { format!("0{k}") } else { format!("1{k}") })
        })
        .collect()
}

/// Every numeric param path a bound may name, with its authored value: instrument params, globals,
/// bus returns and effect params, route depths. Read from the tree, so it works mid-keystroke.
fn numeric_paths(a: &Analysis) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    let Some(root) = a.root() else { return out };
    let nums = |prefix: String, obj: Option<&Node>, out: &mut Vec<(String, f64)>| {
        for m in obj.map(Node::members).unwrap_or_default() {
            if let Some(x) = m.value.as_ref().and_then(Node::as_f64) {
                out.push((format!("{prefix}.{}", m.key.name), x));
            }
        }
    };
    nums("global".into(), root.get("global"), &mut out);
    for name in ["master_gain_db", "stereo_width"] {
        if !out.iter().any(|(p, _)| p == &format!("global.{name}")) {
            out.push((format!("global.{name}"), engines_core::params::default_num(name).map_or(0.0, f64::from)));
        }
    }
    for i in root.get("instruments").map(Node::members).unwrap_or_default() {
        nums(format!("instruments.{}.params", i.key.name), i.value.as_ref().and_then(|v| v.get("params")), &mut out);
    }
    for b in root.get("buses").map(Node::members).unwrap_or_default() {
        let Some(bus) = &b.value else { continue };
        if bus.get("type").and_then(Node::as_str) == Some("external_return") {
            continue;
        }
        out.push((format!("buses.{}.return_gain_db", b.key.name), bus.get("return_gain_db").and_then(Node::as_f64).unwrap_or(f64::NEG_INFINITY)));
        for fx in bus.get("effects").map(Node::members).unwrap_or_default() {
            nums(format!("buses.{}.effects.{}.params", b.key.name, fx.key.name), fx.value.as_ref().and_then(|v| v.get("params")), &mut out);
        }
    }
    for r in root.get("matrix").map(Node::members).unwrap_or_default() {
        let Some(e) = &r.value else { continue };
        if e.get("kind").and_then(Node::as_str) == Some("mod") {
            // A depth is bounded under its own key, in its dest's unit (`matrix.m.depth_oct`).
            let key = engines_core::loop_doc::depth_unit_for(e.get("dest").and_then(Node::as_str).unwrap_or("")).key();
            out.push((format!("matrix.{}.{key}", r.key.name), e.get(key).and_then(Node::as_f64).unwrap_or(0.0)));
        }
    }
    out
}

fn value_items(a: &Analysis, segs: &[Seg], node: Option<&Node>, replace: Span, quote: char) -> Vec<Item> {
    let ks = keys(segs);
    let q = |s: &str| crate::cst::edit::string_as(s, quote);
    let strs = |values: Vec<(String, Option<String>)>, kind: ItemKind| -> Vec<Item> {
        values.into_iter().enumerate().map(|(i, (v, d))| {
            let it = Item::new(v.clone(), kind, q(&v), replace).sort(format!("{i:03}"));
            match d {
                Some(d) => it.detail(d),
                None => it,
            }
        }).collect()
    };
    let mods = |note_only: bool| -> Vec<(String, Option<String>)> {
        a.refs.ids(IdKind::Modulator, None).into_iter().filter_map(|id| {
            let m = a.node(&format!("modulators.{id}"))?;
            let kind = m.get("type").and_then(Node::as_str).unwrap_or("?");
            let output = m.get("output").and_then(Node::as_str).unwrap_or("control");
            if note_only && output != "note" {
                return None;
            }
            let rate = m.get("rate_hz").and_then(Node::as_f64).map(|r| format!(" {r} Hz")).unwrap_or_default();
            Some((id.to_string(), Some(format!("{kind} · {output}{rate}"))))
        }).collect()
    };
    let last = ks.last().copied().unwrap_or("");
    let in_voice = matches!(ks.as_slice(), ["modulators", _, "steps", _, "voicing", _, "offset"] | ["sequence_pools", _, "notes", _, "offset"] | ["modulators", _, "phrase", "realized", "notes", _, "offset"]);
    let in_walk = matches!(ks.as_slice(), ["modulators", _, "pitch_walk", "pool", _]);
    if in_voice || in_walk {
        return offset_items(a, node, replace);
    }
    match ks.as_slice() {
        ["harmony", "root"] => {
            const ROOTS: [&str; 17] = ["C", "C#", "Db", "D", "D#", "Eb", "E", "F", "F#", "Gb", "G", "G#", "Ab", "A", "A#", "Bb", "B"];
            strs(ROOTS.iter().map(|r| (r.to_string(), None)).collect(), ItemKind::Value)
        }
        ["harmony", "mode"] => {
            let root = a.node("harmony.root").and_then(Node::as_str).unwrap_or("C");
            let values = MODES.iter().map(|m| {
                let detail = engines_core::theory::Key::new(root, m).map(|k| {
                    let notes: Vec<String> = (0..7).map(|d| k.spell(k.scale.pitch_class(d))).collect();
                    format!("{} · {}", notes.join(" "), k.scale.mode.degree_labels().join(" "))
                });
                (m.to_string(), detail)
            }).collect();
            strs(values, ItemKind::Value)
        }
        ["matrix", _, "source"] | ["song", "matrices", _, _, "source"] => {
            let kind = route_kind(a, &segs[..segs.len() - 1]);
            strs(mods(kind.as_deref() == Some("note")), ItemKind::Id)
        }
        ["matrix", _, "dest"] | ["song", "matrices", _, _, "dest"] => {
            let kind = route_kind(a, &segs[..segs.len() - 1]);
            strs(dest_paths(a, kind.as_deref()), ItemKind::Id)
        }
        ["movement", "bind", "chord" | "bass" | "arp"] | ["movement", "bind", "oneshot", _] => strs(mods(true), ItemKind::Id),
        ["modulators", _, "phrase", "pool", _] | ["modulators", _, "phrase", "active"] => {
            let mut ids: Vec<(String, Option<String>)> = a.refs.ids(IdKind::Pool, None).into_iter().map(|p| (p.to_string(), Some("sequence pool".into()))).collect();
            ids.extend(engines_core::phrase::library().iter().map(|(name, def)| (format!("lib:{name}"), Some(format!("built-in · {} steps", def.length)))));
            strs(ids, ItemKind::Id)
        }
        ["song", "sequence", _, "matrix"] => strs(a.refs.ids(IdKind::Layer, None).into_iter().map(|l| (l.to_string(), None)).collect(), ItemKind::Id),
        ["modulators", _, "wave"] => strs(LFO_WAVES.iter().map(|w| (w.to_string(), None)).collect(), ItemKind::Value),
        _ => {
            let Some(s) = schema_at(a.root(), segs) else { return Vec::new() };
            let s = engines_core::schema::resolve(s);
            if let Some(e) = s.get("enum").and_then(Value::as_array) {
                return e.iter().enumerate().map(|(i, v)| {
                    let label = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
                    let insert = v.as_str().map(q).unwrap_or_else(|| v.to_string());
                    Item::new(label, ItemKind::Value, insert, replace).sort(format!("{i:03}")).doc(description(s))
                }).collect();
            }
            match s.get("type").and_then(Value::as_str) {
                Some("boolean") => ["true", "false"].iter().map(|b| Item::new(*b, ItemKind::Value, *b, replace)).collect(),
                Some("number" | "integer") => match s.get("default") {
                    Some(d) => vec![Item::new(d.to_string(), ItemKind::Value, d.to_string(), replace).detail(format!("default {last}"))],
                    None => Vec::new(),
                },
                _ => Vec::new(),
            }
        }
    }
}

/// The `kind` of the route entry at `entry` (`matrix.<route>` or `song.matrices.<layer>.<route>`) —
/// a song layer that only patches a base route keeps the base route's kind.
fn route_kind(a: &Analysis, entry: &[Seg]) -> Option<String> {
    let own = a.node(&crate::cst::path::render(entry)).and_then(|e| e.get("kind")).and_then(Node::as_str);
    let base = || match keys(entry).as_slice() {
        ["song", "matrices", _, route] => a.node(&format!("matrix.{route}.kind")).and_then(Node::as_str),
        _ => None,
    };
    own.or_else(base).map(str::to_string)
}

/// Every `dest` a route of `kind` (`mod` / `note` / unknown) can target in this doc.
fn dest_paths(a: &Analysis, kind: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let notes = kind != Some("mod");
    let params = kind != Some("note");
    let Some(root) = a.root() else { return out };
    for m in root.get("instruments").map(Node::members).unwrap_or_default() {
        let id = &m.key.name;
        let inst = m.value.as_ref();
        let synth = inst.and_then(|i| i.get("type")).and_then(Node::as_str).unwrap_or("subtractive");
        if notes {
            out.push((format!("instruments.{id}.notes"), Some(format!("{synth} · notes"))));
        }
        if params {
            if synth == "midi" {
                for cc in inst.and_then(|i| i.get("routing")).and_then(|r| r.get("cc")).map(Node::members).unwrap_or_default() {
                    out.push((format!("instruments.{id}.params.{}", cc.key.name), Some("midi CC".into())));
                }
                continue;
            }
            let wave = inst.and_then(|i| i.get("params")).and_then(|p| p.get("wave").or(p.get("source"))).and_then(Node::as_str);
            let pitched = inst.and_then(|i| i.get("pitched")).and_then(Node::as_bool) == Some(true);
            for p in engines_core::engine::MODULATABLE_INST_PARAMS {
                if param_applies(synth, wave == Some("filtered_noise"), pitched, p) {
                    let path = format!("instruments.{id}.params.{p}");
                    let unit = units::unit_label(&path);
                    out.push((path, Some(format!("{synth} · {unit}"))));
                }
            }
        }
    }
    if params {
        out.push(("global.master_gain_db".into(), Some(units::unit_label("global.master_gain_db").to_string())));
        out.push(("global.stereo_width".into(), Some("0..1".into())));
        for b in root.get("buses").map(Node::members).unwrap_or_default() {
            let id = &b.key.name;
            if b.value.as_ref().and_then(|v| v.get("type")).and_then(Node::as_str) == Some("external_return") {
                continue;
            }
            out.push((format!("buses.{id}.return_gain_db"), Some("bus return · dB".into())));
            for fx in b.value.as_ref().and_then(|v| v.get("effects")).map(Node::members).unwrap_or_default() {
                for leaf in engines_core::effects::REVERB_PARAM_LEAVES {
                    let path = format!("buses.{id}.effects.{}.params.{leaf}", fx.key.name);
                    let unit = units::unit_label(&path);
                    out.push((path, Some(format!("reverb · {unit}"))));
                }
            }
        }
    }
    out
}

/// The scale's tones from two octaves below the root to two above, nearest the current value first.
fn offset_items(a: &Analysis, node: Option<&Node>, replace: Span) -> Vec<Item> {
    let Some(key) = &a.key else { return Vec::new() };
    let current = node.and_then(Node::as_f64).unwrap_or(0.0);
    let mut offsets: Vec<i32> = (-24..=24).filter(|&o| music::in_scale(key, o as f64)).collect();
    // Nearest first; on a tie, the one above.
    offsets.sort_by_key(|&o| ((((o as f64) - current).abs() * 100.0) as i64, (o as f64) < current));
    let key_name = key.name();
    offsets
        .into_iter()
        .enumerate()
        .map(|(i, o)| {
            let detail = format!("{} · {} · {key_name}", music::note_of(key, o as f64, 0.0), music::degree_of(key, o as f64));
            Item::new(o.to_string(), ItemKind::Note, o.to_string(), replace).detail(detail).sort(format!("{i:03}"))
        })
        .collect()
}

#[cfg(test)]
mod tests;
