# ADR 0033 — Control settings live in the loop doc; strict checking at the authoring boundary

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** the rename D1 put off came with ADR 0054. `constraints`, guardrail ceilings/floors and overrides are in native units under the v0.9 keys (a `cutoff: 0.5` edge is `cutoff_hz: 800`, `level: 0.4` is `level_db: -7.96`); `max_change` and `max_drift` stay fractions. The embedded schema is `contracts/loop_contract_v0.9.schema.json`, and D4's lenient `schema_version` is gone: another version is refused, naming `sme-migrate`.

Status: accepted (built 2026-10-09)
Relates to: ADR 0011 (two-axis control + the objectives doc), ADR 0023 (JSON5), ADR 0029 (songs),
ADR 0031 (re-scope — a piece is a self-contained project), ADR 0032 (reflecting drift, `min == max`
freezes).

## Context

The composition workflow (ADR 0031) keeps each piece in `pieces/<slug>/` as **one** loop doc that the
author, Claude and the live player all edit. Two things stood in the way:

1. **How a piece moves lived elsewhere.** The timbre axis (`optimize`), the Orrery config (`movement`)
   and Tier-3 bands (`constraints`) were only settable through CLI flags or a separate objectives doc
   (`--objectives`), so a piece's sound depended on how it was launched (the published `idle-night-v2`
   render didn't even record its flags).
2. **Typos were silent.** The engine's serde structs drop unknown keys and default bad values: `cutof`,
   `wave: "sawtooth"` (plays as a sine), `mode: "dorain"` (falls back to ionian), a matrix route to a
   misspelled instrument — all rendered without a word. An editor-centric workflow (LSP, `sme-live`,
   Claude's edits) needs every problem reported, located, at once.

## Decision

**D1 — Additive at schema v0.8, not a v0.9 rename.** The new top-level fields join v0.8 the way songs,
phrases and stems did. Nothing gates on `schema_version` (it parses best-effort and lints), so a bump
would only invalidate every tracked doc for no behavioural gain.

**D2 — The doc carries its control settings.** Top-level `optimize` (`hold` | `drift` | `live_edit`),
`movement` (the objectives doc's `movement`, same shape: the Orrery on/off, bind, phrase length,
tension, Arranger tuning) and `constraints` (JSONPath → `{min, max}`, native units). Precedence:

- an `--objectives <file>` **replaces** the control (it stays the tool for one-off experiments);
- otherwise CLI flags **layer over** the doc (`SessionControl::with_flags`: `--optimize` replaces the
  axis, `--orrery` switches the Orrery on keeping the doc's config, ceilings/floors add guardrails);
- otherwise the doc's own settings (`SessionControl::from_doc`); a doc with none of the three keeps the
  session defaults, so every existing doc renders bit-identically (pinned: a doc carrying a control ≡
  the same control passed explicitly, bit for bit).

The controller strips the three fields from its operating point (they are control, not sound). A live
edit that changes `movement`/`constraints` folds into the running control at the next loop with the
revision bumped and the Orrery's state kept (`with_doc_settings`); `optimize` stays the player's (a
live-edit session doesn't flip itself into drift because the doc says so) — except in `sme-live`,
whose mode *is* the doc's: there a saved `optimize` change switches hold ⇄ drift at the next loop
(ADR 0034 D5, 2026-10-09).

**D3 — New pieces start from a tracked root defaults file, copied once.** `sme-defaults.json5` holds
only control keys (`optimize`, `movement`, `constraints`, `max_change`, `max_drift`); /piece-new copies
them into the new doc, after which the piece owns them. No live layering: editing the defaults never
changes an existing piece (renders stay reproducible from the doc alone).

**D4 — Strict checking at the authoring boundary** (`sme_core::check`). `check_str` runs four passes and
reports **every** problem as a `DocDiag { severity, path (JSONPath), code, message }`:

1. JSON5 syntax;
2. a **schema walk** of the raw value against the embedded `contracts/loop_contract_v0.8.schema.json`
   (`schema.rs`): unknown keys with did-you-mean, enums, types, ranges, conditional shapes. The schema
   is the single source of truth for keys/enums/ranges (also for the P6 LSP's completion); keys
   starting `x_` are free annotations in any record (never drift candidates, so they can't change a
   render) — but **not** as ids in an id-keyed map (`instruments.x_pad` is an instrument the engine
   plays, so it is checked like any other; amended in review). A key repeated in its
   object is an error (`duplicate`): JSON5 keeps only the last;
3. `LoopDoc::validate_all` — the engine's structural rules, now collecting all errors with paths;
4. semantic checks the schema can't express: strict `harmony.root`/`mode`, dangling matrix
   sources/dests/sends (song-layer routes included), ids containing `.`, an unknown LFO wave or effect
   type, inverted bands — a one-sided `safe_bounds` side beyond the Tier-2 default's other side wins
   and is explained with the effective band — and (warnings) params or
   routes a synth type ignores, silent instruments (polyphony 0) and buses (no `return_gain`), bands
   naming no param, an explicitly bound `movement.bind` id that isn't a modulator.

Errors refuse the doc — `LoopDoc::from_json_str` is now strict, so every bin, the session and the FFI
reject a typo'd doc. Warnings are advice. The engine's own message wins when both it and the schema
flag the same node; a stale `schema_version` stays a warning (the engine's lint). Pinned: every tracked
doc, house reference and the template check clean — **and so does what the engine re-serializes** from
them (history snapshots and the controller's operating point must re-load).

**D5 — Structure analysis** (`sme_core::analysis::polymeter`): per routed modulator, its cycle in loops
and when it is back at its start on a loop top. Loop-synced sequences realign every loop; a free
sequence of `length · step_duration = a/b` loops realigns after `a`; LFOs (always free-running, float
rates) realign approximately — the first loop count with phase error ≤ 0.02 cycle, at least one whole
cycle in (a very slow LFO that has barely moved isn't "back") — and LFOs above
1 Hz are texture, not form. The doc realigns at the lcm of the exact parts extended to the first
multiple where every structural LFO is back in phase (horizon 100 000 loops). This is the "structure"
lever made visible (the idle drone: ≈ 275 loops ≈ 1:06:40).

**D6 — `crates/sme_authoring` + `sme-check`.** A desktop crate on the lean core (no dev-bin features)
that will also hold the JSON5 CST and `sme-lsp` (P6). `sme-check <doc>... [--json] [--parts]`: a
verdict, one line per diagnostic and the polymeter summary; `--json` prints one object per doc per line
for tools (the P3 PostToolUse hook, the editor). Exit 0 = every doc loads (warnings allowed), 1 = some
doc has errors, 2 = unreadable file. `make check [DOC=…]` checks every tracked doc by default.

## Consequences

- Strictness found real latent bugs: test docs authored `wave: "saw"` (no such wave — those voices had
  always played as sines; they now say `"sine"`, preserving what the tests exercised), the schema
  required `polyphony ≥ 1` while the engine serializes `0` for an instrument without a `note_engine`
  (now `minimum: 0`, warned as silent), `idle-nap` carries a `safe_bounds` band on a matrix depth that
  nothing reads, and the sportscar fixture routes `global.reverb_mix` (only seeds the default bus).
- Hand-authored docs that relied on silent fallbacks now fail to load until fixed — the point.
- The schema file is now load-bearing at runtime (embedded with `include_str!`); keep it in step with
  the serde structs (the round-trip test catches drift between them).
- Not done here: bus-chain reconciliation with decaying tails moves to P4 (it needs block rendering);
  native-unit tables move into the core with the LSP (P6). The plan's separate "field table" is the
  embedded schema plus `check::param_applies`.
