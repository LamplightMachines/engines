//! Diagnostics: syntax errors, the strict checker's findings put on the exact key or value they
//! name, and out-of-scale note offsets as hints.

use super::{music, note_sites, Analysis};
use crate::cst::{Cst, Span};
use engines_core::check::{DocDiag, Severity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
    Hint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub span: Span,
    pub level: Level,
    /// `syntax`, the checker's code (`unknown-key`, `enum`, `dangling`, …), or `out-of-scale`.
    pub code: String,
    pub message: String,
    /// The checker's JSONPath (empty for syntax errors and doc-wide findings).
    pub path: String,
}

pub fn diagnostics(a: &Analysis) -> Vec<Diag> {
    let mut out: Vec<Diag> = a
        .cst
        .errors
        .iter()
        .map(|e| Diag { span: e.span, level: Level::Error, code: "syntax".into(), message: e.message.clone(), path: String::new() })
        .collect();
    if let Some(checked) = &a.checked {
        for d in &checked.diags {
            let level = match d.severity {
                Severity::Error => Level::Error,
                Severity::Warning => Level::Warning,
            };
            out.push(Diag { span: locate(a, d), level, code: d.code.clone(), message: d.message.clone(), path: d.path.clone() });
        }
    }
    out.extend(scale_hints(a));
    out
}

/// Where a checker finding goes: an unknown/forbidden/repeated key on the key (a repeat on its last
/// occurrence, the one that wins), a missing key on its object's
/// key, anything else on the value it names (a container's key); a path that no longer fully exists
/// (a missing `note_engine`) on its deepest existing key; doc-wide findings on the opening brace.
pub fn locate(a: &Analysis, d: &DocDiag) -> Span {
    locate_in(&a.cst, d)
}

/// [`locate`] on a bare CST (what `engines-check` uses for its line:column).
pub fn locate_in(cst: &Cst, d: &DocDiag) -> Span {
    let root_brace = cst.root.as_ref().map(|r| Span::new(r.span.start, r.span.start + 1)).unwrap_or_default();
    if d.path.is_empty() {
        return root_brace;
    }
    let f = cst.find(&d.path);
    if f.complete {
        if let (Some(m), "unknown-key" | "not-allowed" | "missing" | "duplicate") = (f.member, d.code.as_str()) {
            return m.key.span;
        }
        return f.focus().unwrap_or(root_brace);
    }
    f.member.map(|m| m.key.span).unwrap_or(root_brace)
}

/// A note offset outside `harmony.mode`: not an error (borrowed colour is a choice) — a hint.
fn scale_hints(a: &Analysis) -> Vec<Diag> {
    let (Some(key), Some(root)) = (a.key.as_ref(), a.root()) else { return Vec::new() };
    note_sites(root)
        .into_iter()
        .filter(|s| s.offset.is_some() && !music::in_scale(key, s.semitones()))
        .map(|s| {
            let o = s.semitones();
            Diag {
                span: s.anchor(),
                level: Level::Hint,
                code: "out-of-scale".into(),
                message: format!("{} ({}) is outside {}", music::note_of(key, o, 0.0), music::degree_of(key, o), key.name()),
                path: String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: -7.96, cutof_hz: 800 } } },
  modulators: {
    seq: { type: "step_seq", output: "note", length: 4,
           steps: [ { step: 0, voicing: { v1: { offset: 0 }, v2: { offset: 4 } } } ] },
  },
  matrix: { m_notes: { source: "seqq", dest: "instruments.pad.notes", kind: "note", voices: 2 } },
}"#;

    #[test]
    fn findings_land_on_the_exact_token() {
        let a = Analysis::new(DOC);
        let diags = diagnostics(&a);
        let at = |code: &str| diags.iter().find(|d| d.code == code).unwrap_or_else(|| panic!("no {code} in {diags:#?}"));
        assert_eq!(at("unknown-key").span.text(DOC), "cutof_hz");
        assert!(at("unknown-key").message.contains("did you mean `cutoff_hz`"));
        assert_eq!(at("dangling").span.text(DOC), "\"seqq\"");
        let hint = at("out-of-scale");
        assert_eq!(hint.level, Level::Hint);
        assert_eq!(hint.span.text(DOC), "4");
        assert_eq!(hint.message, "F#4 (3) is outside D dorian");
    }

    #[test]
    fn a_midi_instrument_is_said_to_make_no_sound_in_the_player() {
        // The checker's warning reaches the editor, on the instrument's `type`.
        let src = DOC.replace("instruments: {", "instruments: {\n    ext: { type: \"midi\", note_engine: { polyphony: 1 }, routing: { channel: 2 } },");
        let diags = diagnostics(&Analysis::new(src.as_str()));
        let d = diags.iter().find(|d| d.code == "silent" && d.message.contains("makes no sound in `engines-live`")).unwrap_or_else(|| panic!("{diags:#?}"));
        assert_eq!(d.level, Level::Warning);
        assert_eq!(d.span.text(&src), "\"midi\"");
    }

    #[test]
    fn a_repeated_key_is_an_error_on_the_repeat() {
        // JSON5 keeps the last of a repeated key — the earlier block is silently gone, so say so.
        let src = DOC.replace("cutof_hz: 800", "cutoff_hz: 800, cutoff_hz: 1200");
        let a = Analysis::new(src.as_str());
        let d = diagnostics(&a).into_iter().find(|d| d.code == "duplicate").expect("a duplicate diagnostic");
        assert_eq!(d.level, Level::Error);
        assert_eq!(d.path, "instruments.pad.params.cutoff_hz");
        assert_eq!(d.span.start, src.rfind("cutoff_hz").unwrap(), "on the repeat");
    }

    #[test]
    fn syntax_errors_are_located_and_the_rest_still_works() {
        let src = "{ harmony: { root: \"D\", mode: \"dorian\" }, x: [1 2] }";
        let a = Analysis::new(src);
        let d = diagnostics(&a);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].code.as_str(), d[0].span), ("syntax", Span::new(src.find(" 2").unwrap(), src.find(" 2").unwrap())));
        assert!(a.checked.is_none() && a.key.is_some());
    }

    #[test]
    fn a_missing_node_points_at_its_deepest_key() {
        let src = DOC.replace("note_engine: { polyphony: 2 }, ", "").replace("cutof_hz", "cutoff_hz");
        let a = Analysis::new(src.as_str());
        let d = diagnostics(&a);
        let silent = d.iter().find(|d| d.code == "silent").expect("silent warning");
        assert_eq!(silent.span.text(&src), "pad");
    }
}
