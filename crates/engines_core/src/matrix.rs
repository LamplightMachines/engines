//! Matrix routing helpers (CE-R5.4/R5.5).
//!
//! `mod` summing/clamping/depth-slew is applied inline in the engine render. This module
//! owns the **priority-aware fan-in voice allocator** (CE-R5.5 — the key M2 gap):
//! when an instrument's sources demand more than its polyphony, steal
//! **lowest-priority-source-first, then oldest-within-source**; high-priority sources
//! are never stolen until lower ones are exhausted.

use crate::engine::ActiveVoice;
use std::collections::BTreeMap;

/// Per-source voices for one instrument. NOTE: the `Vec` is **not** ordered by age —
/// `apply_note_event` returns this event's (fresh) voices at the head and carried/sustained
/// voices after them — so age-aware decisions must read the per-voice clocks, never positions.
pub struct InstrumentVoices<'a> {
    /// source_id -> (priority, voices). Lower priority number = higher priority.
    pub sources: &'a mut BTreeMap<String, SourceVoices>,
}

#[derive(Clone, Debug, Default)]
pub struct SourceVoices {
    pub priority: i32,
    pub voices: Vec<ActiveVoice>,
}

/// Steal voices until the instrument's total ≤ `polyphony` (CE-R5.5).
/// Mutates the per-source voice lists in place.
pub fn enforce_polyphony(sources: &mut BTreeMap<String, SourceVoices>, polyphony: usize) {
    loop {
        let total: usize = sources.values().map(|s| s.voices.len()).sum();
        if total <= polyphony {
            break;
        }
        // Choose the source with the highest priority NUMBER (lowest priority) that
        // still has voices; tie-break by source_id for determinism.
        let victim = sources
            .iter()
            .filter(|(_, s)| !s.voices.is_empty())
            .max_by(|(id_a, a), (id_b, b)| {
                a.priority.cmp(&b.priority).then_with(|| id_a.cmp(id_b))
            })
            .map(|(id, _)| id.clone());

        match victim {
            Some(id) => {
                // Within that source, steal the least musically valuable voice: one already in
                // its release tail first, else the OLDEST by note-age (`elapsed_decay_s` — the
                // engine stamps articulation clocks *before* enforcement, so a fresh strike
                // reads newest). The list is NOT positionally age-ordered (`apply_note_event`
                // puts fresh articulations at the head), so the previous positional
                // `remove(0)` stole the newborn strike while long-dead-but-unreleased
                // sustained voices squatted the polyphony — an arp driving an
                // intrinsic-decay synth (modal) could fall permanently silent.
                let vs = &mut sources.get_mut(&id).unwrap().voices;
                let mut idx = 0usize;
                for i in 1..vs.len() {
                    let ka = (vs[i].released_at_s.is_some(), vs[i].elapsed_decay_s);
                    let kb = (vs[idx].released_at_s.is_some(), vs[idx].elapsed_decay_s);
                    if ka > kb {
                        idx = i; // strictly-greater keeps the first of equals (deterministic)
                    }
                }
                vs.remove(idx);
            }
            None => break,
        }
    }
}

/// Flatten all surviving voices for synthesis (stable order: by source_id, then age).
pub fn flatten(sources: &BTreeMap<String, SourceVoices>) -> Vec<ActiveVoice> {
    let mut out = Vec::new();
    for s in sources.values() {
        out.extend(s.voices.iter().cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voices(source: &str, ids: &[&str]) -> Vec<ActiveVoice> {
        ids.iter()
            .map(|id| ActiveVoice { voice_id: (*id).into(), source_id: source.into(), ..Default::default() })
            .collect()
    }

    #[test]
    fn steals_low_priority_source_first_oldest_within() {
        // poly polyphony 6: chords priority 1 (4 voices), arp priority 3 (4 voices) → 8, steal 2.
        let mut sources = BTreeMap::new();
        sources.insert(
            "chords".to_string(),
            SourceVoices { priority: 1, voices: voices("chords", &["c1", "c2", "c3", "c4"]) },
        );
        sources.insert(
            "arp".to_string(),
            SourceVoices { priority: 3, voices: voices("arp", &["a1", "a2", "a3", "a4"]) },
        );
        enforce_polyphony(&mut sources, 6);

        // chords (high priority) keeps all 4; arp loses its 2 oldest (a1,a2).
        assert_eq!(sources["chords"].voices.len(), 4);
        let arp_ids: Vec<&str> = sources["arp"].voices.iter().map(|v| v.voice_id.as_str()).collect();
        assert_eq!(arp_ids, vec!["a3", "a4"]);
    }

    #[test]
    fn high_priority_never_stolen_until_low_exhausted() {
        let mut sources = BTreeMap::new();
        sources.insert("pad".to_string(), SourceVoices { priority: 1, voices: voices("pad", &["p1", "p2"]) });
        sources.insert("arp".to_string(), SourceVoices { priority: 3, voices: voices("arp", &["a1"]) });
        enforce_polyphony(&mut sources, 2); // total 3 → drop 1; must be arp's
        assert_eq!(sources["pad"].voices.len(), 2);
        assert_eq!(sources["arp"].voices.len(), 0);
    }

    #[test]
    fn steal_prefers_released_then_oldest_never_the_newborn() {
        // Regression (the "muted bell"): `apply_note_event` returns the FRESH articulation at the
        // list head, so a positional remove(0) stole the newborn strike and kept long-dead
        // sustained voices squatting the polyphony. The steal must pick by state/age instead:
        // released-first, then oldest by note-age (`elapsed_decay_s`), never list position.
        let mk = |id: &str, elapsed: f32, released: Option<f32>| ActiveVoice {
            voice_id: id.into(),
            source_id: "arp".into(),
            elapsed_decay_s: elapsed,
            released_at_s: released,
            ..Default::default()
        };
        // Head = a strike born this event (clock stamped -(frame/sr) pre-enforcement);
        // tail = two carried voices sustained from prior loops (large positive ages).
        let mut sources = BTreeMap::new();
        sources.insert(
            "arp".to_string(),
            SourceVoices {
                priority: 1,
                voices: vec![mk("newborn", -0.5, None), mk("old_a", 2.14, None), mk("old_b", 10.71, None)],
            },
        );
        enforce_polyphony(&mut sources, 2);
        let ids: Vec<&str> = sources["arp"].voices.iter().map(|v| v.voice_id.as_str()).collect();
        assert_eq!(ids, vec!["newborn", "old_a"], "the oldest squatter is stolen, never the newborn");

        // A voice already in its release tail is the least valuable — stolen before an even
        // older held voice.
        let mut sources = BTreeMap::new();
        sources.insert(
            "arp".to_string(),
            SourceVoices {
                priority: 1,
                voices: vec![mk("held_old", 20.0, None), mk("releasing_young", 1.0, Some(0.8))],
            },
        );
        enforce_polyphony(&mut sources, 1);
        let ids: Vec<&str> = sources["arp"].voices.iter().map(|v| v.voice_id.as_str()).collect();
        assert_eq!(ids, vec!["held_old"], "released voices are stolen before held ones");
    }
}
