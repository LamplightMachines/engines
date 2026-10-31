//! Sequence pools (ADR 0028): the built-in phrase library + shared validation/resolution.
//!
//! A phrase ([`PhraseDef`]) is pitch + rhythm + articulation tiled over its host modulator's
//! step grid. Docs reference built-in entries as `lib:<name>`; the library is an embedded
//! JSON5 asset parsed with the same types, so an entry is pinned by copy-pasting it into the
//! doc's `sequence_pools` verbatim. Library content is engine-versioned like the DSP — a
//! render referencing `lib:` entries may change across engine versions; pin by copying.

use crate::loop_doc::{LoopDoc, PhraseDef};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The built-in phrase library (embedded at compile time).
pub fn library() -> &'static BTreeMap<String, PhraseDef> {
    static LIB: OnceLock<BTreeMap<String, PhraseDef>> = OnceLock::new();
    LIB.get_or_init(|| {
        let src = include_str!("../assets/phrase_library.json5");
        let lib: BTreeMap<String, PhraseDef> =
            json5::from_str(src).expect("assets/phrase_library.json5 parses");
        for (id, def) in &lib {
            if let Err(e) = validate_phrase(id, def) {
                panic!("built-in phrase library entry invalid: {e}");
            }
        }
        lib
    })
}

/// Resolve a pool id: doc-local `sequence_pools` first; `lib:<name>` hits the built-in library.
pub fn resolve<'a>(id: &str, doc: &'a LoopDoc) -> Option<&'a PhraseDef> {
    if let Some(name) = id.strip_prefix("lib:") {
        return library().get(name);
    }
    doc.sequence_pools.get(id)
}

/// Structural validation shared by doc `validate()` and the library's own parse.
pub fn validate_phrase(id: &str, def: &PhraseDef) -> Result<(), String> {
    if !matches!(def.fit.as_str(), "chord" | "arp" | "root" | "literal") {
        return Err(format!("phrase '{id}': unknown fit \"{}\" (chord|arp|root|literal)", def.fit));
    }
    if def.length == 0 {
        return Err(format!("phrase '{id}': length must be ≥ 1 (host steps)"));
    }
    if def.notes.is_empty() {
        return Err(format!("phrase '{id}': no notes"));
    }
    for (k, n) in def.notes.iter().enumerate() {
        if !(0.0..def.length as f32).contains(&n.t) {
            return Err(format!("phrase '{id}' note {k}: t {} out of 0..length ({})", n.t, def.length));
        }
        if let Some(d) = n.dur {
            // A gate must END inside its own tile: `p{k}` ids are shared by legato chains
            // (the head re-strikes at ITS t next tile) and by every other pool entry (a
            // re-selected tile may strike the same id at t = 0), so a note-off that spilled
            // into the next tile could release a voice that was just struck (ADR 0028 D4).
            if d <= 0.0 {
                return Err(format!("phrase '{id}' note {k}: dur {d} must be > 0"));
            }
            if n.t + d > def.length as f32 {
                return Err(format!(
                    "phrase '{id}' note {k}: t {} + dur {d} = {} exceeds the tile (length {}) — a gate must end within its own tile",
                    n.t, n.t + d, def.length
                ));
            }
        }
        if !(0.0..=1.0).contains(&n.prob) {
            return Err(format!("phrase '{id}' note {k}: prob {} out of 0..=1", n.prob));
        }
        if let Some(v) = n.velocity {
            if !(1..=127).contains(&v) {
                return Err(format!("phrase '{id}' note {k}: velocity {v} out of 1..=127"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_library_parses_and_every_entry_validates() {
        // `library()` panics on a bad asset; touching it here keeps the asset honest and
        // pins the starter set the ADR names.
        let lib = library();
        for name in ["glass_up", "and_lift", "triplet_ripple", "pedal_pulse", "sigh", "alberti_low", "long_hold", "syncopa"] {
            assert!(lib.contains_key(name), "library ships '{name}'");
        }
        for (id, def) in lib {
            assert!(validate_phrase(id, def).is_ok());
            assert!(!def.tags.is_empty(), "library entry '{id}' is tagged for selection");
        }
    }
}
