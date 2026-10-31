// Engines (ADR 0038): the `engines-loop` language (JSON5 grammar + `engines-lsp`) for piece docs, and
// the live player — `engines-live --stdio` hears each save of the doc mid-loop (or, with
// `engines.live.applyOn: "edit"`, the buffer as you type); a panel, the status bar, CodeLens and step
// highlighting show and steer what it plays.

import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';
import { repoRoot, swapBinary } from './binaries';
import { API_VERSION, Companions, type EnginesApi } from './companion';
import { associationWrites, carriedOverMessage, doneId, oldKey, settingWrites, OLD_EXTENSION_ID, type Associations, type Seen, type Write } from './carryOver';
import { readHistory, revisionPath, ago, schemaVersion, SCHEMA_VERSION } from './history';
import { LiveController, samePath } from './live/controller';
import type { LiveState } from './live/state';
import { Lsp } from './lsp';
import { LivePanel } from './panel/panel';
import { Jumper } from './jump';
import { currentAction } from './parts';
import { listPieces, pieceOfDoc, type Piece } from './piece';
import { parseSeconds, previewKinds, previewLengths, previewMessage } from './previewView';
import { StatusBar } from './statusBar';
import { StepHighlighter } from './steps';

/** What `activate` returns — the e2e tests drive the extension through it. */
export interface Api {
  /** The companion API's version (API.md). */
  version: string;
  /** The companion API for an extension built on this one: `connect(name, major)`, refused (an error
   *  saying which to update) for another major version. */
  connect(name: string, major: number): EnginesApi;
  // The rest is this extension's own tests' (the e2e suite), not the companion API.
  companions: Companions;
  live: LiveController;
  lsp: Lsp;
  steps: StepHighlighter;
  jumper: Jumper;
  panel(): LivePanel | undefined;
}

let player: LiveController | undefined;

/** The kind of engines-lsp's **Add part: …** source actions. */
const ADD_PART = vscode.CodeActionKind.Source.append('addPart');

