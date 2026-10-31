# The Engines companion API

**Version 1.0.0** (`API_VERSION` in `src/companion.ts`)

An extension built on Engines, such as Engines Studio, adds its features through this API. Its code
never ships in the public extension. The API lives in `src/companion.ts`, and its tests are in
`test/companion.test.ts`.

## Connecting

```ts
const engines = vscode.extensions.getExtension('LamplightMachines.engines');
const api = (await engines.activate()).connect('Engines Studio', 1); // the major version you're built for
```

`connect(name, major)` returns the API, or throws an error saying which extension to update when
`major` isn't this version's major. Anything else on `activate()`'s result (`live`, `lsp`, …) belongs
to the extension's own tests and isn't part of the API.

## What a companion can do

| Call | What it does |
|---|---|
| `setBinary(tool, path)` | Play on your build of `engines-live` (or serve `engines-lsp` from yours). A running player restarts onto it. `undefined` goes back to the open one. |
| `registerStripAction({ id, icon, state, run, onDidChange? })` | A button on each mixer strip. `icon` is SVG path data in a 24×24 box, drawn stroked in the button's colour. The panel never parses it as markup. `state(strip)` returns `null` (no button) or `{ on, blocked?, title }`. `run(strip)` runs on a click. `onDidChange(listener)` makes the panel redraw. |
| `registerOutputStep({ id, pairsDescription?, beforeSwitch? })` | A step of the output picker, after a multichannel device is picked. `pairsDescription(ctx)` rewrites what the routing-pairs choice says. `beforeSwitch(ctx)` runs before the player switches (`ctx.route`: each source on its own pair). The routing-pairs and mix choices themselves stay open. |
| `registerMenuItem({ command, title, where })` | Place a command you registered with VS Code in the panel's footer (`'panel'`), or as a link in the status bar item's tooltip (`'statusBar'`). Only registered items run. |
| `piece()` | The piece playing: `{ slug, dir, doc, root }`, or `undefined`. |
| `player()` | `{ state, ready, status }`: the player's state as this extension folds it, plus the raw last `ready` event and `.live/status.json`, protocol fields it doesn't know included (`ready.capabilities`, for one). |
| `onDidChange(listener)` | Fires when the piece or the player's state changes. |

Every `register*` call returns a disposable. Disposing it removes what it added.

## Versions

- **The major version changes** for anything that can break a companion built for the old one: a
  call removed or renamed, a parameter or a return value that changes shape or meaning, or a strip
  action's state read differently.
- **The minor version changes** for additions only: a new call, a new optional field, a new `where`.
  A companion built for 1.0 works with any 1.x.
- Each Engines release checks Engines Studio against this API.
