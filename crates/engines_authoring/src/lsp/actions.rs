//! Code actions: one-click fixes for the checker's did-you-means, snapping an out-of-scale note to
//! the scale, transposing a step / phrase / modulator by scale degrees, and dropping a bound that
//! names nothing, and migrating a schema v0.8 doc (ADR 0054 U6). Every edit goes through the CST's
//! comment-preserving edit ops. Plus the source actions **Add part: …** (the parts library, ADR 0039 —
//! [`add_part_actions`]).

use super::diagnostics::{diagnostics, Diag};
use super::{music, note_sites, Analysis, NoteSite};
use crate::cst::edit::{string_as, TextEdit};
use crate::cst::path::{render, Seg, Target};
use crate::cst::{Node, NodeKind, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    QuickFix,
    Refactor,
    /// `source.addPart` — offered on request (the editor's Source Action menu), not in the lightbulb.
    AddPart,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub title: String,
    pub kind: ActionKind,
    pub edits: Vec<TextEdit>,
    /// The diagnostic it fixes.
    pub fixes: Option<Diag>,
    pub preferred: bool,
}

pub fn actions(a: &Analysis, range: Span) -> Vec<Action> {
    let mut out = Vec::new();
    let overlaps = |s: Span| s.start <= range.end && range.start <= s.end;
    for d in diagnostics(a).into_iter().filter(|d| overlaps(d.span)) {
        if let Some(fix) = did_you_mean(a, &d) {
            out.push(fix);
        }
        if d.code == "out-of-scale" {
            out.extend(snap(a, &d));
        }
        if d.code == "version" {
            out.extend(migrate(a, &d));
        }
        if d.code == "dangling" && (d.path.starts_with("safe_bounds.") || d.path.starts_with("constraints.")) {
            if let Some(edits) = a.cst.remove_member(&a.text, &d.path) {
                out.push(Action { title: "Remove the unused band".into(), kind: ActionKind::QuickFix, edits, fixes: Some(d.clone()), preferred: false });
            }
        }
    }
    out.extend(transpose(a, range.start));
    out
}

/// **Add part: …** for every part of the library that fits the doc. A part that acts on an
/// existing instrument (`breath`, `shimmer`) takes the one the cursor is in when it fits, else the
/// first that does. The edit is a minimal diff (insertions where the sections are).
pub fn add_part_actions(a: &Analysis, at: usize) -> Vec<Action> {
    let here = match a.cst.locate(&a.text, at).segs.as_slice() {
        [Seg::Key(s), Seg::Key(id), ..] if s == "instruments" => Some(id.clone()),
        _ => None,
    };
    crate::parts::library()
        .into_iter()
        .filter_map(|p| {
            let mut opts = crate::parts::Options::default();
            if p.target.is_some() {
                opts.target = here.clone();
            }
            let added = crate::parts::add(&a.text, &p, &opts).or_else(|_| crate::parts::add(&a.text, &p, &crate::parts::Options::default())).ok()?;
            let on = added.target.as_ref().map(|t| format!(" — on `{t}`")).unwrap_or_default();
            Some(Action { title: format!("Add part: {}{on}", p.summary), kind: ActionKind::AddPart, edits: crate::parts::diff_edits(&a.text, &added.text), fixes: None, preferred: false })
        })
        .collect()
}

/// The fix a checker message suggests: `… — did you mean `cutoff_hz`?` / `… did you mean "dorian"?`.
fn suggestion(message: &str) -> Option<&str> {
    let rest = &message[message.find("did you mean ")? + "did you mean ".len()..];
    let q = rest.chars().next().filter(|c| *c == '`' || *c == '"')?;
    let body = &rest[1..];
    Some(&body[..body.find(q)?])
}

/// The first backticked word of a message (`unknown instrument `pdd`` → `pdd`).
fn first_ticked(message: &str) -> Option<&str> {
    let start = message.find('`')? + 1;
    let len = message[start..].find('`')?;
    Some(&message[start..start + len])
}

