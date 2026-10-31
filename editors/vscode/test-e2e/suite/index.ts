// The e2e suite, run inside VS Code by @vscode/test-electron. A tiny sequential runner (no mocha):
// each step logs, the first failure throws (VS Code exits non-zero).

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';
import type { Api } from '../../src/extension';
import { waitFor } from '../../src/extension';

const WS = process.env.ENGINES_E2E_WS ?? '';

async function until<T>(what: string, f: () => T | undefined | false | Promise<T | undefined | false>, ms = 15_000): Promise<T> {
  const t0 = Date.now();
  for (;;) {
    const v = await f();
    if (v) {
      return v;
    }
    if (Date.now() - t0 > ms) {
      throw new Error(`timed out: ${what}`);
    }
    await new Promise((r) => setTimeout(r, 50));
  }
}

/** `.live/status.json`'s `editor` (what Claude reads before writing the doc). */
function editorStatus(slug: string): { unsaved?: boolean } | undefined {
  try {
    return JSON.parse(fs.readFileSync(path.join(WS, 'pieces', slug, '.live', 'status.json'), 'utf8')).editor;
  } catch {
    return undefined;
  }
}

async function open(slug: string): Promise<vscode.TextEditor> {
  const doc = await vscode.workspace.openTextDocument(path.join(WS, 'pieces', slug, `${slug}.json5`));
  return vscode.window.showTextDocument(doc, { preview: false });
}

function posOf(doc: vscode.TextDocument, needle: string, delta = 0): vscode.Position {
  const i = doc.getText().indexOf(needle);
  assert.ok(i >= 0, `no ${needle}`);
  return doc.positionAt(i + delta);
}

async function replace(doc: vscode.TextDocument, from: string, to: string): Promise<void> {
  const i = doc.getText().indexOf(from);
  assert.ok(i >= 0, `no ${from}`);
  const edit = new vscode.WorkspaceEdit();
  edit.replace(doc.uri, new vscode.Range(doc.positionAt(i), doc.positionAt(i + from.length)), to);
  assert.ok(await vscode.workspace.applyEdit(edit));
}

