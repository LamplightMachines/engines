//! Go to definition, find references, rename — over the id index ([`super::refs`]), plus "go to the
//! param a `dest` / bound path names".

use super::refs::{Form, IdKind};
use super::Analysis;
use crate::cst::edit::TextEdit;
use crate::cst::path::{Seg, Target};
use crate::cst::{is_identifier, Span};

/// Where what's under the cursor is defined: an id's key — or, on a `dest` / bound path's param
/// segment, that param's key in the doc.
pub fn definition(a: &Analysis, offset: usize) -> Option<Span> {
    if let Some(r) = a.refs.at(offset) {
        if let Some(d) = a.refs.def_of(r) {
            return Some(d.span);
        }
    }
    // A param path: `dest: "instruments.pad.params.cutoff_hz"` (in `matrix` or a song layer's route),
    // `safe_bounds: { "global.master_gain_db": … }`.
    let cursor = a.cst.locate(&a.text, offset);
    let path = match (&cursor.target, cursor.segs.as_slice()) {
        (Target::Value { node: Some(n), .. }, [Seg::Key(m), _, Seg::Key(d)]) if m == "matrix" && d == "dest" => n.as_str()?.to_string(),
        (Target::Value { node: Some(n), .. }, [Seg::Key(s), Seg::Key(m), _, _, Seg::Key(d)]) if s == "song" && m == "matrices" && d == "dest" => n.as_str()?.to_string(),
        (Target::Key { member, .. }, [Seg::Key(f), _]) if f == "safe_bounds" || f == "constraints" => member.key.name.clone(),
        (Target::Key { member, .. }, [Seg::Key(s), Seg::Key(q), _, Seg::Key(c), Seg::Key(d), _]) if s == "song" && q == "sequence" && c == "control" && d == "drift" => member.key.name.clone(),
        _ => return None,
    };
    let f = a.cst.find(&path);
    f.member.map(|m| m.key.span)
}

/// Where a JSON path is written (ADR 0040, `engines/pathRange` — the live panel's jump-to-source).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathSpan {
    /// The whole member (key through value), or the array item / root value.
    pub span: Span,
    /// Where the cursor goes: the member's key, or an item's opening bracket.
    pub key: Span,
    /// The whole path matched (otherwise these are its deepest existing prefix's).
    pub complete: bool,
}

/// Resolve `segs` to where it's written. A `song.sequence[i]` step whose entry no longer carries
/// `label` (a block inserted before it since the panel drew it) moves to the entry that does — a
/// song entry is labelled by `label`, else its `matrix`, else `base` (as `SongSpec::label`).
/// `None` only for a doc with no value at all.
pub fn path_span(a: &Analysis, segs: &[Seg], label: Option<&str>) -> Option<PathSpan> {
    let mut segs = segs.to_vec();
    if let (Some(want), [Seg::Key(s), Seg::Key(q), Seg::Index(i), ..]) = (label, segs.as_slice()) {
        if s == "song" && q == "sequence" {
            let items = a.cst.find_segs(&segs[..2]).node.map(|n| n.items()).unwrap_or_default();
            let label_of = |n: &crate::cst::Node| n.get("label").or_else(|| n.get("matrix")).and_then(|v| v.as_str()).unwrap_or("base").to_string();
            if items.get(*i).is_none_or(|it| label_of(&it.node) != want) {
                if let Some(j) = items.iter().position(|it| label_of(&it.node) == want) {
                    segs[2] = Seg::Index(j);
                }
            }
        }
    }
    let f = a.cst.find_segs(&segs);
    let (span, key) = match (f.member, f.node) {
        (Some(m), _) => (m.span(), m.key.span),
        (None, Some(n)) => (n.span, Span::new(n.span.start, n.span.start + 1)),
        (None, None) => return None,
    };
    Some(PathSpan { span, key, complete: f.complete })
}

/// Every site of the id under the cursor (its definition first when `with_definition`).
pub fn references(a: &Analysis, offset: usize, with_definition: bool) -> Vec<Span> {
    let Some(r) = a.refs.at(offset) else { return Vec::new() };
    let mut out: Vec<(bool, Span)> = a.refs.sites(r).filter(|x| with_definition || !x.def).map(|x| (!x.def, x.span)).collect();
    out.sort_by_key(|(is_use, s)| (*is_use, s.start));
    out.into_iter().map(|(_, s)| s).collect()
}

/// The id under the cursor and its current name, when it can be renamed.
pub fn prepare_rename(a: &Analysis, offset: usize) -> Option<(Span, String)> {
    let r = a.refs.at(offset)?;
    a.refs.def_of(r)?;
    Some((r.span, r.id.clone()))
}

