//! The id index: where every instrument / modulator / bus / effect / pool / route / song-layer id is
//! **defined** (its key) and **referenced** (a `source`, a segment of a `dest` or bound path — in
//! `matrix` or a song layer's route entries — a send key, a bind, a pool entry, a song layer) — with
//! the exact byte range of the id text, so go to definition, find references and rename are lookups.

use crate::cst::{Cst, Key, Node, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IdKind {
    Instrument,
    Modulator,
    Bus,
    /// An effect on a bus (ids are per bus: [`IdRef::scope`] names it).
    Effect,
    /// A `sequence_pools` entry.
    Pool,
    /// A `matrix` route.
    Route,
    /// A `song.matrices` layer.
    Layer,
}

impl IdKind {
    pub fn noun(self) -> &'static str {
        match self {
            IdKind::Instrument => "instrument",
            IdKind::Modulator => "modulator",
            IdKind::Bus => "bus",
            IdKind::Effect => "effect",
            IdKind::Pool => "sequence pool",
            IdKind::Route => "route",
            IdKind::Layer => "song layer",
        }
    }
}

/// How the id is written — what a rename has to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// A bare key: a non-identifier new name needs quotes around the whole key.
    BareKey,
    /// Inside quotes (a quoted key, a string value, or a segment of a path string).
    Quoted(char),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdRef {
    pub kind: IdKind,
    pub id: String,
    /// The owning bus of an effect id.
    pub scope: Option<String>,
    /// The id's own text.
    pub span: Span,
    pub form: Form,
    /// The definition (an id key) rather than a use.
    pub def: bool,
}

impl IdRef {
    pub fn same_id(&self, other: &IdRef) -> bool {
        self.kind == other.kind && self.id == other.id && self.scope == other.scope
    }
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub refs: Vec<IdRef>,
}

impl Index {
    pub fn build(src: &str, cst: &Cst) -> Index {
        let mut b = Builder { src, refs: Vec::new() };
        if let Some(root) = &cst.root {
            b.walk(root);
        }
        b.refs.sort_by_key(|r| r.span.start);
        Index { refs: b.refs }
    }

    /// The id under (or touching) `offset`.
    pub fn at(&self, offset: usize) -> Option<&IdRef> {
        self.refs.iter().find(|r| r.span.touches(offset))
    }

    /// The definition of what `r` names.
    pub fn def_of(&self, r: &IdRef) -> Option<&IdRef> {
        self.refs.iter().find(|d| d.def && d.same_id(r))
    }

    /// Every site (definition included) of what `r` names.
    pub fn sites<'a>(&'a self, r: &'a IdRef) -> impl Iterator<Item = &'a IdRef> + 'a {
        self.refs.iter().filter(move |x| x.same_id(r))
    }

    /// The defined ids of a kind (for an effect kind, on one bus), in source order.
    pub fn ids(&self, kind: IdKind, scope: Option<&str>) -> Vec<&str> {
        self.refs.iter().filter(|r| r.def && r.kind == kind && r.scope.as_deref() == scope).map(|r| r.id.as_str()).collect()
    }
}

struct Builder<'a> {
    src: &'a str,
    refs: Vec<IdRef>,
}