export async function activate(ctx: vscode.ExtensionContext): Promise<Api> {
  const output = vscode.window.createOutputChannel('Engines', { log: true });
  await carryOver(ctx, output).catch((e: Error) => output.warn(`carrying the SME settings over failed: ${e.message}`));
  const root = repoRoot();
  const live = new LiveController(output);
  player = live;
  // The old extension, still installed, would start a player of its own.
  const oldInstalled = () => !!vscode.extensions.getExtension(OLD_EXTENSION_ID);
  live.refusal = () => (oldInstalled() ? `The old SME Loop Docs extension (${OLD_EXTENSION_ID}) is still installed: uninstall it first, or two players would start.` : undefined);
  if (oldInstalled()) {
    void offerUninstallOld();
  }
  const lsp = new Lsp(root, output, () => live.state);
  const steps = new StepHighlighter(live, lsp);
  const jumper = new Jumper(lsp);
  LivePanel.memento = ctx.globalState;
  // The companion API: what Engines Studio adds goes through it.
  const companions = new Companions({
    piece: () => live.piece && { slug: live.piece.slug, dir: live.piece.dir, doc: live.piece.doc, root: live.piece.root },
    player: () => ({ state: live.state, ready: live.lastReady, status: readStatus(live.piece?.dir) }),
    binaryChanged: (tool) => {
      swapBinary(tool, companions.binary(tool));
      if (tool === 'engines-lsp') {
        void lsp.restart();
      } else if (live.state.phase !== 'off') {
        live.restart();
      }
    },
  });
  LivePanel.companions = companions;
  live.companions = companions;
  ctx.subscriptions.push(output, live, lsp, steps, jumper, new StatusBar(live, companions));

  // Live state the CodeLens titles show (● soloed / ● muted / ◼ looping block).
  let lensKey = '';
  ctx.subscriptions.push(
    live.onDidChange((s) => {
      const key = JSON.stringify([s.phase, s.muted, s.soloed, s.song?.mode, s.song?.block, s.song?.from]);
      if (key !== lensKey) {
        lensKey = key;
        lsp.refreshLenses();
      }
    }),
    live.onEvent((ev) => {
      if (ev.type === 'preview') {
        const actions = ev.stems ? ['Open mix', 'Reveal stems'] : ['Open mix'];
        void vscode.window.showInformationMessage(previewMessage(ev), ...actions).then((pick) => {
          if (pick === 'Open mix') {
            void vscode.env.openExternal(vscode.Uri.file(ev.path));
          } else if (pick === 'Reveal stems' && ev.stems) {
            void vscode.commands.executeCommand('revealFileInOS', vscode.Uri.file(ev.stems.dir));
          }
        });
      }
    }),
  );

  /** The piece to act on: an explicit doc, the editor in front, a visible piece doc, the playing one, or a pick. */
  async function choosePiece(arg?: unknown): Promise<Piece | undefined> {
    const fromUri = (u: unknown) => {
      if (u instanceof vscode.Uri) {
        return u.scheme === 'file' ? pieceOfDoc(u.fsPath) : undefined;
      }
      if (typeof u === 'string') {
        try {
          return pieceOfDoc(vscode.Uri.parse(u).fsPath);
        } catch {
          return undefined;
        }
      }
      return undefined;
    };
    const editors = [vscode.window.activeTextEditor, ...vscode.window.visibleTextEditors].filter((e): e is vscode.TextEditor => !!e);
    const found =
      fromUri(arg) ??
      editors.map((e) => fromUri(e.document.uri)).find((p) => p) ??
      live.piece;
    if (found) {
      return found;
    }
    const all = root ? listPieces(root) : [];
    if (!all.length) {
      void vscode.window.showInformationMessage('No pieces found (pieces/<slug>/<slug>.json5) — /engines-new makes one.');
      return undefined;
    }
    const pick = await vscode.window.showQuickPick(
      all.map((p) => ({ label: p.slug, description: path.relative(root ?? '', p.doc), piece: p })),
      { placeHolder: 'Which piece?' },
    );
    return pick?.piece;
  }

  /** Make sure `piece` is playing (start it if needed); `false` if it couldn't start. */
  async function ensureLive(piece: Piece): Promise<boolean> {
    await live.settled(); // a lens click right after ⏏: the old player is still quitting
    if (live.piece && samePath(live.piece.doc, piece.doc) && live.state.phase === 'running') {
      return true;
    }
    await live.start(piece);
    return waitFor(live, (s) => s.phase === 'running' || s.phase === 'failed' || s.phase === 'off', 20_000).then((s) => s?.phase === 'running');
  }

  /** A CodeLens command: `[uri, id | block]` from engines-lsp. */
  function lensCommand(run: (arg: unknown) => void) {
    return async (uri?: unknown, arg?: unknown) => {
      const piece = await choosePiece(uri);
      if (piece && (await ensureLive(piece))) {
        run(arg);
      }
    };
  }

  const reg = (id: string, fn: (...args: any[]) => unknown) => ctx.subscriptions.push(vscode.commands.registerCommand(id, fn));
  reg('engines.live.start', async (arg?: unknown) => {
    const piece = await choosePiece(arg);
    if (piece) {
      await live.start(piece);
      LivePanel.show(ctx.extensionUri, live);
    }
  });
  reg('engines.live.stop', () => live.stop());
  reg('engines.live.restart', () => live.restart());
  reg('engines.live.chooseOutput', () => live.chooseOutput());
  reg('engines.live.showPanel', (opts?: { focus?: boolean }) => LivePanel.show(ctx.extensionUri, live, { focus: opts?.focus === true }));
  reg('engines.live.showOutput', () => output.show(true));
  // The panel's jump-to-source: a path in the playing piece's doc (a song block also by its label).
  reg('engines.live.reveal', async (path: (string | number)[], label?: string) => {
    const doc = live.piece?.doc ?? (await choosePiece())?.doc;
    return doc ? jumper.reveal(doc, path, label) : false;
  });
  reg('engines.live.toggleHold', () => live.toggleHold());
  // ADR 0040: how long, then the mix alone or the mix + stems (each with its size on disk).
  reg('engines.live.preview', async () => {
    const len = await vscode.window.showQuickPick(previewLengths(), { placeHolder: 'Render how long a preview? (offline, from the doc as written)' });
    if (!len) {
      return;
    }
    let seconds = len.seconds;
    if (seconds === undefined) {
      const v = await vscode.window.showInputBox({
        prompt: 'Seconds to render offline (up to 600; stems up to 300)',
        value: '45',
        validateInput: (t) => (typeof parseSeconds(t) === 'string' ? (parseSeconds(t) as string) : undefined),
      });
      const n = v === undefined ? undefined : parseSeconds(v);
      if (typeof n !== 'number') {
        return;
      }
      seconds = n;
    }
    const kind = await vscode.window.showQuickPick(previewKinds(seconds, live.state.instruments.length, live.state.sampleRate), { placeHolder: `${seconds} s — the mix alone, or with stems?`, matchOnDetail: true });
    if (kind) {
      live.preview(seconds, kind.stems);
    }
  });
  reg('engines.live.revealStems', () => {
    const dir = live.state.preview?.stemsDir;
    if (dir) {
      void vscode.commands.executeCommand('revealFileInOS', vscode.Uri.file(dir));
    }
  });
  // The panel passes the playing piece's doc: the revisions it lists are that piece's history, not
  // the history of whichever piece doc is in front.
  reg('engines.live.revert', async (rev?: number, doc?: unknown) => {
    const piece = await choosePiece(doc);
    if (!piece) {
      return;
    }
    if (typeof rev !== 'number') {
      const playing = !!live.piece && samePath(live.piece.doc, piece.doc);
      const history = readHistory(piece.dir).reverse();
      if (!history.length) {
        void vscode.window.showInformationMessage(`${piece.slug} has no revision history yet (it starts when the player first plays it).`);
        return;
      }
      const pick = await vscode.window.showQuickPick(
        history.map((r) => ({ label: `r${r.rev}`, description: `${ago(r.at)} · ${r.source}${playing && r.rev === live.state.appliedRev ? ' · playing' : ''}`, rev: r.rev })),
        { placeHolder: `Put which revision of ${piece.slug} in the editor?` },
      );
      if (!pick) {
        return;
      }
      rev = pick.rev;
    }
    await revert(piece, rev, lsp, output);
  });
  reg('engines.live.diff', async (rev: number, doc?: unknown) => {
    const piece = await choosePiece(doc);
    const snap = piece && revisionPath(piece.dir, rev);
    if (piece && snap) {
      await vscode.commands.executeCommand('vscode.diff', vscode.Uri.file(snap), vscode.Uri.file(piece.doc), `${piece.slug}: r${rev} ↔ the doc`);
    }
  });
  reg('engines.addPart', () => addPart());
  reg('engines.lsp.restart', () => lsp.restart());
  reg('engines.solo', lensCommand((id) => live.toggleSolo(String(id))));
  reg('engines.mute', lensCommand((id) => live.toggleMute(String(id))));
  reg('engines.loopBlock', lensCommand((k) => live.loopBlock(Number(k))));
  reg('engines.playFrom', lensCommand((k) => live.playFrom(Number(k))));

  await lsp.start();
  ctx.subscriptions.push(live.onDidChange(() => companions.changed()));
  return { version: API_VERSION, connect: (name, major) => companions.connect(name, major), companions, live, lsp, steps, jumper, panel: () => LivePanel.current };
}