export async function run(): Promise<void> {
  const steps: [string, () => Promise<void>][] = [];
  const step = (name: string, fn: () => Promise<void>) => steps.push([name, fn]);
  let api!: Api;
  let ed!: vscode.TextEditor;

  /** The revision of the next `applied` event at or after `rev` (rejects after `ms`, like `until`). */
  const nextApplied = (rev: number, ms = 10_000) =>
    new Promise<number>((resolve, reject) => {
      const timer = setTimeout(() => {
        sub.dispose();
        reject(new Error(`timed out: r${rev} applied`));
      }, ms);
      const sub = api.live.onEvent((ev) => {
        if (ev.type === 'applied' && ev.rev >= rev) {
          clearTimeout(timer);
          sub.dispose();
          resolve(ev.rev);
        }
      });
    });

  step('a piece doc opens as engines-loop and activates the extension', async () => {
    ed = await open('demo');
    assert.equal(ed.document.languageId, 'engines-loop');
    const ext = vscode.extensions.getExtension<Api>('LamplightMachines.engines');
    assert.ok(ext);
    api = await ext.activate();
    await until('the language client', () => api.lsp.client?.isRunning());
  });

  step('the settings the SME extension had carry over', async () => {
    // run.ts seeds them under their old keys: the user's trace + association, the workspace's debounce.
    await until('the carried-over settings', () => vscode.workspace.getConfiguration().inspect('engines.trace.server')?.globalValue === 'messages');
    const now = vscode.workspace.getConfiguration();
    assert.equal(now.inspect('engines.live.debounceMs')?.workspaceValue, 150);
    assert.equal(now.inspect('sme.live.debounceMs')?.workspaceValue, 150, 'the old key is left as it was');
    assert.deepEqual(now.inspect<Record<string, string>>('files.associations')?.globalValue, { '*.loopdoc': 'engines-loop' });
  });

  step('Engines Studio adds its features through the companion API', async () => {
    const commands = await vscode.commands.getCommands(true);
    if (process.env.ENGINES_E2E_STUDIO !== '1') {
      // The free build alone: none of Studio's commands.
      assert.ok(!commands.includes('engines.buildLiveSet') && !commands.includes('engines.live.toggleMidi'));
      return;
    }
    await vscode.extensions.getExtension('LamplightMachines.engines-studio')?.activate();
    await until('Studio’s commands', async () => (await vscode.commands.getCommands(true)).includes('engines.buildLiveSet'));
    // Its MIDI switch, on a strip of a routed session from a player with the mirror.
    const routing = { channels: 16, pairs: [{ pair: '1/2', source: 'inst:pad', name: 'pad' }], overflow: '3/4', unrouted: [], stale: false };
    const state = { ...api.live.state, routing, capabilities: ['midi_mirror'] };
    assert.deepEqual(api.companions.stripActions('pad', state).map((a) => [a.id, a.on]), [['midi', false]]);
    assert.deepEqual(api.companions.stripActions('pad', { ...state, capabilities: [] }), [], 'the free player: no switch');
  });

  step('diagnostics land on the exact token', async () => {
    const typo = await open('typo');
    const d = await until('a diagnostic', () => vscode.languages.getDiagnostics(typo.document.uri).find((x) => x.code === 'unknown-key'));
    assert.equal(typo.document.getText(d.range), 'cutof_hz');
    assert.equal(d.severity, vscode.DiagnosticSeverity.Error);
    ed = await open('demo');
  });

  step('completion offers scale-aware offsets', async () => {
    const doc = ed.document;
    const list = await vscode.commands.executeCommand<vscode.CompletionList>('vscode.executeCompletionItemProvider', doc.uri, posOf(doc, 'offset: 7', 8));
    const items = list.items.map((i) => `${typeof i.label === 'string' ? i.label : i.label.label} · ${i.detail ?? ''}`);
    assert.ok(items[0].startsWith('7 · A4'), items.slice(0, 4).join(' | '));
  });

  step('hover reads an offset as a note', async () => {
    const doc = ed.document;
    const hovers = await vscode.commands.executeCommand<vscode.Hover[]>('vscode.executeHoverProvider', doc.uri, posOf(doc, 'offset: 7', 8));
    const text = hovers.flatMap((h) => h.contents.map((c) => (typeof c === 'string' ? c : c.value))).join('\n');
    assert.match(text, /A4/);
  });

  step('inlay hints and CodeLens', async () => {
    const doc = ed.document;
    const hints = await vscode.commands.executeCommand<vscode.InlayHint[]>('vscode.executeInlayHintProvider', doc.uri, new vscode.Range(0, 0, doc.lineCount, 0));
    const labels = hints.map((h) => (typeof h.label === 'string' ? h.label : h.label.map((p) => p.value).join('')));
    assert.ok(labels.includes('A4') && labels.includes('1.0 s loop'), labels.join(' | '));
    const lenses = await vscode.commands.executeCommand<vscode.CodeLens[]>('vscode.executeCodeLensProvider', doc.uri);
    const titles = lenses.map((l) => l.command?.title);
    // Icon-led (ADR 0040), with a tooltip saying what a click does.
    assert.ok(titles.includes('$(circle-large-outline) solo') && titles.includes('$(unmute)') && titles.some((t) => t?.startsWith('$(sync) loop · ')), titles.join(' | '));
    assert.ok(lenses.some((l) => l.command?.tooltip === 'Mute pad'), 'the mute lens says what it does');
  });

  step('play: the headless player starts and the panel opens', async () => {
    await vscode.commands.executeCommand('engines.live.start', ed.document.uri);
    const s = await waitFor(api.live, (s) => s.phase === 'running' || s.phase === 'failed', 20_000);
    assert.equal(s?.phase, 'running', s?.message ?? '');
    assert.deepEqual(s?.instruments, ['pad']);
    assert.equal(s?.key?.name, 'D dorian');
    await until('the panel model', () => api.panel()?.lastModel?.phase === 'running');
  });

  step('an unsaved edit waits (marked); saving plays it', async () => {
    await replace(ed.document, 'level_db: -7.96', 'level_db: -9.12');
    assert.ok(ed.document.isDirty);
    await until('the unsaved mark', () => api.live.state.unsaved && api.panel()?.lastModel?.unsaved);
    await until('status.json says so (for Claude)', () => editorStatus('demo')?.unsaved === true);
    await new Promise((r) => setTimeout(r, 800)); // well past the debounce
    assert.equal(api.live.state.rev, 1, 'nothing reached the player before the save');
    const applied = nextApplied(2);
    await ed.document.save();
    assert.equal(await applied, 2);
    assert.equal(api.live.state.unsaved, false);
    await until('status.json clears it', () => editorStatus('demo')?.unsaved === false);
  });

  step('a broken save is refused, the good revision keeps playing', async () => {
    await replace(ed.document, 'cutoff_hz: 800', 'cutof_hz: 800');
    await ed.document.save();
    const s = await waitFor(api.live, (s) => !!s.rejected, 10_000);
    assert.ok(s?.rejected);
    assert.equal(s.appliedRev, 2);
    await new Promise((r) => setTimeout(r, 800)); // the file watcher sees the same save: not news
    assert.equal(api.live.state.activity.filter((a) => a.kind === 'error' && a.text.includes('refused')).length, 1, 'refused once');
    await replace(ed.document, 'cutof_hz: 800', 'cutoff_hz: 800');
    await ed.document.save();
    await waitFor(api.live, (s) => !s.rejected, 10_000);
  });

  step('with applyOn "edit", typing is heard unsaved', async () => {
    const cfg = vscode.workspace.getConfiguration('engines.live');
    await cfg.update('applyOn', 'edit', vscode.ConfigurationTarget.Workspace);
    try {
      const applied = nextApplied(3);
      await replace(ed.document, 'level_db: -9.12', 'level_db: -10.46');
      assert.ok(ed.document.isDirty);
      assert.equal(await applied, 3);
      assert.equal(api.live.state.unsaved, false, 'nothing is waiting for a save');
    } finally {
      await cfg.update('applyOn', undefined, vscode.ConfigurationTarget.Workspace);
    }
    await ed.document.save();
  });

  step('CodeLens commands mute and the lens title follows', async () => {
    await vscode.commands.executeCommand('engines.mute', ed.document.uri.toString(), 'pad');
    await waitFor(api.live, (s) => s.muted.includes('pad'), 5000);
    const titles = await until('the ● muted lens', async () => {
      const lenses = await vscode.commands.executeCommand<vscode.CodeLens[]>('vscode.executeCodeLensProvider', ed.document.uri);
      const t = lenses.map((l) => l.command?.title);
      return t.includes('$(mute) muted') && t;
    });
    assert.ok(titles);
    await vscode.commands.executeCommand('engines.mute', ed.document.uri.toString(), 'pad');
    await waitFor(api.live, (s) => !s.muted.includes('pad'), 5000);
  });

  step('hold ⇄ drift', async () => {
    const before = api.live.state.optimize;
    await vscode.commands.executeCommand('engines.live.toggleHold');
    const s = await waitFor(api.live, (s) => s.optimize !== before, 5000);
    assert.equal(s?.optimize, before === 'hold' ? 'drift' : 'hold');
  });

  step('the playing step is highlighted where it is written', async () => {
    const r = await until('a highlighted step', () => api.steps.current.length > 0 && api.steps.current[0]);
    assert.match(ed.document.getText(r), /^\{ step: [02], voicing/);
  });

  step('song audition from a CodeLens', async () => {
    await vscode.commands.executeCommand('engines.loopBlock', ed.document.uri.toString(), 1);
    const s = await waitFor(api.live, (s) => s.song?.mode === 'block', 5000);
    assert.equal(s?.song?.block, 1);
    await vscode.commands.executeCommand('engines.loopBlock', ed.document.uri.toString(), 1);
    await waitFor(api.live, (s) => s.song?.mode === 'loop', 5000);
  });

  step('a name in the panel jumps the editor to its path (and flashes)', async () => {
    // The panel's link posts `reveal`; the extension runs this command with the path.
    await vscode.commands.executeCommand('workbench.action.focusFirstEditorGroup');
    ed.selection = new vscode.Selection(0, 0, 0, 0);
    assert.equal(await vscode.commands.executeCommand('engines.live.reveal', ['instruments', 'pad']), true);
    const at = vscode.window.activeTextEditor;
    assert.equal(at?.document.uri.fsPath, ed.document.uri.fsPath);
    const sel = at!.selection.active;
    assert.equal(at!.document.getText(new vscode.Range(sel, sel.translate(0, 4))), 'pad:', 'the cursor is on the key');
    // A song entry whose index went stale is found by its label.
    assert.equal(await vscode.commands.executeCommand('engines.live.reveal', ['song', 'sequence', 0], 'thin'), true);
    const block = vscode.window.activeTextEditor!.selection.active;
    assert.match(vscode.window.activeTextEditor!.document.lineAt(block.line).text.slice(block.character), /^\{ matrix: "thin"/);
  });

  step('revert puts an old revision in the editor (recorded as a revert)', async () => {
    const before = api.live.state.rev;
    await vscode.commands.executeCommand('engines.live.revert', 1);
    await until('the r1 text', () => ed.document.getText().includes('level_db: -7.96,'));
    // Going back is a new revision (history is a timeline), heard like any edit once saved.
    await ed.document.save();
    const s = await waitFor(api.live, (s) => s.appliedRev > before, 10_000);
    assert.ok(s, `no revision after r${before} was applied`);
    const index = fs.readFileSync(path.join(WS, 'pieces', 'demo', '.live', 'history', 'index.jsonl'), 'utf8').trim().split('\n');
    assert.equal(JSON.parse(index[index.length - 1]).source, 'revert');
  });

  step('reverting to a pre-v0.9 revision puts it in the editor migrated (ADR 0054 U6)', async () => {
    // A revision from before the break, as sme-live recorded it before the rename: v0.8 text this build refuses.
    const dir = path.join(WS, 'pieces', 'demo', '.live', 'history');
    const v08 = '{\n  schema_version: "0.8",\n  transport: { bpm: 240, master_steps: 4 },\n  harmony: { root: "D", mode: "dorian" },\n  global: { master_gain: 0.5 },\n  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level: 0.4, cutoff: 0.5, sustain: 1.0 } } },\n  modulators: { seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } } ] } },\n  matrix: { m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },\n}\n';
    fs.writeFileSync(path.join(dir, 'r00900-v08.json5'), v08);
    fs.appendFileSync(path.join(dir, 'index.jsonl'), JSON.stringify({ rev: 900, hash: 'v08', file: 'r00900-v08.json5', at: Math.floor(Date.now() / 1000), source: 'sme-live' }) + '\n');
    await vscode.commands.executeCommand('engines.live.revert', 900);
    await until('the migrated r900', () => ed.document.getText().includes('level_db: -7.96'));
    const text = ed.document.getText();
    assert.ok(text.includes('schema_version: "0.9"') && text.includes('cutoff_hz: 800') && text.includes('master_gain_db: -6.02'), text);
    assert.ok(!/\blevel: 0\.4/.test(text), 'no v0.8 key is pasted');
    // Back to the saved text for the steps that follow.
    await vscode.commands.executeCommand('workbench.action.files.revert');
    await until('the saved text', () => !ed.document.isDirty);
  });

  step('a part from the library is added and heard', async () => {
    const before = api.live.state.rev;
    const actions = await vscode.commands.executeCommand<vscode.CodeAction[]>('vscode.executeCodeActionProvider', ed.document.uri, new vscode.Range(0, 0, 0, 0), 'source.addPart');
    const pulse = actions.find((a) => a.title.startsWith('Add part: plucked pulse'));
    assert.ok(pulse?.edit, actions.map((a) => a.title).join(' | '));
    assert.ok(await vscode.workspace.applyEdit(pulse.edit));
    assert.match(ed.document.getText(), /m_pulse: \{ source: "pulse_seq"/);
    await ed.document.save();
    const s = await waitFor(api.live, (s) => s.instruments.includes('pulse') && s.appliedRev > before, 10_000);
    assert.ok(s, 'the player plays the doc with the pulse in it');
  });

  step('stop quits the player', async () => {
    await vscode.commands.executeCommand('engines.live.stop');
    assert.equal(api.live.state.phase, 'off');
    await until('the step highlight cleared', () => api.steps.current.length === 0);
    await new Promise((r) => setTimeout(r, 400)); // a late paint mustn't light it again
    assert.equal(api.steps.current.length, 0);
    await vscode.commands.executeCommand('workbench.action.files.revert');
  });

  step('a doc with errors on disk fails to start, and starts once fixed and saved', async () => {
    const typo = await open('typo');
    await vscode.commands.executeCommand('engines.live.start', typo.document.uri);
    const failed = await waitFor(api.live, (s) => s.phase === 'failed', 15_000);
    assert.match(failed?.message ?? '', /errors/);
    await replace(typo.document, 'cutof_hz', 'cutoff_hz');
    await typo.document.save();
    const s = await waitFor(api.live, (s) => s.phase === 'running', 15_000);
    assert.equal(s?.piece, 'typo');
    await vscode.commands.executeCommand('engines.live.stop');
  });

  for (const [name, fn] of steps) {
    const t0 = Date.now();
    try {
      await fn();
      console.log(`  ✔ ${name} (${Date.now() - t0} ms)`);
    } catch (e) {
      console.error(`  ✖ ${name}`);
      throw e;
    }
  }
}
