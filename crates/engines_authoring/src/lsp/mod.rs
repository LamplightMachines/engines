//! # `engines-lsp` — the loop-doc language server (ADR 0037)
//!
//! Everything here works on one [`Analysis`] of a doc's text and answers in **byte spans**; only
//! [`server`] speaks LSP (positions in UTF-16, JSON-RPC over stdio). That keeps every feature a pure
//! function, testable with a string and an offset:
//!
//! - [`diagnostics`] — syntax errors (located, recovered), the strict checker's findings located by
//!   their JSONPath, and out-of-scale note offsets as hints;
//! - [`complete`] — keys (params filtered by synth type), enum values, ids (`source`, `dest` paths,
//!   sends, binds, pools), and scale-aware `offset`s (`7 · A4 · 5 · D dorian`);
//! - [`hover`] — schema docs, native units, note/degree/chord, the just-intonation harmonic a
//!   `cents_offset` tunes to, loop length and cycles;
//! - [`inlay`] — note names, native units, loop length, modulator cycles, the polymeter;
//! - [`lens`] — solo/mute per instrument, audition/play-from per song block (commands the editor
//!   extension forwards to `engines-live`);
//! - [`actions`] — did-you-mean fixes, snap to the scale, transpose by scale degrees, drop an unused
//!   band;
//! - [`navigate`] — go to definition, find references, rename an id everywhere it's used;
//! - [`steps`] — where each step of a step sequence is written (the extension highlights the step
//!   `engines-live` is playing; custom request `engines/stepRanges`).

pub mod actions;
pub mod complete;
pub mod diagnostics;
pub mod hover;
pub mod inlay;
pub mod lens;
pub mod music;
pub mod navigate;
pub mod readings;
pub mod refs;
pub mod schema_nav;
pub mod server;
pub mod steps;

use crate::cst::lines::LineIndex;
use crate::cst::{Cst, Node, Span};
use engines_core::analysis::{polymeter, PartCycle, Polymeter};
use engines_core::check::{check_value, duplicate_key, duplicate_keys, Checked};
use engines_core::loop_doc::LoopDoc;
use engines_core::theory::Key;

/// One analysed revision of a doc.
pub struct Analysis {
    pub text: String,
    pub lines: LineIndex,
    pub cst: Cst,
    /// The strict check — run only when the text parses (`None` while it has syntax errors).
    pub checked: Option<Checked>,
    /// The doc's key (`harmony.root` + `mode`), read from the tree so it survives syntax errors
    /// elsewhere.
    pub key: Option<Key>,
    /// Every id definition and reference.
    pub refs: refs::Index,
    /// The polymeter, when the doc deserializes.
    pub poly: Option<Polymeter>,
}

impl Analysis {
    pub fn new(text: impl Into<String>) -> Analysis {
        let text = text.into();
        let lines = LineIndex::new(&text);
        let cst = Cst::parse(&text);
        // The CST's value keeps a repeated key's last occurrence, like JSON5: report the repeats.
        let checked = cst.to_doc_value().map(check_value).map(|mut c| {
            c.diags.extend(duplicate_keys(&text).iter().map(|p| duplicate_key(p)));
            c.diags.sort_by_key(|d| d.severity);
            c
        });
        let key = cst.root.as_ref().and_then(|r| {
            let h = r.get("harmony")?;
            Key::new(h.get("root")?.as_str()?, h.get("mode")?.as_str()?)
        });
        let refs = refs::Index::build(&text, &cst);
        let poly = checked.as_ref().and_then(|c| c.doc.as_ref()).map(polymeter);
        Analysis { text, lines, cst, checked, key, refs, poly }
    }

    /// Carry the previous revision's polymeter while this one doesn't analyse (mid-keystroke), so the
    /// cycle hints and loop summaries don't flicker away while a key is half-typed.
    pub fn with_fallback(mut self, previous: Option<&Analysis>) -> Analysis {
        if self.poly.is_none() {
            self.poly = previous.and_then(|p| p.poly.clone());
        }
        self
    }

    /// The deserialized doc (when it parses and has the engine's shape — warnings and even errors
    /// allowed).
    pub fn doc(&self) -> Option<&LoopDoc> {
        self.checked.as_ref()?.doc.as_ref()
    }

    pub fn root(&self) -> Option<&Node> {
        self.cst.root.as_ref()
    }

    /// The node at a checker path, when the whole path exists.
    pub fn node(&self, path: &str) -> Option<&Node> {
        let f = self.cst.find(path);
        f.node.filter(|_| f.complete)
    }

    /// A modulator's polymeter part.
    pub fn part(&self, id: &str) -> Option<&PartCycle> {
        self.poly.as_ref()?.parts.iter().find(|p| p.id == id)
    }

    /// The loop length in seconds — from the analysed doc, else straight from the tree's transport
    /// (beats), so it shows while the doc has errors.
    pub fn loop_seconds(&self) -> Option<f64> {
        if let Some(p) = &self.poly {
            return Some(p.loop_seconds);
        }
        let t = self.root()?.get("transport")?;
        let (bpm, steps) = (t.get("bpm")?.as_f64()?, t.get("master_steps")?.as_f64()?);
        (bpm > 0.0).then(|| steps * 60.0 / bpm)
    }

    /// `harmony.mode`'s display name with the tonic (`D dorian`), or `"the doc's scale"`.
    pub fn key_name(&self) -> String {
        self.key.map(|k| k.name()).unwrap_or_else(|| "the doc's scale".into())
    }
}

/// Where a note pitch is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    /// A step voice: `modulators.<m>.steps[i].voicing.<v>`.
    Step,
    /// A phrase note: `sequence_pools.<p>.notes[i]` (or a realized phrase).
    Phrase,
    /// A `pitch_walk.pool` entry.
    Walk,
}

