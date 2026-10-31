//! Shared test fixtures (`#[cfg(test)]` only). Mirrors `engines_core`'s internal `test_support`
//! (cfg(test) items can't cross the crate boundary, so this tiny twin is the cheaper trade).

#![allow(dead_code)]

use engines_core::loop_doc::LoopDoc;

/// The canonical v0.8 contract fixture's raw JSON (shared with the lab suite).
pub(crate) fn fixture_json() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../lab/tests/fixtures/loop_contract_v0.9.json"
    ))
    .unwrap()
}

/// The canonical fixture, parsed.
pub(crate) fn fixture_doc() -> LoopDoc {
    LoopDoc::from_json_str(&fixture_json()).unwrap()
}
