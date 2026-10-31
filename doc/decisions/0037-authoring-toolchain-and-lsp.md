# ADR 0037 — The authoring toolchain: a lossless JSON5 CST, theory for the editor, and `sme-lsp`

Status: accepted (built 2026-10-10)
Relates to: ADR 0031 (the composition re-scope), ADR 0033 (strict checking, `sme-check`, the
polymeter), ADR 0034 (pieces), ADR 0036 (the `sme-live` protocol the CodeLens commands map to).
Consumed by ADR 0038 (the VS Code extension, P7) and ADR 0039 (the parts library, P8).

## Context

Composing happens in the loop doc, in VS Code. The strict checker (ADR 0033) knows every rule but
answers with JSONPaths on a command line; the editor needs the same knowledge **at the cursor**:
red squiggles on the exact token, completion that knows the schema, the doc's ids and its **scale**,
hovers that read numbers back in musical and physical units, and edits that never destroy the
comments the docs are full of. The engine's `json5` crate parses straight to values — no positions,
no comments, no partial trees — so the editor side needs its own syntax tree.

## Decision

**D1 — A lossless JSON5 CST in `sme_authoring::cst`.** Hand-rolled (no suitable crate does JSON5
with spans and recovery):

- the lexer covers every byte (trivia included — tokens concatenate back to the source), reusing
  `json5::char`'s Unicode classes so whitespace / identifiers agree with the engine's parser;
- the parser **recovers** — a key without `:` is a member with no value, a missing value or comma is
  reported and parsing continues, unclosed brackets close at EOF, and `{ cutoff: ⏎ resonance: 0.1 }`
  doesn't swallow the next key as the value — so completion works mid-keystroke;
- `Cst::to_value` is **tested identical to `json5::from_str::<Value>`** on every tracked doc and on a
  80-odd-case grammar table (numbers incl. hex / `±Infinity` / `NaN` / `-0` / u64/i64 limits, every
  escape, identifier keys incl. `\u` escapes and Unicode, comments, trailing commas, duplicates), and
  agrees on accept/reject;
- positions are byte offsets everywhere; `lines::LineIndex` converts to LSP positions (line +
  **UTF-16** column, `\n` / `\r\n` / `\r`) only at the protocol edge;
- `path` maps the checker's JSONPaths to nodes (keys aren't quoted in those paths, so a key may hold
  dots — `safe_bounds.global.master_gain` — resolved by the **longest** key that fits) and says what
  the cursor is on (key / value / object slot / array slot / nothing-in-a-comment);
- `edit` makes **comment-preserving** edits (replace a value, rename a key keeping its quoting,
  set / insert a member in the object's own layout — after a trailing comment, at the members'
  indentation, with a trailing comma — remove a member with its line). The parts library (P8) builds
  on these.

