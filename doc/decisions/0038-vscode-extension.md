# ADR 0038 — The VS Code extension: `sme-loop` language, `sme-lsp` client, and the live player

Status: accepted (built 2026-10-10)
Amended by: ADR 0040 (the panel redesign: protocol additions, the panel, stem previews).
Relates to: ADR 0034 (pieces, revisions, `sme-live` v0), ADR 0035 (mid-loop apply), ADR 0036 (the
`sme-live` protocol), ADR 0037 (`sme-lsp`, the CST). Consumed by ADR 0039 (parts library, P8) and the
P9 skills.

## Context

The workflow (ADR 0031) puts composing in VS Code: open a piece's doc, type, and hear it. The pieces
already exist:
- `sme-lsp` knows the schema, the scale and the ids.
- `sme-live --stdio` applies a doc mid-loop about 0.2 s after receiving it, and reports what it plays.

The editor still needs a few things:
- a language id for piece docs, so the built-in JSON support stays out of the way;
- a client for the language server;
- something that runs the player, feeds it the buffer and shows what it's doing;
- the hold ⇄ drift toggle the user asked for (2026-10-09) for fine-tuning.

## Decision

**D1 — `editors/vscode/`, TypeScript, compiled with `tsc`.**
- The only runtime dependency is `vscode-languageclient` 10. There's no bundler, no framework, and
  the webview is plain HTML/CSS/JS.
- The editor-independent parts (process manager, state fold, keyboard and panel models, history,
  debouncing) are separate modules with no `vscode` import, so `node:test` covers them without an
  editor.
- `make vscode` builds `sme-lsp` and `sme-live` (release), runs `npm ci`, packages
  `sme-loop.vsix` (`vsce`) and installs it. `make vscode-test` runs both test suites.

**D2 — The `sme-loop` language for `**/pieces/**/*.json5`.**
- A JSON5 TextMate grammar: identifier and quoted keys as property names; hex, `±Infinity`, `NaN`
  and leading- or trailing-dot numbers; single-quoted strings, escapes and line continuations; both
  comment styles.
- A language configuration: comment toggling, brackets, auto-closing, indentation.
- Defaults for the language: ~~`files.autoSave: afterDelay` plus `files.autoSaveWhenNoErrors`~~
  (dropped by the amendment below: the player hears saves now, so autosave would decide when an
  edit plays). Quick suggestions are on inside strings, where ids, enums and `dest` paths are
  completed.
- No semantic tokens. The grammar colours JSON5, and the language server's diagnostics and inlays
  carry the meaning.

**D3 — The language client.**
- Starts `sme-lsp` over stdio (`target/release/sme-lsp`, or `sme.server.path`).
- The `sme.inlayHints.*` settings go in `initializationOptions` and follow changes via
  `workspace/didChangeConfiguration`. The server accepts camelCase (`loopLength`) and now sends
  `workspace/inlayHint/refresh`, so a toggled hint kind shows at once.
- A CodeLens middleware rewrites lens titles from the player's state (`● soloed`, `● muted`,
  `◼ looping block 2`) and refreshes them when that state changes.
- A missing binary offers to build it in a terminal task.

**D4 — One live player at a time** (`LiveController` over `LiveProcess`).
- **Start.** Spawns `sme-live <doc> --stdio` (plus `--headless` and `sme.live.args`). The player
  starts from the file on disk, so an edited buffer is saved first. If `.live/status.json` shows
  another live player for the piece, the user is asked before a second one starts.
- **Edits.** ~~Every change to the piece doc is sent as a `doc` command after 300 ms of quiet
  (`sme.live.debounceMs`), carrying the document version. Saving isn't needed.~~ Each save of the
  piece doc is sent as a `doc` command, carrying the document version (amendment below;
  `sme.live.applyOn: "edit"` restores the debounced buffer).
- **Refresh.** After `accepted`, the controller asks for `status`, and the fresh `ready` carries
  instruments, song blocks and the key.
