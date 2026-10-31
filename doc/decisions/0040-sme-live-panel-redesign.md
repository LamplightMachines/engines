# ADR 0040 — The SME Live panel redesign (Ableton-style panel, jump-to-source, stereo meters, stem previews)

Status: accepted (built 2026-10-10, P10, branch `feat/sme-live-redesign`)
Amends: ADR 0038 D5 (what the panel shows) and D6 (protocol additions); ADR 0036 (protocol fields,
additive, still `protocol: 1`). Relates to: ADR 0029 (songs), ADR 0030 (stems), ADR 0021 (no master
limiter in renders), ADR 0037 (the CST, `sme-lsp`). Amended by ADR 0044 D10 (2026-10-21): the 12
identity colours (and Master's grey) are now Ableton Live palette colours, so a piece's Live tracks
match its strips; contrast on the name bars is ≥ 5:1 (WCAG AA) rather than the design's 7:1.

## Context

The user commissioned a redesign of the live panel through Claude Design. The handoff is checked in
at `design/vs_extension/`: `SPEC.md` (the interaction and visual spec), `tokens.css`, the mockup
source `reference/Main.dc.html` and the doc it was drawn against.

The design keeps the panel's job (show what `sme-live` plays, send clicks back as protocol commands)
and changes its look and reach:
- an Ableton Live 12 (Dark) look with its own palette, independent of the VS Code theme;
- collapsible sections with one-line summaries;
- named things (instruments, `harmony`, `global`, song blocks) are links that move the editor to
  that path in the doc;
- mixer strips with stereo meters, in document order, coloured from a cycling palette;
- a song structure bar that doubles as a progress indicator;
- a preview that can render stems.

Reviewing it against the code (2026-10-10) found places where it assumed things the player doesn't
do, or dropped things ADR 0038 requires. The user decided each one (below).

## Decision

**D1 — The song keeps the player's three modes.** The mockup assumed the song plays through by
default, with ⟲ meaning "repeat this block when it ends". The player's model stays:
- `loop` — the doc's top-level matrix (the default; live edits apply mid-loop);
- `block` — ⟲ loops one block's materialized doc (live edits re-materialize it);
- `song` — ▶ plays the song from block *k* (live edits are held, ADR 0029).

So ▶ = `song play_from k` (PLAY FROM tag on *k*, PLAYING on the current block), ⟲ = `song
loop_block k` (LOOPING), and **Back to loop** = `song stop`, disabled in loop mode. Making the song
the default would have turned live editing off most of the time.

**D2 — Nothing ADR 0038 D5 lists is dropped.** The redesign left out several items; they come back
restyled: the ● unsaved dot (on the revision chip), the refused-edit banner, the starting /
restarting / failed / ended states (status pill), limiter gain reduction (beside the Limiter
toggle), underruns (only when non-zero), "all revisions…", "clear solo", the idle "Play this piece"
state, and a fifth collapsible section, **Activity** (warnings, the activity feed, output and
preview links), collapsed by default.

**D3 — Real stereo meters.** Per-instrument and master meters gain L/R peaks (additive fields; the
existing max-of-both values stay). Metering is telemetry only: no sample math changes, the goldens
are unchanged.

**D4 — Preview: mix, or mix + stems, chosen each time.** The preview dialog asks for a length, then
"Mix only" or "Mix + stems" (ADR 0030 stems: per-instrument dry + per-bus wet, −18 LUFS). The stem
code moves from the `sme-song` binary into `sme_core::stems` so the player can use it; `sme-song`'s
output stays bit-identical.

**D5 — Jump-to-source goes through `sme-lsp`.** A new custom request `sme/pathRange` resolves a JSON
path to the key's range over the Rust CST. The extension never parses JSON5 itself.

**D6 — Defaults.**
- The webview stays plain HTML/CSS/JS (ADR 0038 D1) and a `WebviewPanel` beside the doc.
- Fonts: VS Code's own UI and editor fonts (the user's call, 2026-10-10 — the design's Archivo /
  JetBrains Mono aren't bundled, so the CSP needs no `font-src`, and the 94% stretch tuned for
  Archivo is dropped).
