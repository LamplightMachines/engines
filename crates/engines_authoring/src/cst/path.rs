//! JSONPaths ↔ CST nodes, and what the cursor is on.
//!
//! Paths are the checker's (`engines_core::check::DocDiag::path`): keys joined by `.`, array indices as
//! `[n]` — `instruments.pad.params.cutoff_hz`, `modulators.arp.steps[2].voicing.v1`. Keys aren't quoted,
//! so a key may itself hold dots (`safe_bounds."global.master_gain_db"` is written
//! `safe_bounds.global.master_gain_db`); [`Cst::find`] resolves that by taking the **longest** key that
//! fits at each object.

use super::{Cst, Member, Node, NodeKind, Span, TokenKind};

/// One step of a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// `a.b[2].c` from segments.
pub fn render(segs: &[Seg]) -> String {
    let mut s = String::new();
    for seg in segs {
        match seg {
            Seg::Key(k) => {
                if !s.is_empty() {
                    s.push('.');
                }
                s.push_str(k);
            }
            Seg::Index(i) => s.push_str(&format!("[{i}]")),
        }
    }
    s
}

/// What a path resolved to — the deepest node it reached.
#[derive(Clone, Debug)]
pub struct Found<'a> {
    /// The value at the deepest matched step (`None` when that member has no value yet).
    pub node: Option<&'a Node>,
    /// The member holding it (`None` for the root or an array item).
    pub member: Option<&'a Member>,
    /// The matched steps.
    pub segs: Vec<Seg>,
    /// The whole path matched (otherwise this is its deepest existing prefix).
    pub complete: bool,
}

impl Found<'_> {
    /// Where to point at the found thing: the key for a member whose value is a container (or
    /// missing), else the value itself.
    pub fn focus(&self) -> Option<Span> {
        match (self.member, self.node) {
            (Some(m), Some(n)) if n.is_container() => Some(m.key.span),
            (_, Some(n)) if n.is_container() => Some(Span::new(n.span.start, n.span.start + 1)),
            (_, Some(n)) => Some(n.span),
            (Some(m), None) => Some(m.key.span),
            (None, None) => None,
        }
    }
}

/// The cursor's context.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    /// The path of the member or item the cursor is on (for `Key` / `Value`), or of the container whose
    /// gap it's in (for the slots).
    pub segs: Vec<Seg>,
    pub target: Target<'a>,
}