fn did_you_mean(a: &Analysis, d: &Diag) -> Option<Action> {
    let fix = suggestion(&d.message)?;
    let edits = if d.code == "unknown-key" {
        a.cst.rename_key(&d.path, fix)?
    } else {
        // A string value: the whole string, or the one path segment that's wrong.
        let f = a.cst.find(&d.path);
        let node = f.node.filter(|n| f.complete && n.span == d.span)?;
        let NodeKind::String(s) = &node.kind else { return None };
        let quote = a.text[node.span.start..].chars().next().unwrap_or('"');
        let new = match first_ticked(&d.message) {
            Some(wrong) if s.split('.').any(|seg| seg == wrong) && s != wrong => s.split('.').map(|seg| if seg == wrong { fix } else { seg }).collect::<Vec<_>>().join("."),
            _ => fix.to_string(),
        };
        vec![TextEdit::new(node.span, string_as(&new, quote))]
    };
    Some(Action { title: format!("Change to `{fix}`"), kind: ActionKind::QuickFix, edits, fixes: Some(d.clone()), preferred: true })
}

/// The scale tones on either side of an out-of-scale offset.
fn snap(a: &Analysis, d: &Diag) -> Vec<Action> {
    let Some(key) = &a.key else { return Vec::new() };
    let Some(o) = d.span.text(&a.text).trim().parse::<f64>().ok() else { return Vec::new() };
    let o = o.round() as i32;
    let below = (1..12).map(|k| o - k).find(|&x| music::in_scale(key, x as f64));
    let above = (1..12).map(|k| o + k).find(|&x| music::in_scale(key, x as f64));
    [below, above]
        .into_iter()
        .flatten()
        .map(|t| Action {
            title: format!("Snap to {} ({}) in {}", music::note_of(key, t as f64, 0.0), music::degree_of(key, t as f64), key.name()),
            kind: ActionKind::QuickFix,
            edits: vec![TextEdit::new(d.span, t.to_string())],
            fixes: Some(d.clone()),
            preferred: false,
        })
        .collect()
}

/// The next scale tone above (`up`) or below an offset.
fn step_degree(a: &Analysis, o: f64, up: bool) -> Option<i32> {
    let key = a.key.as_ref()?;
    let o = o.round() as i32;
    (1..=12).map(|k| if up { o + k } else { o - k }).find(|&x| music::in_scale(key, x as f64))
}

/// Transpose the notes of whatever the cursor is in — a step, a phrase, or (on its key) a whole
/// modulator — up or down one scale degree.
fn transpose(a: &Analysis, offset: usize) -> Vec<Action> {
    let (Some(root), Some(key)) = (a.root(), a.key.as_ref()) else { return Vec::new() };
    let cursor = a.cst.locate(&a.text, offset);
    let keys: Vec<&str> = cursor.segs.iter().map(|s| if let Seg::Key(k) = s { k.as_str() } else { "" }).collect();
    let sites = note_sites(root);
    let (what, picked): (&str, Vec<&NoteSite>) = match keys.as_slice() {
        ["modulators", _] if matches!(cursor.target, Target::Key { .. }) => {
            let m = a.node(&render(&cursor.segs));
            ("modulator", sites.iter().filter(|s| m.is_some_and(|m| m.span.start <= s.voice.span.start && s.voice.span.end <= m.span.end)).collect())
        }
        ["modulators", _, "steps", _, ..] => {
            let step = a.node(&render(&cursor.segs[..4]));
            ("step", sites.iter().filter(|s| step.is_some_and(|st| std::ptr::eq(s.group, st))).collect())
        }
        ["sequence_pools", _, ..] => {
            let phrase = a.node(&render(&cursor.segs[..2]));
            ("phrase", sites.iter().filter(|s| phrase.is_some_and(|p| std::ptr::eq(s.group, p))).collect())
        }
        _ => return Vec::new(),
    };
    if picked.is_empty() {
        return Vec::new();
    }
    [(true, "up"), (false, "down")]
        .into_iter()
        .filter_map(|(up, word)| {
            let mut edits = Vec::new();
            for s in &picked {
                let new = step_degree(a, s.semitones(), up)?;
                match s.offset {
                    Some(o) => edits.push(TextEdit::new(o.span, new.to_string())),
                    None => edits.extend(insert_offset(a, s.voice, new)?),
                }
            }
            Some(Action { title: format!("Transpose {what} {word} a degree of {}", key.name()), kind: ActionKind::Refactor, edits, fixes: None, preferred: false })
        })
        .collect()
}