impl Builder<'_> {
    fn key(&mut self, kind: IdKind, key: &Key, scope: Option<&str>, def: bool) {
        let (span, form) = match key.quote {
            None => (key.span, Form::BareKey),
            Some(q) => match self.inner(key.span, &key.name) {
                Some(inner) => (inner, Form::Quoted(q)),
                None => return,
            },
        };
        self.refs.push(IdRef { kind, id: key.name.clone(), scope: scope.map(str::to_string), span, form, def });
    }

    /// The span of a string token's content, when it's written without escapes (so offsets map 1:1).
    fn inner(&self, token: Span, decoded: &str) -> Option<Span> {
        let inner = Span::new(token.start + 1, token.end.saturating_sub(1));
        (token.len() >= 2 && inner.text(self.src) == decoded).then_some(inner)
    }

    /// A string value that is an id.
    fn str_ref(&mut self, kind: IdKind, n: &Node) {
        let Some(id) = n.as_str() else { return };
        let Some(inner) = self.inner(n.span, id) else { return };
        let q = self.src[n.span.start..].chars().next().unwrap_or('"');
        self.refs.push(IdRef { kind, id: id.to_string(), scope: None, span: inner, form: Form::Quoted(q), def: false });
    }

    /// The ids inside a param path (`instruments.<id>.params.p`, `buses.<id>.effects.<fx>.params.p`,
    /// `matrix.<route>.depth_oct`) written in a string token (`token`) whose decoded text is `path`.
    fn path_refs(&mut self, token: Span, path: &str) {
        let Some(inner) = self.inner(token, path) else { return };
        let q = self.src[token.start..].chars().next().unwrap_or('"');
        let mut segs = Vec::new();
        let mut at = inner.start;
        for s in path.split('.') {
            segs.push((s, Span::new(at, at + s.len())));
            at += s.len() + 1;
        }
        let mut push = |kind, i: usize, scope: Option<&str>| {
            if let Some(&(id, span)) = segs.get(i) {
                if !id.is_empty() {
                    self.refs.push(IdRef { kind, id: id.to_string(), scope: scope.map(str::to_string), span, form: Form::Quoted(q), def: false });
                }
            }
        };
        match segs.first().map(|s| s.0) {
            Some("instruments") => push(IdKind::Instrument, 1, None),
            Some("matrix") => push(IdKind::Route, 1, None),
            Some("buses") => {
                push(IdKind::Bus, 1, None);
                if segs.get(2).map(|s| s.0) == Some("effects") {
                    let bus = segs.get(1).map(|s| s.0.to_string());
                    push(IdKind::Effect, 3, bus.as_deref());
                }
            }
            _ => {}
        }
    }

    /// A route entry's `source` (a modulator) and `dest` (a param path) — in `matrix` or a song layer.
    fn route_refs(&mut self, entry: &Node) {
        if let Some(src) = entry.get("source") {
            self.str_ref(IdKind::Modulator, src);
        }
        if let Some(dest) = entry.get("dest") {
            if let Some(d) = dest.as_str() {
                self.path_refs(dest.span, d);
            }
        }
    }

    fn walk(&mut self, root: &Node) {
        if let Some(insts) = root.get("instruments") {
            for m in insts.members() {
                self.key(IdKind::Instrument, &m.key, None, true);
                for s in m.value.as_ref().and_then(|v| v.get("sends_db")).map(Node::members).unwrap_or_default() {
                    self.key(IdKind::Bus, &s.key, None, false);
                }
            }
        }
        if let Some(mods) = root.get("modulators") {
            for m in mods.members() {
                self.key(IdKind::Modulator, &m.key, None, true);
                if let Some(phrase) = m.value.as_ref().and_then(|v| v.get("phrase")) {
                    for it in phrase.get("pool").map(Node::items).unwrap_or_default() {
                        if !it.node.as_str().is_some_and(|s| s.starts_with("lib:")) {
                            self.str_ref(IdKind::Pool, &it.node);
                        }
                    }
                    if let Some(a) = phrase.get("active").filter(|a| !a.as_str().is_some_and(|s| s.starts_with("lib:"))) {
                        self.str_ref(IdKind::Pool, a);
                    }
                }
            }
        }
        if let Some(buses) = root.get("buses") {
            for m in buses.members() {
                self.key(IdKind::Bus, &m.key, None, true);
                for fx in m.value.as_ref().and_then(|v| v.get("effects")).map(Node::members).unwrap_or_default() {
                    self.key(IdKind::Effect, &fx.key, Some(&m.key.name), true);
                }
            }
        }
        for m in root.get("sequence_pools").map(Node::members).unwrap_or_default() {
            self.key(IdKind::Pool, &m.key, None, true);
        }
        for m in root.get("matrix").map(Node::members).unwrap_or_default() {
            self.key(IdKind::Route, &m.key, None, true);
            if let Some(entry) = &m.value {
                self.route_refs(entry);
            }
        }
        for field in ["safe_bounds", "constraints"] {
            for m in root.get(field).map(Node::members).unwrap_or_default() {
                if m.key.quote.is_some() {
                    self.path_refs(m.key.span, &m.key.name);
                }
            }
        }
        if let Some(bind) = root.get("movement").and_then(|m| m.get("bind")) {
            for slot in ["chord", "bass", "arp"] {
                if let Some(v) = bind.get(slot) {
                    self.str_ref(IdKind::Modulator, v);
                }
            }
            for it in bind.get("oneshot").map(Node::items).unwrap_or_default() {
                self.str_ref(IdKind::Modulator, &it.node);
            }
        }
        if let Some(song) = root.get("song") {
            for layer in song.get("matrices").map(Node::members).unwrap_or_default() {
                self.key(IdKind::Layer, &layer.key, None, true);
                for route in layer.value.as_ref().map(Node::members).unwrap_or_default() {
                    self.key(IdKind::Route, &route.key, None, false);
                    // A full entry (or a patch of `source` / `dest`) names ids like a base route.
                    if let Some(entry) = &route.value {
                        self.route_refs(entry);
                    }
                }
            }
            for block in song.get("sequence").map(Node::items).unwrap_or_default() {
                if let Some(l) = block.node.get("matrix") {
                    self.str_ref(IdKind::Layer, l);
                }
                // A block's drift overrides are keyed by param path, like `constraints` (ADR 0055).
                for m in block.node.get("control").and_then(|c| c.get("drift")).map(Node::members).unwrap_or_default() {
                    if m.key.quote.is_some() {
                        self.path_refs(m.key.span, &m.key.name);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_definitions_and_every_kind_of_reference() {
        let src = r#"{
  instruments: { pad: { sends_db: { rv: -7.96 } } },
  modulators: { lfo: {}, seq: { phrase: { pool: ["walk", "lib:drift"], active: "walk" } } },
  buses: { rv: { effects: { verb: {} } } },
  sequence_pools: { walk: {} },
  matrix: {
    m1: { source: "lfo", dest: "instruments.pad.params.cutoff_hz" },
    m2: { source: "lfo", dest: "buses.rv.effects.verb.params.room" },
  },
  safe_bounds: { "instruments.pad.params.level_db": {}, "matrix.m1.depth_oct": {} },
  movement: { bind: { chord: "seq", oneshot: ["lfo"] } },
  song: {
    matrices: { deep: { m1: null, m3: { source: "lfo", dest: "instruments.pad.params.level_db", kind: "mod", depth_db: -3 } } },
    sequence: [{ matrix: "deep", loops: 4 }],
  },
}"#;
        let cst = Cst::parse(src);
        assert!(cst.errors.is_empty());
        let idx = Index::build(src, &cst);
        let count = |kind, id: &str, def| idx.refs.iter().filter(|r| r.kind == kind && r.id == id && r.def == def).count();
        assert_eq!(count(IdKind::Instrument, "pad", true), 1);
        assert_eq!(count(IdKind::Instrument, "pad", false), 3); // a dest + a bound path + a layer route's dest
        assert_eq!(count(IdKind::Modulator, "lfo", false), 4); // two sources + a oneshot bind + a layer route's source
        assert_eq!(count(IdKind::Modulator, "seq", false), 1);
        assert_eq!(count(IdKind::Bus, "rv", false), 2); // a send key + a dest
        assert_eq!(count(IdKind::Effect, "verb", false), 1);
        assert_eq!(count(IdKind::Pool, "walk", false), 2); // pool entry + active (lib: skipped)
        assert_eq!(count(IdKind::Route, "m1", false), 2); // a bound path + a song layer key
        assert_eq!(count(IdKind::Layer, "deep", false), 1);
        // Spans are the id text itself.
        for r in &idx.refs {
            assert_eq!(r.span.text(src), r.id, "{r:?}");
        }
        let use_site = idx.at(src.find("pad.params.cutoff_hz").unwrap() + 1).unwrap();
        assert_eq!(idx.def_of(use_site).unwrap().span.text(src), "pad");
        assert_eq!(idx.ids(IdKind::Effect, Some("rv")), ["verb"]);
    }
}