- **Crashes.** A crash after `ready` restarts the player with backoff (0.5 → 10 s, at most 5 in a
  row; a 30 s run resets the count). The buffer is re-sent, and mutes, solos and the hold/drift
  choice are restored.
- **Start failures.** A death before `ready` isn't retried blindly: a doc with errors on disk, no
  audio device, a missing binary. The extension says why, and tries again on the next save of the
  doc.
- **Closing.** Quitting sends `quit`, then EOF, then a kill after 2 s. Deactivating the extension
  quits the player, so `status.json` says `stopped`.

**D5 — What it shows.** One pure fold (`live/state.ts`) of the protocol events feeds everything.
- **The live panel** (a webview beside the doc) has:
  - play, stop, restart and close;
  - the **hold ⇄ drift** segmented toggle (the `control` command; hold plays exactly what's
    written);
  - Orrery and limiter switches, and a preview render;
  - loop position, master meter, limiter gain reduction, last loop peak and underruns;
  - the **Orrery chord on a keyboard**: scale tones tinted, chord tones marked, pad voicing and bass
    as dots, spelled in the doc's key;
  - per-instrument mute/solo with post-gain meters;
  - song blocks (`⟲ loop` / `▶ play from`, current block highlighted);
  - the 10 newest revisions, with diff against the doc and revert. Revert is one undoable edit, and
    "all revisions…" opens the full picker;
  - warnings, and an activity feed of applied (loop and position), refused, external changes and
    previews.

  The panel posts at most 15 times a second. Lists are rebuilt only when their shape changes, so a
  click never lands on a replaced button.
- **The status bar** shows `piece r12 · hold · ⟲ deep · Fmaj7`, a warning tint while an edit is
  refused, and an error tint on failure. With nothing playing, it becomes a ▶ button for the piece
  doc in front.
- **Step highlighting.** `tick.steps` names the current step of each loop-synced sequence. A new
  custom request, `sme/stepRanges`, returns where each step object is written. Ranges are cached
  per document version, and repaints happen only when a step changes, at most 15 Hz.
- **CodeLens commands.** `sme.solo` / `sme.mute` toggle against the live state; `sme.loopBlock` /
  `sme.playFrom` audition a block (again: back to the loop). Each starts the player first if
  needed.

**D6 — Protocol additions (additive, still `protocol: 1`).**
- `ready.key` gives `name`, `root` (pitch class), `scale` (semitones above the root), `degrees` and
  `names` (all 12 pitch classes spelled in the key).
- `loop.voicing` gives the Orrery's `pad` offsets, `bass` and chord `tones`; it's `null` with the
  Orrery off.
- These come from new `OrreryTelemetry` fields `pad` / `bass` / `tones`. That's telemetry only:
  the goldens are unchanged.

## Consequences

- The plan's end-to-end loop works inside the editor: type → hear the edit about 0.2 s after the
  pause. A refusal shows in the panel and status bar while the last good revision keeps playing.
- Claude's edits reach the player two ways: on disk through `sme-live`'s watcher
  (`external_change`), and through the editor buffer once VS Code reloads it. Revisions are
  content-addressed, so the second copy is `unchanged`.
- The JS dependencies are the language client, plus `tsc`, `vsce`, `@vscode/test-electron`,
  `vscode-textmate` and `vscode-oniguruma` for development. `npm audit` is clean (vsce 4).
  Installing needs network for npm.
- Not done:
  - semantic tokens;
  - a webview test that clicks real buttons (the panel's model is tested, and its rendering was
    checked in a browser harness);
  - several players at once;
  - MIDI-out in `sme-live` (unchanged, ADR 0036).

## Verification

