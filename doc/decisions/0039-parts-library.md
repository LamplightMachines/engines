# ADR 0039 — The parts library: multi-section fragments merged into a loop doc

> **Units note ([ADR 0054](0054-native-units-schema-v0-9.md), schema v0.9):** the parts are v0.9. `pulse`'s `level: 0.2` and its freeze band are `level_db: -13.98`, and its send is `sends_db: { $bus: -12.04 }`. `breath`'s `depth: 0.1` into the cutoff is `depth_oct: 0.664`, and its `part.target` is `cutoff_hz`. A `patch` writes `sends_db`. A v0.8 part file goes through `sme-migrate` before `sme-part add --file`.

Status: accepted (built 2026-10-10)
Relates to: ADR 0037 (the CST and its comment-preserving edits, `sme-lsp`), ADR 0038 (the VS Code
extension), ADR 0032 (freezing a lever with `min == max`), ADR 0021 (no master limiter: docs gain-stage
themselves), ADR 0028 (sequence pools). Consumed by P9 (the skills).

## Context

Most musical ideas in a loop doc span several sections at once. A plucked pulse, for example, is
an instrument, the step sequence that plays it, a phrase cell, the note route between them, a
send to the reverb, and (in the house style) a frozen level. Writing one by hand means touching
five places with matching ids, which is error-prone for a person and for Claude alike. A typo in
any one of them is a refused doc, or a voice that never sounds.

The plan asked for a library of such fragments, with three consumers:
- an "Add part…" action in the editor;
- an extension command;
- `sme-part add` for Claude's skills.

It also set the test: every part merged into every house piece must parse, check clean, render
without clipping, keep ids unique and keep comments intact.

## Decision

**D1 — A part is a JSON5 file of doc sections plus placeholders** (`crates/sme_authoring/assets/parts/<name>.json5`,
format in its README).
- **Header.** `part: { summary, target? }`.
- **Sections:** `instruments`, `buses`, `sequence_pools`, `modulators`, `matrix` and `safe_bounds`,
  whose members are appended to the doc's sections, plus `patch: { "<path>": value }` for values set
  in existing objects.
- **Placeholders,** resolved against the doc before merging:
  - `$id` is the part's id, made unique (`pulse`, `pulse_2`…).
  - `$bus` is the doc's send bus: `--bus`, else the first bus that isn't an `external_return` (it
    takes no sends; naming one with `--bus` is refused). A `$bus` send is dropped when no bus
    qualifies.
  - `$target` is an existing instrument: the one under the cursor, `--target`, or the first that has
    `part.target` (`cutoff`, or `any`).
  - `"$deg(N, oct)"` is the semitones of scale degree N in the doc's own mode.
- **Musical defaults.** Parts are key-agnostic: `$deg` pitches, `fit: "chord"` cells, and harmonics
  limited to fifths and octaves. Levels are quiet, because a part joins a doc that is already
  gain-staged.
- **Location.** The plan put the files in `sme_core/assets/`. They live in `sme_authoring`
  instead, because only the authoring tools read them and the portable core shouldn't carry them.

**D2 — The six parts** (the plan's pulse, drone partials, halo pad, LFO and shimmer bus, plus the
floor drone the house pieces all have):

| part | adds |
|---|---|
| `pulse` | a 3-mode modal pluck, a loop-locked `step_seq` phrase arm (`oneshot` role), a `fit: "chord"` cell (root, ghost, fifth, octave), its note route, a send |
| `drone` | a low saw on the root two octaves down (`context` role, `orrery_disable`), its route, a small send |
| `partials` | three triangle voices on harmonics 3, 4 and 6 of that root, `cents_offset`-tuned just (`context` role), wet |
| `halo` | a slow detuned pad on degrees 5↓·1·3·5 of the mode (`chord` role: the Orrery re-voices it; a second chord track layers), very wet |
| `breath` | a ~22 s free sine LFO on `$target`'s cutoff |
| `shimmer` | a shimmer-reverb bus, a ~43 s swell of its shimmer, and a send into it from `$target` |