- High-contrast themes fall back to VS Code theme colours.
- Instruments are listed in **document order** (the player now reads key order from the CST;
  `LoopDoc.instruments` is a `BTreeMap`).
- Identity colours follow position: tracks `palette[i % 12]`, blocks `palette[(i + 6) % 12]`.
- The Limiter toggle is the live-only protection limiter (never in renders, ADR 0021); its tooltip
  says so.
- Revision history records where each revision came from: `editor`, `disk` or `revert`.

**D7 — Song progress, block mode included.** The player reports how far into its block each loop
is (`loop_in_block` of `block_loops`, and `tail`), from the session's song telemetry in song mode and
counted from the loop the block started in block mode (wrapping, since a looped block repeats). The
extension keeps the block a song plays **from** apart from the block it has **reached** — a `loop`
event used to overwrite the one with the other.

## Protocol additions (`protocol: 1`, additive)

| Message | New fields |
|---|---|
| `ready` | `master_steps`, `limiter` (from `LiveOptions` — a headless `--no-limiter` no longer reports `true`), `note` (`meta.note`), `instruments` **in document order**, `song.{from, current, tail_loops}` |
| `tick` | `meters_lr` (`{id: [L, R]}` dBFS), `master_lr`, `song` |
| `loop` | `song.{loop_in_block, block_loops, tail}` (block mode too) |
| `doc` (command) | `source` is recorded in the history (`editor` default; `revert`; the watcher's `disk`) |
| `preview` (command / event) | `stems` (bool) / `stems: {dir, target_lufs, files, skipped}`; stem previews capped at 300 s |

`sme-lsp` gains the custom request `sme/pathRange` (`{textDocument, path, label?}` → `{range,
keyRange, complete}`). `SongStamp` (the loop log, the FFI's records JSON) gains the same song-position
fields; `BlockTelemetry` carries L/R peaks and the song position. All telemetry: no sample math
changed, every golden hash is unchanged.

## Consequences

- The panel matches the design and still shows everything ADR 0038 D5 lists. The webview stays plain
  JS: a skeleton built once, lists rebuilt only when their shape changes (block rows are now keyed by
  label and length, not by state, so a click never lands on a replaced button), styles via the CSSOM.
- `sme-live` depends on `sme_authoring` (the CST only) for the instruments' document order.
- `sme_core::stems` (feature `stems`, enabled by `cli`) holds the stem export's shared half; `sme-song
  --stems` is byte-identical (pinned by `tests/sme_song_stems_golden.rs`, written before the move).
- The CodeLens titles are icon-led (`$(circle-large-…) solo`, `$(unmute)` / `$(mute) muted`, `$(sync)
  loop · …`, `$(play) play from …`) with tooltips; the status bar reads `$(play) piece r · mode · ♫
  block · chord` in a contributed "playing" colour (status items honour only error/warning
  backgrounds).
- Colours follow position (SPEC open question 1, decided 2026-10-10: fine for now): inserting or
  reordering an instrument or block shifts the colours after it. Stable-by-name colours (a hash of
  the id) or a per-doc override can come later without a protocol change.
- Not done: a webview test that clicks real buttons
  (the model is unit-tested, the rendering is checked in `editors/vscode/dev/`'s browser harness).

## Verification

`cargo test` (the goldens unblessed; `sme_core` session meters + song position, `sme_live` protocol:
ready/tick/loop fields, document order, history sources, stem previews; `sme_authoring` pathRange) ·
`make test` · `cd editors/vscode && npm test` (state fold, keyboard + aria, lens titles, flash
frames, panel model, song section, preview dialog, the token guard) · `make vscode-test` (e2e: icon
lens titles, a reveal by path and by label, revert's history source) · the harness scenarios at 500 /
320 px and in high contrast.