/**
 * Add a part from the library (ADR 0039): engines-lsp's `source.addPart` actions at the cursor (a part
 * that acts on an instrument takes the one the cursor is in), picked from a list, applied as one
 * undoable edit — the player hears it like any other edit (when saved, by default).
 */
async function addPart(): Promise<void> {
  const ed = vscode.window.activeTextEditor;
  if (!ed || ed.document.languageId !== 'engines-loop') {
    void vscode.window.showInformationMessage('Open a loop doc to add a part to.');
    return;
  }
  const doc = ed.document;
  const version = doc.version; // what engines-lsp computes the edits against
  const partActions = async () => {
    const at = ed.selection.active;
    const actions = (await vscode.commands.executeCommand<vscode.CodeAction[]>('vscode.executeCodeActionProvider', doc.uri, new vscode.Range(at, at), ADD_PART.value)) ?? [];
    return actions.filter((a) => a.kind && ADD_PART.contains(a.kind) && a.edit);
  };
  const parts = await partActions();
  if (!parts.length) {
    void vscode.window.showInformationMessage('No part fits this doc as it is (a syntax error, or no instrument for it to act on).');
    return;
  }
  const pick = await vscode.window.showQuickPick(
    parts.map((a) => {
      const [label, ...rest] = a.title.replace(/^Add part: /, '').split(' — ');
      return { label, description: rest.join(' — '), action: a };
    }),
    { placeHolder: 'Add which part? (F2 on its id renames it everywhere afterwards)' },
  );
  if (!pick) {
    return;
  }
  // The doc may have changed while the list was open: ask engines-lsp again rather than apply an edit
  // made for the old text.
  const action = await currentAction(pick.action, version, () => doc.version, partActions);
  if (!action?.edit) {
    void vscode.window.showWarningMessage(`The doc changed while you picked, and "${pick.label}" no longer fits as it did — try Add a Part… again.`);
    return;
  }
  if (!(await vscode.workspace.applyEdit(action.edit))) {
    return;
  }
  const first = action.edit.get(doc.uri)[0];
  if (first) {
    const line = first.range.start.line + 1;
    ed.revealRange(new vscode.Range(line, 0, line, 0), vscode.TextEditorRevealType.InCenterIfOutsideViewport);
  }
}