**D3 — The merge** (`sme_authoring::parts::add`) only ever adds to the doc's text.
- **Appending.** Each section's members go after the doc's last member, at the members'
  indentation. The part's own comments come with them (a blank line before a commented block), and
  a comma is added if the last member lacked one.
- **Creating a section.** A missing section is created after the nearest section that precedes it
  in the usual order, so `sequence_pools` lands after `modulators`. An empty `{}` is opened up, and
  a one-line object gets a one-line member.
- **Patches** use the CST's `set_member`, creating `sends: {…}` if needed.
- **`safe_bounds`** is merged only when the doc already has a `safe_bounds` section. A doc that
  freezes its levels gets the part's level frozen too; one that lets levels drift lets the part's
  drift. A band the doc already has for a path is kept as authored, never duplicated (added in
  review).
- **The result is checked like the engine loads it.** A part that would add errors isn't added;
  new warnings are reported. A doc with a syntax error is refused before anything happens.
  *Amended 2026-10-10 (Copilot review):* errors the doc already had are returned separately
  (`Added::existing_errors`). `sme-part`, which writes the file, refuses then, so it never leaves a
  doc the player refuses; fix those first. The editor's source actions still add the part to a
  buffer with an unrelated mistake: nothing is written, and the squiggles show what to fix before
  saving.

**D4 — Three ways in**, one implementation:
- **`sme-part list | show <part> | add <doc|slug> <part> [--id] [--bus] [--target] [--dry-run] [--json]`**
  writes the doc in one atomic save, so a playing `sme-live` hears one revision. Exit codes: 0 added,
  1 refused (including a doc that already has errors), 2 not found. `make part PIECE=<slug> PART=<name>` wraps it.
- **`sme-lsp` source actions** "Add part: …" (kind `source.addPart`). They're computed only when a
  client asks for source actions, because each one is a trial merge and check, too costly for every
  cursor move. The edit is a minimal line diff (`parts::diff_edits`), so undo and the cursor behave.
- **The extension's `SME: Add a Part…`** (editor context menu too) asks for those actions at the
  cursor, shows them in a picker, applies the chosen one as one undoable edit and reveals it. The
  player hears it like any edit. Renaming the new id everywhere is F2 (ADR 0037).

## Consequences

- One step adds a whole idea to a piece, with matching ids, routes and sends, a gain-staged level,
  the house style's frozen level where the doc uses one, and comments that explain it. Claude's
  skills can call `sme-part add` instead of editing five sections by hand (P9 updates them).
- The library is engine-versioned like the phrase library: a part is copied in, so later changes
  to the library never touch an existing doc.
- Not done:
  - parameters beyond id, bus and target (a part's level or register is edited after adding);
  - user part libraries (a `pieces/` or home-folder search path);
  - removing a part as a unit.

## Verification

`cargo test -p sme_authoring`:
- **Every part into every reference doc:** the two house pieces, the annotated template and two
  strict-JSON fixtures. Each result parses (CST and `json5`), checks with no errors, keeps every
  comment in order, has unique keys in every object and contains every added path. The editor's
  diff reproduces it, and adding the same part again gives a fresh id that's just as sound.
- **Rendering:** each house piece with each part in it, raw and from the controller's operating
  point, peaks at 0.55–0.69 FS (no clip).
- **Layout:** the members' indentation and comments, a trailing comment kept, sections created in
  place, empty and one-line objects handled, `safe_bounds` following the doc.
- **Resolution:** `$deg` in D dorian and lydian, a doc with no bus, chosen ids, targets and buses,
  and every refusal.
- **Editor and CLI:** source actions follow the cursor and appear only when asked for (engine and
  JSON-RPC); `sme-part` list, dry run, add by slug, `--json` and exit codes.

The extension's end-to-end suite adds a part through the source action and hears it applied by the
headless player.
