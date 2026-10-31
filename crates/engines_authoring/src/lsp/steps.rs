//! Where each step of each step sequence is written — what the editor extension highlights while
//! `engines-live` plays (ADR 0038): its `tick` events name the current step of every loop-synced
//! sequence, and the extension asks for these spans with the custom `engines/stepRanges` request.

use super::Analysis;
use crate::cst::{Node, Span};
use std::collections::BTreeMap;

/// `modulator id → [(step, the step object's span)]`, in document order. A step's number is its
/// `step` field (entries may share one — sub-step `t`, ADR 0028); an entry without one counts by
/// its position.
pub fn step_spans(a: &Analysis) -> BTreeMap<String, Vec<(u32, Span)>> {
    let Some(root) = a.root() else { return BTreeMap::new() };
    root.get("modulators")
        .map(Node::members)
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let steps = m.value.as_ref()?.get("steps")?.items();
            let list: Vec<(u32, Span)> = steps
                .iter()
                .enumerate()
                .map(|(i, it)| (it.node.get("step").and_then(Node::as_f64).map_or(i as u32, |s| s.max(0.0) as u32), it.node.span))
                .collect();
            (!list.is_empty()).then(|| (m.key.name.clone(), list))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_map_to_their_objects() {
        let src = r#"{
  modulators: {
    seq: { type: "step_seq", length: 8, steps: [
      { step: 0, voicing: { v1: { offset: 0 } } },
      { step: 4, t: 0.5, voicing: { v1: { offset: 7 } } },
      { voicing: { v1: { offset: 3 } } },
    ] },
    lfo: { type: "lfo", rate_hz: 0.1 },
    half: { type: "step_seq", steps: [ { step: 1
"#;
        let a = Analysis::new(src);
        let spans = step_spans(&a);
        let seq: Vec<(u32, &str)> = spans["seq"].iter().map(|(n, s)| (*n, s.text(src))).collect();
        assert_eq!(seq[0], (0, "{ step: 0, voicing: { v1: { offset: 0 } } }"));
        assert_eq!(seq[1].0, 4);
        assert_eq!(seq[2], (2, "{ voicing: { v1: { offset: 3 } } }"), "no `step` ⇒ its position");
        assert!(!spans.contains_key("lfo"));
        assert_eq!(spans["half"][0].0, 1, "a half-typed step still maps");
    }
}