- **`npm test`** (16 tests):
  - the state fold, keyboard, panel model and status-bar view;
  - piece paths, history and debounce/throttle;
  - the grammar, tokenized with VS Code's own engine (literals, keys after multi-line strings, and
    every tracked loop doc with no stray or invalid scopes);
  - `LiveProcess` against a fake child (framing, crash → backoff → give up, failure before `ready`,
    missing binary) and against the real `sme-live --stdio --headless` (ready → edit applied →
    refused → clean quit).
- **`npm run test:e2e`** (15 steps, in an isolated instance of the installed VS Code):
  - language id, diagnostics on the token, scale-aware completion, hover, inlays and CodeLens;
  - the headless player starts and the panel opens;
  - an unsaved edit is applied; a typo is refused and the good revision plays;
  - `sme.mute` from a CodeLens, and the lens title follows;
  - hold ⇄ drift; the playing step is highlighted at its object; song block audition;
  - revert is heard as a new revision; stop;
  - a doc with errors on disk fails to start, then starts once it's fixed and saved.
- **Rust:** `sme/stepRanges` and the inlay refresh (`sme_authoring`); `ready.key`
  (`sme_live` protocol test); `OrreryTelemetry` voicing (`sme_core` controller test).
- **Packaging:** the `.vsix` was packaged and installed into a throwaway extensions dir.

## Amendment (2026-10-10) — the player hears saves, not keystrokes

Found in the user's first real session: hearing every pause in typing is fast, but an edit that
takes several keystrokes (a new value, a re-voiced step, a part being trimmed) is heard half-made.
The user wants to decide when an edit plays.

- **D7 — `sme.live.applyOn`: `"save"` (the default) or `"edit"`.**
  - With `save`, the piece doc is sent as a `doc` command when it's saved (⌘S), and only then.
    The player starts from the saved file, and a dirty buffer isn't saved for it; after a crash the
    restarted player reads the saved file, which is all it should hear.
  - With `edit`, the D4 behaviour: the buffer after `sme.live.debounceMs` of quiet, saved or not.
  - Revert and **Add a Part…** put their text in the editor as before; it plays when saved.
- **D8 — Unsaved edits are marked.** `LiveState.unsaved` (a host-side fact, not an event): the
  status bar adds `● unsaved` (its tooltip says to save) and the panel shows an `● unsaved` badge
  beside the revision.
- **D9 — No autosave default.** The `[sme-loop]` defaults no longer turn on `files.autoSave`. A user
  who sets `afterDelay` gets the old hands-free feel, with valid-only saves if they also set
  `files.autoSaveWhenNoErrors`.
- **D10 — A save is heard once.** `sme-live` used to hear an editor save twice: the `doc` command,
  then its own file watcher reading the same text. An accepted text was harmless (content-addressed,
  `unchanged`), but a refused one was refused twice. The watcher now counts a text that arrived as a
  `doc` as delivered (`DocWatcher::seen_text`). Claude's edits on disk are still heard through the
  watcher (`external_change`).
- **D11 — Claude sees the unsaved buffer** (P9). Without autosave, an unsaved buffer is common,
  and an edit Claude writes to the file would collide with it (VS Code asks which to keep at the
  next save). The extension sends a new protocol command, `editor { unsaved }` (no event), whenever
  the buffer's dirty state flips, and on each player (re)start, whatever `applyOn` is. `sme-live`
  writes it straight to `status.json` (`editor: { unsaved, at }`). The piece skills ask the user to
  save or undo before they write.
- **Verified:**
  - `npm test` covers the status bar and panel marks.
  - The e2e suite (17 steps) checks:
    - an unsaved edit isn't sent, even well past the debounce, and the save plays it;
    - a refused save is refused once;
    - `applyOn: "edit"` still plays the unsaved buffer;
    - revert and an added part play once saved.
  - `sme_live`'s protocol test checks that a refused `doc` saved to the file isn't refused again,
    while a different save on disk is heard, and that `editor` reaches `status.json`. The e2e
    suite checks that the status says unsaved while the buffer is dirty, and clears after the save.