#[derive(Clone, Debug)]
pub enum Target<'a> {
    /// On a key (hovering one, or typing a new one).
    Key { object: &'a Node, member: &'a Member },
    /// On a value, or in the empty value position after `key:` (`node: None`).
    Value { member: Option<&'a Member>, node: Option<&'a Node> },
    /// In an object between members — where a new key would go.
    ObjectSlot { object: &'a Node },
    /// In an array between items — where a new item would go (`index` = items before the cursor).
    ArraySlot { array: &'a Node, index: usize },
    /// In a comment, or outside the doc.
    Nothing,
}

impl Cst {
    /// Resolve a checker path to the deepest node it reaches (`""` is the root).
    pub fn find(&self, path: &str) -> Found<'_> {
        let mut found = Found { node: self.root.as_ref(), member: None, segs: Vec::new(), complete: false };
        let mut rest = path;
        loop {
            if rest.is_empty() {
                found.complete = true;
                return found;
            }
            let Some(node) = found.node else { return found };
            match &node.kind {
                NodeKind::Object(members) => {
                    let fits = |k: &str| rest == k || rest.strip_prefix(k).is_some_and(|r| r.starts_with('.') || r.starts_with('['));
                    // Longest key wins (a dotted key over its prefix); on a repeated key, the last.
                    let Some(m) = members.iter().filter(|m| fits(&m.key.name)).max_by_key(|m| (m.key.name.len(), m.key.span.start)) else {
                        return found;
                    };
                    rest = &rest[m.key.name.len()..];
                    rest = rest.strip_prefix('.').unwrap_or(rest);
                    found.segs.push(Seg::Key(m.key.name.clone()));
                    found.member = Some(m);
                    found.node = m.value.as_ref();
                }
                NodeKind::Array(items) => {
                    let Some((idx, after)) = rest.strip_prefix('[').and_then(|r| r.split_once(']')) else { return found };
                    let Some(item) = idx.parse::<usize>().ok().and_then(|i| items.get(i)) else { return found };
                    found.segs.push(Seg::Index(idx.parse().expect("parsed above")));
                    found.member = None;
                    found.node = Some(&item.node);
                    rest = after.strip_prefix('.').unwrap_or(after);
                }
                _ => return found,
            }
        }
    }

    /// Resolve a path given as segments — exact keys (the **last** of a repeated key, as `json5`
    /// keeps) and array indices — to the deepest node it reaches. Unlike [`Cst::find`], nothing is
    /// split on `.`, so a key may hold any characters.
    pub fn find_segs(&self, segs: &[Seg]) -> Found<'_> {
        let mut found = Found { node: self.root.as_ref(), member: None, segs: Vec::new(), complete: false };
        for seg in segs {
            let Some(node) = found.node else { return found };
            match seg {
                Seg::Key(k) => {
                    let Some(m) = node.member(k) else { return found };
                    found.member = Some(m);
                    found.node = m.value.as_ref();
                }
                Seg::Index(i) => {
                    let Some(item) = node.items().get(*i) else { return found };
                    found.member = None;
                    found.node = Some(&item.node);
                }
            }
            found.segs.push(seg.clone());
        }
        found.complete = true;
        found
    }

    /// The cursor context at byte `offset` of `src` (the text this CST was parsed from).
    pub fn locate<'a>(&'a self, src: &str, offset: usize) -> Cursor<'a> {
        let nothing = Cursor { segs: Vec::new(), target: Target::Nothing };
        // Inside a comment (not at its very start): nothing to offer.
        if let Some((_, t)) = self.token_at(offset) {
            if matches!(t.kind, TokenKind::LineComment | TokenKind::BlockComment) && t.span.start < offset {
                return nothing;
            }
        }
        let Some(root) = &self.root else { return nothing };
        if !inside(src, root, offset) {
            return nothing;
        }
        let mut segs = Vec::new();
        let mut node = root;
        loop {
            match &node.kind {
                NodeKind::Object(members) => {
                    let mut next = None;
                    for (i, m) in members.iter().enumerate() {
                        if m.key.span.touches(offset) {
                            segs.push(Seg::Key(m.key.name.clone()));
                            return Cursor { segs, target: Target::Key { object: node, member: m } };
                        }
                        let Some(colon) = m.colon else { continue };
                        if offset < colon.end {
                            continue;
                        }
                        // The value position runs from the colon to the value's end — or, with no
                        // value, to the next member's key.
                        let limit = match &m.value {
                            Some(v) => v.span.end,
                            None => members.get(i + 1).map(|n| n.key.span.start).unwrap_or(node.span.end),
                        };
                        if offset <= limit && m.comma.is_none_or(|c| offset <= c.start) {
                            segs.push(Seg::Key(m.key.name.clone()));
                            match &m.value {
                                Some(v) if v.is_container() && inside(src, v, offset) => next = Some(v),
                                v => return Cursor { segs, target: Target::Value { member: Some(m), node: v.as_ref() } },
                            }
                            break;
                        }
                    }
                    match next {
                        Some(v) => node = v,
                        None => return Cursor { segs, target: Target::ObjectSlot { object: node } },
                    }
                }
                NodeKind::Array(items) => {
                    let mut next = None;
                    for (i, it) in items.iter().enumerate() {
                        if it.node.span.touches(offset) {
                            segs.push(Seg::Index(i));
                            if it.node.is_container() && inside(src, &it.node, offset) {
                                next = Some(&it.node);
                                break;
                            }
                            return Cursor { segs, target: Target::Value { member: None, node: Some(&it.node) } };
                        }
                    }
                    match next {
                        Some(v) => node = v,
                        None => {
                            let index = items.iter().filter(|it| it.node.span.end <= offset).count();
                            return Cursor { segs, target: Target::ArraySlot { array: node, index } };
                        }
                    }
                }
                _ => return Cursor { segs, target: Target::Value { member: None, node: Some(node) } },
            }
        }
    }
}

/// Strictly between a container's brackets (or anywhere after its opener when it's unclosed).
fn inside(src: &str, n: &Node, offset: usize) -> bool {
    let closed = n.span.len() >= 2 && matches!(src.as_bytes().get(n.span.end - 1), Some(b'}' | b']'));
    n.span.start < offset && (offset < n.span.end || (!closed && offset == n.span.end))
}
