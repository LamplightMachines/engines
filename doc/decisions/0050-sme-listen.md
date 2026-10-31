# ADR 0050 — sme-listen: inspiration snippets into loop-doc parts, an offline analysis tool over renders

> **Published trimmed.** This record also decides parts of tools that stay closed (a companion's inspiration tool, `engines-listen`).
> Those sections are left out here; the decisions below are the ones the open code implements, word for word.
> It was written before the rename to Engines: `sme` is `engines` throughout (`sme-check` is `engines-check`, `sme-part` is `engines-part`; the catalog's contract is now `contracts/capability_catalog_v2.schema.json`, in native units (ADR 0054)).

Status: accepted — 2026-10-24.
Relates to:
- **ADR 0039: parts.** Patches join a doc as parts, through `engines-part add --file`.
- **ADR 0005 and ADR 0009:** native units at the doc boundary, and the synth types. The capability catalog publishes
  both.
- **CE-R7.3: determinism.** Nothing here changes engine output, and no golden hash moves.

## Context

An authoring tool (or an agent) that maps a sound onto a loop doc needs to know exactly what the engine can do —
every synth type's params, their ranges, units and modulatability — without reading the code, and needs a way to
merge a patch it writes into a doc the way library parts merge. The record's main subject, a tool that measures an
inspiration snippet, stays closed; these are the engine helpers it asked for.

## Decision (the open part)

**D6 — Engine helpers.**
- **A capability catalog:** `sme-check --catalog --json`, with its contract in
  `contracts/capability_catalog_v1.schema.json`. It holds the synth types, each param's applicability, range, kind,
  default and modulatability, the effects, and the unit maps (each with sample points).
  - It is generated from the engine's own registries (`PARAMS`, `check::param_applies`, `MODULATABLE_INST_PARAMS`,
    `REVERB_PARAM_LEAVES`, and the map functions), with parity tests, so neither the tool's unit conversions nor the
    mapping guide can drift from the engine.
- **`sme-part add --file`:** Claude's patches go through the same placeholders, check and atomic write as library
  parts.
- **Deferred:** `sme-render --seconds`. The engine renders whole loops, and nothing needs a render budget without the
  polish.

Left out: D1–D5 and D7–D10 (the inspiration tool itself: its analysis, models, mapping skill, benchmark and
scope).