/// **Migrate to schema v0.9**: the doc through `engines-migrate`'s conversion (comments kept), as one
/// minimal edit. Only a v0.8 doc migrates; another version is refused by the checker without a fix.
fn migrate(a: &Analysis, d: &Diag) -> Option<Action> {
    let m = crate::migrate::migrate(&a.text).ok().filter(|m| !m.unchanged)?;
    let notes = if m.notes.is_empty() { String::new() } else { format!(" — {} note{} to check", m.notes.len(), if m.notes.len() == 1 { "" } else { "s" }) };
    Some(Action { title: format!("Migrate to schema v0.9 (engines-migrate){notes}"), kind: ActionKind::QuickFix, edits: crate::parts::diff_edits(&a.text, &m.text), fixes: Some(d.clone()), preferred: true })
}

fn insert_offset(a: &Analysis, voice: &Node, new: i32) -> Option<Vec<TextEdit>> {
    a.cst.insert_into(&a.text, voice, "offset", &new.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::edit::apply;

    const DOC: &str = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  instruments: { pad: { note_engine: { polyphony: 3 }, params: { wave: "saw_sof", level_db: -7.96, cutof_hz: 800 } } },
  modulators: {
    seq: { type: "step_seq", output: "note", length: 4,
           steps: [ { step: 0, voicing: { v1: {}, v2: { offset: 4 }, v3: { offset: 7 } } } ] },
  },
  matrix: { m_notes: { source: "seq", dest: "instruments.pdd.notes", kind: "note", voices: 3 } },
  safe_bounds: { "instruments.pad.params.drive_db": { min: 5.58, max: 5.58 } },
}"#;

    fn fix(needle: &str, title_starts: &str) -> String {
        let a = Analysis::new(DOC);
        let at = DOC.find(needle).unwrap() + 1;
        let acts = actions(&a, Span::new(at, at));
        let act = acts.iter().find(|x| x.title.starts_with(title_starts)).unwrap_or_else(|| panic!("no `{title_starts}` in {:?}", acts.iter().map(|x| &x.title).collect::<Vec<_>>()));
        apply(DOC, &act.edits)
    }

    #[test]
    fn did_you_mean_fixes_keys_values_and_path_segments() {
        assert!(fix("cutof_hz", "Change to `cutoff_hz`").contains("cutoff_hz: 800"));
        assert!(fix("\"saw_sof\"", "Change to `saw_soft`").contains("wave: \"saw_soft\""));
        assert!(fix("instruments.pdd", "Change to `pad`").contains("dest: \"instruments.pad.notes\""));
        let out = fix("\"instruments.pad.params.drive_db\"", "Remove the unused band");
        assert!(out.contains("safe_bounds: {}"), "{out}");
    }

    #[test]
    fn snap_and_transpose_stay_in_the_scale() {
        let out = fix("4 }, v3", "Snap to F4 (♭3)");
        assert!(out.contains("v2: { offset: 3 }"), "{out}");
        assert!(fix("4 }, v3", "Snap to G4 (4)").contains("v2: { offset: 5 }"));
        // Transposing the step moves every voice one degree — the implicit `offset: 0` too.
        let out = fix("step: 0", "Transpose step up");
        assert!(out.contains("v1: { offset: 2 }, v2: { offset: 5 }, v3: { offset: 9 }"), "{out}");
        let out = fix("step: 0", "Transpose step down");
        assert!(out.contains("v1: { offset: -2 }, v2: { offset: 3 }, v3: { offset: 5 }"), "{out}");
        assert!(fix("seq: {", "Transpose modulator up").contains("v3: { offset: 9 }"));
    }

    #[test]
    fn a_v08_doc_migrates_in_one_fix() {
        let v08 = r#"{
  schema_version: "0.8",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  // the only voice
  instruments: { pad: { note_engine: { polyphony: 1 }, params: { wave: "sine", level: 0.5, cutoff: 0.5 } } },
  modulators: { seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v: { offset: 0 } } } ] } },
  matrix: { m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
}"#;
        let a = Analysis::new(v08);
        let at = v08.find("\"0.8\"").unwrap() + 1;
        let acts = actions(&a, Span::new(at, at));
        let fix = acts.iter().find(|x| x.title.starts_with("Migrate to schema v0.9")).unwrap_or_else(|| panic!("{:?}", acts.iter().map(|x| &x.title).collect::<Vec<_>>()));
        assert!(fix.preferred && fix.fixes.as_ref().is_some_and(|d| d.code == "version"));
        let out = apply(v08, &fix.edits);
        assert!(out.contains("level_db: -6.02") && out.contains("cutoff_hz: 800") && out.contains("// the only voice"), "{out}");
        assert!(engines_core::check::check_str(&out).errors().next().is_none(), "{out}");
        // A v0.9 doc has nothing to migrate.
        assert!(actions(&Analysis::new(out.as_str()), Span::new(0, 0)).iter().all(|x| !x.title.starts_with("Migrate")));
    }

    #[test]
    fn suggestions_parse_both_quote_styles() {
        assert_eq!(suggestion("unknown key `cutof_hz` — did you mean `cutoff_hz`?"), Some("cutoff_hz"));
        assert_eq!(suggestion("unknown mode \"dorain\" (…) — did you mean \"dorian\"?"), Some("dorian"));
        assert_eq!(suggestion("no hint here"), None);
    }

    #[test]
    fn add_part_actions_follow_the_cursor() {
        let doc = r#"{
  schema_version: "0.9",
  transport: { bpm: 60, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  instruments: {
    bell: { type: "modal", note_engine: { polyphony: 1 }, params: { level_db: -10.46 } },
    pad: { note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -10.46, cutoff_hz: 800 } },
    lead: { note_engine: { polyphony: 1 }, params: { wave: "sine", level_db: -10.46, cutoff_hz: 800 } },
  },
  modulators: { seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v: { offset: 0 } } } ] } },
  matrix: { m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
}"#;
        let a = Analysis::new(doc);
        let at = |needle: &str| doc.find(needle).unwrap() + 1;
        let titles = |acts: &[Action]| acts.iter().map(|x| x.title.clone()).collect::<Vec<_>>();
        let here = add_part_actions(&a, at("lead: {"));
        assert_eq!(here.len(), 9, "{:?}", titles(&here));
        assert!(here.iter().all(|x| x.kind == ActionKind::AddPart));
        let breath = here.iter().find(|x| x.title.starts_with("Add part: breath")).unwrap();
        assert!(breath.title.ends_with("— on `lead`"), "the instrument under the cursor: {}", breath.title);
        // In `bell` (modal: no cutoff) the breath falls back to the first that has one; the shimmer
        // send (any instrument) takes `bell`.
        let in_bell = add_part_actions(&a, at("bell: {"));
        assert!(titles(&in_bell).iter().any(|t| t.starts_with("Add part: breath") && t.ends_with("on `pad`")), "{:?}", titles(&in_bell));
        assert!(titles(&in_bell).iter().any(|t| t.starts_with("Add part: shimmer") && t.ends_with("on `bell`")));
        // Applied, the edit is the merge.
        let merged = apply(doc, &breath.edits);
        assert!(merged.contains("dest: \"instruments.lead.params.cutoff_hz\""), "{merged}");
        assert!(engines_core::check::check_str(&merged).errors().next().is_none());
        // Not part of the everyday actions.
        assert!(actions(&a, Span::new(at("lead: {"), at("lead: {"))).iter().all(|x| x.kind != ActionKind::AddPart));
    }
}