/// One authored note pitch: the voice object (or pool entry), its `offset` node (absent = 0), and
/// its `cents_offset`.
#[derive(Clone, Copy, Debug)]
pub struct NoteSite<'a> {
    pub kind: NoteKind,
    pub voice: &'a Node,
    pub offset: Option<&'a Node>,
    pub cents: f64,
    /// The step (for a step voice) or the phrase (for a phrase note) it belongs to.
    pub group: &'a Node,
}

impl NoteSite<'_> {
    pub fn semitones(&self) -> f64 {
        self.offset.and_then(Node::as_f64).unwrap_or(0.0)
    }
    /// Where to anchor a label: the offset value, else the voice's opening brace.
    pub fn anchor(&self) -> Span {
        self.offset.map(|o| o.span).unwrap_or(Span::new(self.voice.span.start, self.voice.span.start + 1))
    }
}

/// Every note pitch in the doc, in source order.
pub fn note_sites(root: &Node) -> Vec<NoteSite<'_>> {
    let mut out = Vec::new();
    for m in root.get("modulators").map(Node::members).unwrap_or_default() {
        let Some(modv) = &m.value else { continue };
        for step in modv.get("steps").map(Node::items).unwrap_or_default() {
            for v in step.node.get("voicing").map(Node::members).unwrap_or_default() {
                if let Some(voice) = &v.value {
                    out.push(site(NoteKind::Step, voice, &step.node));
                }
            }
        }
        for it in modv.get("pitch_walk").and_then(|w| w.get("pool")).map(Node::items).unwrap_or_default() {
            out.push(NoteSite { kind: NoteKind::Walk, voice: &it.node, offset: Some(&it.node), cents: 0.0, group: modv });
        }
        if let Some(realized) = modv.get("phrase").and_then(|p| p.get("realized")) {
            for n in realized.get("notes").map(Node::items).unwrap_or_default() {
                out.push(site(NoteKind::Phrase, &n.node, realized));
            }
        }
    }
    for p in root.get("sequence_pools").map(Node::members).unwrap_or_default() {
        let Some(phrase) = &p.value else { continue };
        for n in phrase.get("notes").map(Node::items).unwrap_or_default() {
            out.push(site(NoteKind::Phrase, &n.node, phrase));
        }
    }
    out.sort_by_key(|s| s.voice.span.start);
    out
}

fn site<'a>(kind: NoteKind, voice: &'a Node, group: &'a Node) -> NoteSite<'a> {
    NoteSite { kind, voice, offset: voice.get("offset"), cents: voice.get("cents_offset").and_then(Node::as_f64).unwrap_or(0.0), group }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn loop_docs() -> Vec<(String, String)> {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut out = Vec::new();
        for dir in ["lab/tests/fixtures", "doc"] {
            for e in std::fs::read_dir(repo.join(dir)).unwrap().flatten() {
                let p = e.path();
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                if (name.starts_with("loop") || name.starts_with("song")) && !name.contains("objectives") && (name.ends_with(".json5") || name.ends_with(".json")) {
                    out.push((name, std::fs::read_to_string(&p).unwrap()));
                }
            }
        }
        assert!(out.len() >= 15);
        out
    }

    /// Every feature at (nearly) every offset of every tracked loop doc: nothing panics, every span
    /// stays inside the text, and the docs check clean.
    #[test]
    fn every_feature_everywhere_on_every_doc() {
        for (name, text) in loop_docs() {
            let a = Analysis::new(text.as_str());
            assert!(a.cst.errors.is_empty(), "{name}");
            assert!(!a.checked.as_ref().unwrap().has_errors(), "{name}: {:?}", a.checked.as_ref().unwrap().diags);
            let inside = |s: Span| s.start <= s.end && s.end <= text.len();
            for d in diagnostics::diagnostics(&a) {
                assert!(inside(d.span), "{name}: {d:?}");
            }
            for h in inlay::inlays(&a, Span::new(0, text.len()), &inlay::InlayConfig::default()) {
                assert!(h.offset <= text.len() && !h.label.is_empty(), "{name}: {h:?}");
            }
            assert!(lens::lenses(&a).iter().all(|l| inside(l.span)));
            let offsets: Vec<usize> = a.cst.tokens.iter().flat_map(|t| [t.span.start, t.span.start + 1, t.span.end]).filter(|&o| o <= text.len() && text.is_char_boundary(o)).step_by(13).collect();
            for o in offsets {
                for i in complete::complete(&a, o) {
                    assert!(inside(i.replace), "{name}@{o}: {i:?}");
                }
                if let Some(h) = hover::hover(&a, o) {
                    assert!(inside(h.span) && !h.markdown.is_empty(), "{name}@{o}");
                }
                for x in actions::actions(&a, Span::new(o, o)) {
                    assert!(x.edits.iter().all(|e| inside(e.span)), "{name}@{o}: {x:?}");
                }
                let _ = navigate::definition(&a, o);
                let _ = navigate::references(&a, o, true);
            }
        }
    }

    /// Typing a doc from scratch: every prefix of a real doc analyses and completes without panicking.
    #[test]
    fn every_prefix_of_a_doc_is_survivable() {
        let (_, text) = loop_docs().into_iter().find(|(n, _)| n == "loop_drone_d_lydian.json5").unwrap();
        let bounds: Vec<usize> = (0..=text.len()).filter(|&i| text.is_char_boundary(i)).step_by(97).collect();
        for end in bounds {
            let a = Analysis::new(&text[..end]);
            let _ = diagnostics::diagnostics(&a);
            let _ = complete::complete(&a, end);
            let _ = hover::hover(&a, end.saturating_sub(1));
            let _ = inlay::inlays(&a, Span::new(0, end), &inlay::InlayConfig::default());
        }
    }
}
