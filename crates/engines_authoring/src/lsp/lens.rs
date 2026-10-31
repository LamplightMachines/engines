//! CodeLens: play controls inline. Each lens names a **client command** — the editor extension
//! (P7) runs it by sending the matching `engines-live` protocol command (ADR 0036):
//!
//! | command         | arguments            | `engines-live` command (protocol 1)                  |
//! |-----------------|----------------------|--------------------------------------------------|
//! | `engines.solo`      | `[uri, instrument]`  | `solo` `instrument`, `on` toggled by the client   |
//! | `engines.mute`      | `[uri, instrument]`  | `mute` `instrument`, `on` toggled by the client   |
//! | `engines.loopBlock` | `[uri, block index]` | `song` `action: "loop_block"`, `block`            |
//! | `engines.playFrom`  | `[uri, block index]` | `song` `action: "play_from"`, `block`             |
//!
//! (The server adds the `uri`; here a lens carries only its own argument. The client knows the
//! current mute/solo state from `engines-live`'s `gains` events, so it flips `on`.)

use super::Analysis;
use crate::cst::{Node, Span};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Lens {
    pub span: Span,
    pub title: String,
    pub command: String,
    pub args: Vec<Value>,
}

pub fn lenses(a: &Analysis) -> Vec<Lens> {
    let mut out = Vec::new();
    let Some(root) = a.root() else { return out };
    for m in root.get("instruments").map(Node::members).unwrap_or_default() {
        let id = json!(m.key.name);
        out.push(Lens { span: m.key.span, title: "solo".into(), command: "engines.solo".into(), args: vec![id.clone()] });
        out.push(Lens { span: m.key.span, title: "mute".into(), command: "engines.mute".into(), args: vec![id] });
    }
    if let Some(song) = root.get("song") {
        for (i, block) in song.get("sequence").map(Node::items).unwrap_or_default().iter().enumerate() {
            let n = &block.node;
            let name = n.get("label").or(n.get("matrix")).and_then(Node::as_str).map(|l| format!(" · {l}")).unwrap_or_default();
            let loops = n.get("loops").and_then(Node::as_f64).map(|l| format!(" · {l} loops")).unwrap_or_default();
            let span = Span::new(n.span.start, n.span.start + 1);
            out.push(Lens { span, title: format!("▶ loop block {i}{name}{loops}"), command: "engines.loopBlock".into(), args: vec![json!(i)] });
            out.push(Lens { span, title: "play from here".into(), command: "engines.playFrom".into(), args: vec![json!(i)] });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruments_get_solo_mute_and_blocks_get_audition() {
        let src = r#"{ instruments: { pad: {}, sub: {} }, song: { sequence: [ { matrix: "settle", loops: 4 }, { loops: 2, label: "deep" } ] } }"#;
        let a = Analysis::new(src);
        let l = lenses(&a);
        let titles: Vec<&str> = l.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(titles, ["solo", "mute", "solo", "mute", "▶ loop block 0 · settle · 4 loops", "play from here", "▶ loop block 1 · deep · 2 loops", "play from here"]);
        assert_eq!(l[2].args, [json!("sub")]);
        assert_eq!((l[6].command.as_str(), &l[6].args), ("engines.loopBlock", &vec![json!(1)]));
        assert_eq!(l[0].span.text(src), "pad");
    }
}