/** Replace the piece doc's text with revision `rev` (one undoable edit; the player hears it when
 *  it's saved, or at once when it plays as you type). A revision from before schema v0.9 goes in
 *  migrated (`engines/migrate`, ADR 0054 U6): this build would refuse its v0.8 text. */
async function revert(piece: Piece, rev: number, lsp: Lsp, output: vscode.LogOutputChannel): Promise<void> {
  const snap = revisionPath(piece.dir, rev);
  if (!snap) {
    void vscode.window.showErrorMessage(`r${rev} isn't in ${piece.slug}'s history.`);
    return;
  }
  let text = Buffer.from(await vscode.workspace.fs.readFile(vscode.Uri.file(snap))).toString('utf8');
  let migrated = '';
  const version = schemaVersion(text);
  if (version && version !== SCHEMA_VERSION) {
    const m = await lsp.migrate(text);
    if (typeof m === 'string') {
      void vscode.window.showErrorMessage(`r${rev} is schema ${version}, and migrating it failed: ${m}`);
      return;
    }
    text = m.text;
    migrated = ` It was schema ${version}: migrated to v${SCHEMA_VERSION} (engines-migrate${m.notes.length ? `, ${m.notes.length} note${m.notes.length === 1 ? '' : 's'} in the Engines output` : ''}).`;
    for (const n of m.notes) {
      output.info(`r${rev} migrated: ${n}`);
    }
  }
  const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(piece.doc));
  await vscode.window.showTextDocument(doc, { preserveFocus: false, preview: false });
  const edit = new vscode.WorkspaceEdit();
  edit.replace(doc.uri, new vscode.Range(doc.positionAt(0), doc.positionAt(doc.getText().length)), text);
  if (await vscode.workspace.applyEdit(edit)) {
    player?.markRevert(doc); // the history records this text's next send as a revert
    const hear = player?.applyOn() === 'save' ? ' Save (⌘S) to hear it;' : '';
    void vscode.window.showInformationMessage(`The editor now holds r${rev}.${migrated}${hear} undo (⌘Z) brings the previous text back.`);
  }
}

/** Resolve with the state once `pred` holds (or `undefined` after `ms`). */
export function waitFor(live: LiveController, pred: (s: LiveState) => boolean, ms: number): Promise<LiveState | undefined> {
  if (pred(live.state)) {
    return Promise.resolve(live.state);
  }
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      sub.dispose();
      resolve(undefined);
    }, ms);
    const sub = live.onDidChange((s) => {
      if (pred(s)) {
        clearTimeout(timer);
        sub.dispose();
        resolve(s);
      }
    });
  });
}

/** Quit the player cleanly (status.json says `stopped`); the rest goes with ctx.subscriptions. */
export function deactivate(): Promise<void> | undefined {
  return player?.stop();
}

/** Where the carried-over settings are remembered (global scope in globalState, the rest per workspace). */
const CARRIED = 'engines.carriedOver';

/**
 * Copy each `sme.*` setting the user set to its `engines.*` key at the same scope (user, workspace,
 * and each folder of a multi-root workspace), and point `files.associations` entries at the new
 * language — once: the old keys stay as they were, an already-set new key wins, and a carried-over
 * setting the user later clears isn't copied again. Tells the user when it copied something.
 */
