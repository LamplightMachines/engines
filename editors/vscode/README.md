# Engines — the VS Code extension

Compose a piece by editing its loop doc and hearing every edit (ADR 0038). For files matching
`pieces/**/*.json5`, the extension provides:

- **the `engines-loop` language**: a JSON5 grammar, comments, brackets, and folding.
- **`engines-lsp`** (ADR 0037):
  - squiggles on the exact token, with hints for notes outside the scale;
  - completion that knows the schema, the doc's ids and its scale (`7 · A4 · 5 · D dorian`);
  - hovers and inline hints that read a value back: the note nearest a cutoff, a gain's amplitude, a
    depth's swing, a note's degree and chord;
  - a **Migrate to schema v0.9** quick fix on a v0.8 doc (ADR 0054);
  - quick fixes, go to definition, find references and rename.
- **`engines-live`** (ADR 0036). The player hears each save of the doc (⌘S), mid-loop, about 0.2 s
  later. Unsaved edits wait, and the status bar and panel mark them `● unsaved`. If a save has
  errors, the player refuses it and keeps the last good revision playing. To hear edits as you type
  instead, set `engines.live.applyOn` to `edit`.
  - **The live panel** (`Engines: Show the Live Panel`) has:
    - play, stop and restart, the **hold ⇄ drift** toggle, the Orrery, the limiter and a preview;
    - mute/solo with meters for every instrument;
    - the Orrery's chord on a keyboard, with scale tones, chord tones, pad voicing and bass;
    - song blocks: loop one, or play the song from it;
    - the revision history with diff and revert (a revision from before schema v0.9 goes in migrated);
    - an activity feed that shows what was applied, where, and what was refused.
  - **The status bar** shows the piece, its revision, hold or drift, the chord, and `● unsaved`
    while the editor holds edits the player hasn't heard. When nothing is playing it becomes a ▶
    button for the piece doc in front.
  - **CodeLens** puts `solo` / `mute` on each instrument and `▶ loop block K` / `play from here` on
    each song block. The titles show what's live (`● muted`, `◼ looping block 2`).
  - **Step highlighting** marks the step each step sequence is playing, where it's written (at most
    15 updates a second).
- **Parts** (ADR 0039). `Engines: Add a Part…` (also in the editor's context menu and under
  Source Action…) adds a whole idea in one step: a plucked pulse, a root drone, just partials, a halo
  pad, a breathing LFO, or a shimmer bus. The part comes with its instrument, modulators, routes and
  sends, its comments and a unique id. A part that acts on an instrument uses the one your cursor is
  in. F2 on the new id renames it everywhere.

## Install

From the repo root:

```bash
make vscode
```

This builds `engines-lsp` and `engines-live` (release), then `npm ci`, compiles, packages
`editors/vscode/engines.vsix` and installs it with `code --install-extension`. Reload VS Code
afterwards. Open the repo folder: the binaries are found in `target/release/` (or set
`engines.server.path` / `engines.live.path`).

## Use

1. Open `pieces/<slug>/<slug>.json5`.
2. Press ▶ in the editor title, or in the status bar.
3. Edit, then save (⌘S) to hear it. The squiggles, hovers and hints follow your typing; the player
   follows your saves. A save with errors is refused (the panel says why) and the last good
   revision keeps playing.

For hands-free saves, set `files.autoSave: afterDelay` (add `files.autoSaveWhenNoErrors` to skip
saves with errors). Or set `engines.live.applyOn: edit` to hear the buffer as you type, saved or not.

When Claude edits the file on disk, the player picks the edit up too (`external_change`). VS Code
reloads the buffer if it has no unsaved changes. Save or undo your own edits before asking Claude
for one, or VS Code will ask which version to keep when you next save.

## Settings

Coming from the extension's old name, SME Loop Docs? On first activation, Engines copies each
`sme.*` setting you set to its `engines.*` key at the same scope (user, workspace, folder). It points
`files.associations` entries for `sme-loop` at `engines-loop`, tells you once, and leaves the old keys
alone. If `sound-machine.sme-loop` is still installed, Engines offers to uninstall it and won't start
a player until it's gone, because both would claim piece docs.

| setting | default | |
|---|---|---|
| `engines.server.path` / `engines.live.path` | `""` | binaries; empty means `target/release/` in the workspace |
| `engines.live.args` | `[]` | extra `engines-live` flags (`--output-device`, `--ring-ms`, …) |
| `engines.live.headless` | `false` | no audio device; the panel still shows everything |
| `engines.live.applyOn` | `"save"` | when the player hears your edits: each save, or (`"edit"`) as you type |
| `engines.live.debounceMs` | `300` | with `applyOn: "edit"`, how long the buffer must be still before it's sent |
| `engines.live.highlightSteps` | `true` | step highlighting |
| `engines.inlayHints.{notes,units,loopLength,cycles}` | `true` | each kind of inline hint |

## Develop

Extensions built on this one (Engines Studio) add their features through the companion API:
see [API.md](API.md).

```bash
npm ci
npm test            # unit tests: state, keyboard, panel model, the process manager (+ the real engines-live), the grammar
npm run test:e2e    # in an isolated instance of the installed VS Code: LSP features, the headless player, panel, CodeLens, steps, revert
```

Running `code --extensionDevelopmentPath=$PWD` from this folder starts a development host with
the extension loaded.

The extension talks to `engines-live` with `contracts/engines_live_protocol_v1.md`. It talks to `engines-lsp`
with LSP, plus the custom requests `engines/stepRanges` (step highlighting) and `engines/pathRange` (the panel's jump-to-source, ADR 0040).
