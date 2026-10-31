# ADR 0023 — JSON5 input for loop/objectives docs + commented templates

- **Status:** accepted
- **Date:** 2026-06-29
- **Context:** the user wants both engine input files — the **loop document** and the
  **objectives/control document** — to accept inline "built-in" help: comment sections explaining the
  options for each part of the doc, plus examples. They also want the objectives file the TUI
  auto-creates to be that documented template, and the build to ship a commented template per file
  type. The loop-doc parser's doc comment already *anticipated* this ("Parse from plain JSON (strip
  JSON5 comments first)") but only ever called strict `serde_json::from_str`.
- **Related:** ADR 0010 (objectives_v0.1 / loop v0.8 schemas), ADR 0021 §6 (TUI structured editors),
  the TUI objectives auto-create + `0` hotkey (prior change). Code anchors:
  `crates/sme_core/src/{loop_doc.rs, contract.rs, ffi.rs, bin/sme-render.rs, bin/sme-play.rs}`,
  `crates/sme_tui/src/app.rs`, `contracts/objectives_v0.1.template.json5`,
  `doc/loop_contract_v0.8.json5`, `Makefile`.

## Decision

### 1. Parse both input docs as JSON5 (a superset of JSON)
Add the pure-Rust, serde-based `json5` crate to `sme_core`. JSON5 allows `//` and `/* */` comments and
trailing commas (and unquoted keys / single quotes), and is **string-aware**, so it never corrupts a
value that contains `//` (a URL, a path) the way a naive comment-stripper would. Because JSON5 is a
strict superset of JSON, every existing `.json` loop doc, fixture, and FFI payload keeps parsing
unchanged.

- `LoopDoc::from_json_str` → `json5::from_str` (the one choke point all bins/TUI/tests/FFI funnel
  through). Its error type changes `serde_json::Error → String` (every caller formats via
  `Display`/`Debug`, so the change is mechanical).
- New `SessionControl::from_json_str` (json5); the 4 scattered `serde_json::from_str::<SessionControl>`
  read sites (TUI `watch_objectives`, `sme-play` watch, FFI `sme_session_new` + `apply_control`) route
  through it.
- `from_value` (parsing an already-parsed `Value`) stays serde_json — no comments there.

Determinism (CE-R7.3) is unaffected: parsing produces the same struct regardless of the parser, and
never touches audio math. So unlike fundsp (pinned for bit-repro), `json5` floats (`= "1"`).

### 2. Objectives template = inline-commented JSON5, rendered with the live control
A committed `contracts/objectives_v0.1.template.json5` carries an inline `//` help comment + examples
*per field*, with `__TOKEN__` placeholders for the dynamic values.
`contract::objectives_template_json5(&SessionControl)` = `include_str!` + per-field `.replace()` of the
serialized current values. It is used by **every** objectives write — the TUI auto-create seed, the TUI
`apply_edit` write-back (so the help survives live edits), and `sme-play`'s `--objectives` seed — and
round-trips: `from_json_str(&render(&c))` re-serializes equal to `c`. The TUI's auto-created file is
now `sme-objectives.json5`.

### 3. Loop template = the existing annotated json5, audited + extended, now engine-parseable
`doc/loop_contract_v0.8.json5` becomes the canonical loop-doc template (the engine never auto-creates a
loop doc). It now parses directly (`sme-render doc/loop_contract_v0.8.json5 …`), guarded by a test. It
was audited against the schema and extended where the current feature set was undocumented: the stale
"strict serde_json (no comments)" header was corrected; **`orrery_role`** (chord/bass/arp/oneshot/
context) is documented and shown on the chord/bass/arp modulators; a real **`safe_bounds`** override
section was added; and `shift`/`shift_mix`/`resonance` got an inline example note. The comment-stripped
`lab/tests/fixtures/loop_contract_v0.8.json` stays as-is for the existing test suite.

### 4. Templates are embedded + emittable
Both templates are embedded via `include_str!` (`LOOP_TEMPLATE_JSON5`, `OBJECTIVES_TEMPLATE_JSON5`).
`sme-render --emit-template <loop|objectives> [out]` writes/prints one; `make templates` drops both
starter files into the cwd. `.gitignore` covers `sme-objectives.json5` + the `make templates` outputs.

## Consequences
- Loop docs and objectives docs can be heavily commented and still parse; the auto-created objectives
  file is self-documenting; users start a loop doc from a fully-annotated, engine-valid template.
- New tests (sme_core): JSON5 comments + trailing commas + unquoted keys parse for both doc types; the
  annotated loop template parses as a valid doc; the objectives template renders with comments and
  round-trips (default + populated). Suite green; determinism re-render byte-identical.
- A small maintenance contract: the objectives template's placeholder tokens must track the
  `SessionControl` fields — the round-trip test fails loudly if a field is added without a slot.
- Out of scope / deferred: converting the engine to render `doc/loop_contract_v0.8.json5` *instead of*
  the stripped `.json` fixture in tests (kept separate to avoid churning many tests); Python lab still
  reads plain JSON (its fixtures are plain JSON).