**D2 — Theory for the editor, in `sme_core::theory`** (portable, also used by telemetry):
`pitch::Key` spells notes by the doc's root **and** mode — one letter per scale degree, out-of-scale
notes as chromatic degrees against major (D dorian: D E F G A B C, its raised 3rd F#, its ♭6 Bb;
`C#` vs `Db` roots keep their own spelling); `note_name(midi, key)` with the octave of the letter
(Cb4, B#3). `Mode::name` / `degree_labels` (dorian `1 2 ♭3 4 5 6 ♭7`) / `interval_label`.
`chord::identify(pcs, bass)` names a pitch-class set (exact, fifth-less and tension readings, slash
chords, the bass deciding symmetric/ambiguous sets); `Chord::label` now uses it — it had logged
half-diminished sevenths as "dim7", major ninths as "C9" and diminished sevenths as "dim6".
`function::roman` reads numerals against the **parallel major** (the usual modal analysis — D
dorian: `i ii ♭III IV v vi° ♭VII`; borrowed chords show their accidental; secondary dominants as
`V7/x`). Display conventions: ASCII accidentals in note names and chord symbols (`Bb`, `F#m7b5` —
typeable, and what `harmony.root` accepts), Unicode in degrees and numerals (`♭3`, `♭VII`).

**D3 — Native units moved into the core.** The TUI's units registry is now
`sme_core::params::units` (+ `fmt_authored` for a value as written in a doc), shared by the TUI and
the LSP (deferred from ADR 0033).

**D4 — `sme-lsp` (`crates/sme_authoring`, bin `sme-lsp`)** on `lsp-server` 0.7 + `lsp-types` 0.97
(rust-analyzer's synchronous scaffold; no async runtime). Full-text sync; every change re-analyses
the doc (CST + strict check + polymeter: ≈ 2 ms for the 263-line song; each request well under 1 ms
in release). Every feature is a pure function over an `Analysis` answering in byte spans
(`lsp::{diagnostics, complete, hover, inlay, lens, actions, navigate}`); `lsp::server` only converts.

- **Diagnostics:** syntax errors (located, several per doc); every checker finding on the exact
  token its path names (an unknown key on the key, a bad value on the value, a missing node on its
  deepest existing key, doc-wide ones on the opening brace); note offsets outside `harmony.mode` as
  **hints** (borrowed colour is a choice, not an error).
- **Completion:** schema keys (required first, with type/range/default and the schema's
  description), params **filtered by synth type** (dual / modal / filtered-noise), send keys from the
  doc's buses, voice ids (the modulator's, then the next free `vN`), bound param paths; values —
  enums, booleans, `harmony.root` / `mode` (each mode shows its notes and degrees in the doc's root),
  modulator ids for `source` / binds (note modulators for note routes), every valid `dest` for the
  route's kind and the instrument's type, pool ids + `lib:` phrases, song layers; and **scale-aware
  offsets** — the scale's tones ±2 octaves, nearest the current value first, labelled
  `7 · A4 · 5 · D dorian`.
- **Hover:** schema docs + the Tier-2 drift range; params in native units (`cutoff 0.3 → 318Hz`);
  an offset's note, degree, Hz and the **step's chord** with its numeral; a `cents_offset`'s
  just-intonation reading (`the just 7/4 above A (harmonic 7), −31.2¢ from tempered`); the key's
  notes; the loop length + polymeter; a modulator's cycle; what an id is and how often it's used.
- **Inlay hints** (each kind switchable via `initializationOptions.inlayHints` /
  `workspace/didChangeConfiguration`): note names after offsets, `h7 7/4` after a tuned
  `cents_offset`, native units (Hz / s / ms) after params, `14.5 s loop` after `master_steps`, each
  routed modulator's cycle after its object (`⟳ 20.0 s · 5.00 loops · in phase every ≈ 5`), and when
  the doc realigns after `modulators`.
- **CodeLens:** `solo` / `mute` on each instrument, `▶ loop block K` / `play from here` on each song
  block. They are **client commands** (`sme.solo`, `sme.mute`, `sme.loopBlock`, `sme.playFrom`, args
  `[uri, id | block]`) that the extension forwards to `sme-live` (ADR 0036 `mute` / `solo` / `song`)
  — the language server never talks to the player.
- **Code actions:** the checker's did-you-means as one-click fixes (keys, values, a wrong segment
  inside a `dest` path), snap an out-of-scale offset to the scale tone below / above, transpose a
  step / phrase / whole modulator up or down a **scale degree** (an implicit `offset: 0` voice gets
  an explicit one), remove a bound that names nothing.
- **Navigation:** go to definition (an id's key; a `dest` / bound path's param), find references,
  rename an id everywhere — keys, `source`s, path segments inside `dest` and bound keys, send keys,
  binds, pool entries, song layers (refused for an id that exists, an empty name, or a dot) — and a
  document outline.

**D5 — `sme-check` reports line:column too.** Every diagnostic is located through the CST with the
same rule as the editor's squiggles (`  error   12:47 instruments.pad.params.cutof: unknown key …`;
`line` / `column` fields in `--json`, 1-based, columns in characters) — so the PostToolUse hook
points Claude at the line, not just the JSONPath.

## Consequences

- One rule set: the editor's squiggles are exactly what `sme-check`, the hook and `sme-live` refuse
  or warn about (`sme_core::check::check_value` is shared; the LSP parses with the CST, the engine
  with `json5`, and the parity tests keep the two parsers in agreement).
- Telemetry chord labels changed (`C#m7b5` instead of `C#dim7`); audio didn't (goldens unchanged).
- The CodeLens commands do nothing until the P7 extension implements them.
- Not done: incremental sync (full text is cheap at these sizes), semantic tokens (the P7 TextMate
  grammar colours JSON5), a formatter (docs are hand-laid-out; edits preserve layout instead).

## Verification

`cargo test -p sme_authoring`: CST round trip + `json5` parity (every tracked doc + the grammar
table), recovery, paths, cursor contexts, edits; a D dorian completion table; hover / inlay / lens /
action / navigation tables; a whole-repo robustness sweep (every feature at sampled offsets of every
tracked loop doc, and sampled prefixes of a doc while "typing"); an in-memory JSON-RPC session covering
every request kind and UTF-16 positions; and `sme-lsp` itself over stdio (Content-Length framing)
against the house piece.