/// Rename the id under the cursor everywhere — its key and every use (`source`s, `dest` and bound
/// path segments, send keys, binds, pool entries, song layers).
pub fn rename(a: &Analysis, offset: usize, new_name: &str) -> Result<Vec<TextEdit>, String> {
    let r = a.refs.at(offset).ok_or("nothing to rename here")?;
    a.refs.def_of(r).ok_or_else(|| format!("{} `{}` isn't defined in this doc", r.kind.noun(), r.id))?;
    if new_name.is_empty() {
        return Err("a name can't be empty".into());
    }
    if new_name.contains('.') && r.kind != IdKind::Layer {
        return Err("ids are path segments (`instruments.<id>.params…`) — no dots".into());
    }
    if new_name != r.id && a.refs.ids(r.kind, r.scope.as_deref()).contains(&new_name) {
        return Err(format!("there's already a {} `{new_name}`", r.kind.noun()));
    }
    Ok(a.refs
        .sites(r)
        .map(|site| {
            let text = match site.form {
                Form::BareKey if is_identifier(new_name) => new_name.to_string(),
                Form::BareKey => serde_json::Value::String(new_name.to_string()).to_string(),
                Form::Quoted(q) => new_name.replace('\\', "\\\\").replace(q, &format!("\\{q}")),
            };
            TextEdit::new(site.span, text)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::edit::apply;

    const DOC: &str = r#"{
  instruments: { pad: { params: { cutoff_hz: 800 }, sends_db: { rv: -7.96 } } },
  buses: { rv: { return_gain_db: -6.02 } },
  modulators: { lfo: { type: "lfo" } },
  matrix: { m_cut: { source: "lfo", dest: "instruments.pad.params.cutoff_hz" } },
  safe_bounds: { "instruments.pad.params.cutoff_hz": { min: 200 } },
  movement: { bind: { oneshot: ["lfo"] } },
  song: { matrices: { deep: { m_trim: { source: "lfo", dest: "instruments.pad.params.cutoff_hz", kind: "mod", depth_oct: -0.664 } } } },
}"#;

    fn at(needle: &str, delta: usize) -> usize {
        DOC.find(needle).unwrap() + delta
    }

    #[test]
    fn a_path_resolves_to_its_key_and_member_and_a_song_entry_follows_its_label() {
        const SONG: &str = r#"{
  global: { master_gain_db: -13.98 },
  song: { sequence: [ { loops: 2, label: "intro" }, { matrix: "deep", loops: 4 }, { loops: 1 } ] },
}"#;
        let a = Analysis::new(SONG);
        let key = |s: &str| Seg::Key(s.into());
        let p = path_span(&a, &[key("global")], None).unwrap();
        assert_eq!((p.key.text(SONG), p.span.text(SONG), p.complete), ("global", "global: { master_gain_db: -13.98 }", true));
        let seq = |i: usize| [key("song"), key("sequence"), Seg::Index(i)];
        // By index, the item's `{`; the label agrees, so the index stands.
        let p = path_span(&a, &seq(0), Some("intro")).unwrap();
        assert_eq!((p.key.text(SONG), p.span.text(SONG)), ("{", "{ loops: 2, label: \"intro\" }"));
        // The panel's index is stale (a block was inserted before it): the label finds the entry —
        // by its `matrix` when it has no label, `base` when it has neither.
        assert_eq!(path_span(&a, &seq(0), Some("deep")).unwrap().span.text(SONG), "{ matrix: \"deep\", loops: 4 }");
        assert_eq!(path_span(&a, &seq(1), Some("base")).unwrap().span.text(SONG), "{ loops: 1 }");
        // A label nobody has: the index stands.
        assert_eq!(path_span(&a, &seq(2), Some("gone")).unwrap().span.text(SONG), "{ loops: 1 }");
        // A missing tail: the deepest prefix.
        let p = path_span(&a, &[key("global"), key("nope")], None).unwrap();
        assert_eq!((p.key.text(SONG), p.complete), ("global", false));
        assert!(path_span(&Analysis::new(""), &[key("global")], None).is_none());
    }

    #[test]
    fn definitions_and_references() {
        let a = Analysis::new(DOC);
        assert_eq!(definition(&a, at("\"lfo\", dest", 2)).map(|s| s.start), Some(at("lfo: {", 0)));
        assert_eq!(definition(&a, at("rv: -7.96", 0)).map(|s| s.start), Some(at("rv: { return", 0)));
        // A dest's param segment goes to the authored param — in `matrix` and in a song layer's route.
        assert_eq!(definition(&a, at("params.cutoff_hz\" }", 8)).map(|s| s.text(DOC)), Some("cutoff_hz"));
        assert_eq!(definition(&a, at("params.cutoff_hz\", kind", 8)).map(|s| s.text(DOC)), Some("cutoff_hz"));
        // A song layer route's source / dest ids go to their definitions.
        assert_eq!(definition(&a, at("m_trim: { source: \"lfo", 19)).map(|s| s.start), Some(at("lfo: {", 0)));
        assert_eq!(definition(&a, at("pad.params.cutoff_hz\", kind", 1)).map(|s| s.start), Some(at("pad: {", 0)));
        let refs = references(&a, at("lfo: {", 1), true);
        assert_eq!(refs.len(), 4);
        assert_eq!(refs[0].start, at("lfo: {", 0)); // the definition first
        assert_eq!(references(&a, at("lfo: {", 1), false).len(), 3); // two sources + a bind
    }

    #[test]
    fn rename_reaches_every_use() {
        let a = Analysis::new(DOC);
        let out = apply(DOC, &rename(&a, at("pad", 1), "halo").unwrap());
        assert!(out.contains("halo: { params") && out.contains("\"instruments.halo.params.cutoff_hz\" }") && out.contains("\"instruments.halo.params.cutoff_hz\": {"), "{out}");
        assert!(out.contains("\"instruments.halo.params.cutoff_hz\", kind") && !out.contains("pad"), "the song layer's route too: {out}");
        let out = apply(DOC, &rename(&a, at("[\"lfo\"]", 2), "breath").unwrap());
        assert!(out.contains("breath: { type") && out.contains("source: \"breath\"") && out.contains("[\"breath\"]"), "{out}");
        assert_eq!(out.matches("source: \"breath\"").count(), 2, "the song layer's route too: {out}");
        // A bare key that stops being an identifier gets quoted; uses inside strings don't.
        let out = apply(DOC, &rename(&a, at("rv: { return", 0), "big-room").unwrap());
        assert!(out.contains("\"big-room\": -7.96") && out.contains("\"big-room\": { return"), "{out}");
        assert!(rename(&a, at("pad", 1), "x.y").is_err());
        assert!(rename(&a, at("pad", 1), "").is_err());
        assert_eq!(prepare_rename(&a, at("lfo: {", 1)).map(|(_, n)| n).as_deref(), Some("lfo"));
        assert!(prepare_rename(&a, at("cutoff_hz: 800", 1)).is_none());
    }

    /// Renaming an instrument or a modulator a song layer's full route entry names leaves a doc the
    /// strict check (and so engines-live / engines-check) still accepts.
    #[test]
    fn a_rename_through_a_song_layer_route_keeps_the_doc_valid() {
        const SONG: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 8 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -10.46, cutoff_hz: 800 } } },
  modulators: {
    seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] },
    unity: { type: "lfo", output: "control", rate_hz: 0.05 },
  },
  matrix: { m_pad: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
  song: {
    matrices: { deep: { m_trim: { source: "unity", dest: "instruments.pad.params.level_db", kind: "mod", depth_db: -3.52 } } },
    sequence: [ { loops: 2 }, { matrix: "deep", loops: 2 } ],
    transition_s: 1,
  },
}"#;
        let errors = |src: &str| -> Vec<String> {
            engines_core::check::check_str(src).diags.into_iter().filter(|d| d.severity == engines_core::check::Severity::Error).map(|d| d.message).collect()
        };
        assert_eq!(errors(SONG), Vec::<String>::new());
        let a = Analysis::new(SONG);
        for (id, new) in [("pad: {", "drone"), ("unity: {", "one")] {
            let out = apply(SONG, &rename(&a, SONG.find(id).unwrap(), new).unwrap());
            assert_eq!(errors(&out), Vec::<String>::new(), "{id} → {new}:\n{out}");
        }
        // A song whose layers carry full trim entries (`m_hum_trim`), like the house piece the bug was
        // found on (the neutral test song).
        let house = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab/tests/fixtures/song_lydian_tide.json5")).unwrap();
        let a = Analysis::new(house.as_str());
        for (id, new) in [("hum", "humx"), ("unity", "one")] {
            let def = a.refs.refs.iter().find(|r| r.def && r.id == id).unwrap_or_else(|| panic!("{id} defined"));
            let out = apply(&house, &rename(&a, def.span.start, new).unwrap());
            assert_eq!(errors(&out), Vec::<String>::new(), "{id} → {new}");
        }
    }
}