async function carryOver(ctx: vscode.ExtensionContext, output: vscode.LogOutputChannel): Promise<void> {
  const keys = Object.keys(ctx.extension.packageJSON?.contributes?.configuration?.properties ?? {});
  const folders = vscode.workspace.workspaceFile ? (vscode.workspace.workspaceFolders ?? []) : [];
  const seen: Seen[] = [];
  const assoc: Associations[] = [];
  const all = vscode.workspace.getConfiguration();
  for (const key of keys) {
    const [o, n] = [all.inspect(oldKey(key)), all.inspect(key)];
    seen.push({ key, scope: 'global', old: o?.globalValue, current: n?.globalValue });
    seen.push({ key, scope: 'workspace', old: o?.workspaceValue, current: n?.workspaceValue });
  }
  const a = all.inspect('files.associations');
  assoc.push({ scope: 'global', value: a?.globalValue }, { scope: 'workspace', value: a?.workspaceValue });
  for (const f of folders) {
    const c = vscode.workspace.getConfiguration(undefined, f.uri);
    const folder = f.uri.toString();
    for (const key of keys) {
      seen.push({ key, scope: 'folder', folder, old: c.inspect(oldKey(key))?.workspaceFolderValue, current: c.inspect(key)?.workspaceFolderValue });
    }
    assoc.push({ scope: 'folder', folder, value: c.inspect('files.associations')?.workspaceFolderValue });
  }
  const [doneGlobal, doneHere] = [ctx.globalState.get<string[]>(CARRIED, []), ctx.workspaceState.get<string[]>(CARRIED, [])];
  const settings = settingWrites(seen, new Set([...doneGlobal, ...doneHere]));
  const associations = associationWrites(assoc);
  const target = { global: vscode.ConfigurationTarget.Global, workspace: vscode.ConfigurationTarget.Workspace, folder: vscode.ConfigurationTarget.WorkspaceFolder };
  const write = async (w: Write): Promise<boolean> => {
    try {
      await vscode.workspace.getConfiguration(undefined, w.folder ? vscode.Uri.parse(w.folder) : undefined).update(w.key, w.value, target[w.scope]);
      return true;
    } catch (e) {
      output.warn(`couldn't carry ${w.key === 'files.associations' ? w.key : oldKey(w.key)} over (${w.scope}): ${(e as Error).message}`);
      return false;
    }
  };
  const copied: Write[] = [];
  for (const w of settings) {
    if (await write(w)) {
      copied.push(w);
    }
  }
  let rewritten = 0;
  for (const w of associations) {
    rewritten += (await write(w)) ? 1 : 0;
  }
  if (copied.length) {
    const ids = (scopes: string[]) => copied.filter((w) => scopes.includes(w.scope)).map(doneId);
    await ctx.globalState.update(CARRIED, [...doneGlobal, ...ids(['global'])]);
    await ctx.workspaceState.update(CARRIED, [...doneHere, ...ids(['workspace', 'folder'])]);
    output.info(`carried over from SME Loop Docs: ${copied.map((w) => `${oldKey(w.key)} → ${w.key} (${w.scope})`).join(', ')}`);
  }
  const message = carriedOverMessage(copied.length, rewritten);
  if (message) {
    void vscode.window.showInformationMessage(message);
  }
}

/** Offer to uninstall the old extension; a reload finishes it. */
async function offerUninstallOld(): Promise<void> {
  const pick = await vscode.window.showWarningMessage(
    `Engines replaces SME Loop Docs (${OLD_EXTENSION_ID}), which is still installed. Both claim piece docs and each would start a player. Uninstall it?`,
    'Uninstall',
    'Not now',
  );
  if (pick !== 'Uninstall') {
    return;
  }
  await vscode.commands.executeCommand('workbench.extensions.uninstallExtension', OLD_EXTENSION_ID);
  const reload = await vscode.window.showInformationMessage('SME Loop Docs is uninstalled. Reload the window to finish.', 'Reload Window');
  if (reload) {
    await vscode.commands.executeCommand('workbench.action.reloadWindow');
  }
}

/** A piece's `.live/status.json` (what the player last wrote), raw. */
function readStatus(dir: string | undefined): Record<string, unknown> | undefined {
  try {
    return dir ? JSON.parse(fs.readFileSync(path.join(dir, '.live', 'status.json'), 'utf8')) : undefined;
  } catch {
    return undefined;
  }
}
