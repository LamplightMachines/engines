// The live player for one piece at a time: spawns `engines-live <doc> --stdio`, sends the doc as you
// save it (`engines.live.applyOn: "save"`, the default) or the buffer as you type (`"edit"`, debounced),
// turns panel / CodeLens / command actions into protocol commands, and folds the events into a
// `LiveState` everyone else watches.

import { execFile } from 'node:child_process';
import * as path from 'node:path';
import * as vscode from 'vscode';
import { offerBuild, resolveTool } from '../binaries';
import type { Companions } from '../companion';
import { livePlayer, type Piece } from '../piece';
import { liveArgs } from './args';
import { Debouncer } from './debounce';
import { deviceItems, outputCommand, parseDevices, routeItems, sameChoice, type OutputChoice } from './devices';
import { LiveProcess } from './process';
import type { LiveCommand, LiveEvent } from './protocol';
import { exited, initialState, previewAsked, reduce, restarting, starting, type LiveState } from './state';

export function samePath(a: string, b: string): boolean {
  return path.resolve(a) === path.resolve(b);
}

/** When the player hears the editor: on each save (the default) or as you type. */
export type ApplyOn = 'save' | 'edit';

/** What the routing-pairs choice does, unless a companion's step says more. */
const PAIRS_DESCRIPTION = 'one stereo pair each, from the piece’s routing map';

export class LiveController implements vscode.Disposable {
  state: LiveState = initialState();
  piece: Piece | undefined;
  private proc: LiveProcess | undefined;
  /** The quit in flight: it drains for up to the quit timeout (then kills), and until it's done the
   *  old process is still `proc` — a ▶ or a CodeLens action meanwhile waits for it (`settled`). */
  private stopping: Promise<void> | undefined;
  private readonly changed = new vscode.EventEmitter<LiveState>();
  readonly onDidChange = this.changed.event;
  /** Fires when the piece's revision history may have grown (accepted / external change). */
  private readonly historyChanged = new vscode.EventEmitter<void>();
  readonly onDidChangeHistory = this.historyChanged.event;
  /** A live event, as it arrives (tests and the preview notification listen). */
  private readonly evented = new vscode.EventEmitter<LiveEvent>();
  readonly onEvent = this.evented.event;
  private readonly debounce: Debouncer;
  private sentVersion: number | undefined;
  /** What to restore when a crashed player comes back. */
  private wanted: { muted: string[]; soloed: string[]; optimize: 'hold' | 'drift' | undefined } = { muted: [], soloed: [], optimize: undefined };
  private crashed = false;
  private launching = false;
  /** Why the player mustn't start now (e.g. the old extension's would start too), else `undefined`. */
  refusal: (() => string | undefined) | undefined;
  /** Companions' output-picker steps. */
  companions: Companions | undefined;
  /** The last `ready`, raw: protocol fields this extension doesn't know reach a companion. */
  lastReady: Record<string, unknown> | undefined;
  /** A revert just put a revision in the buffer at this version: its send is recorded as `revert`. */
  private revertMark: { path: string; version: number } | undefined;
  /** What the player was last told about the buffer (`editor` → status.json, for Claude). */
  private toldUnsaved: boolean | undefined;
  private readonly subs: vscode.Disposable[] = [];
  /** The output the player was last told to use (its launch settings, then each switch) — and the
   *  one before, to put the settings back if the player refuses a switch. */
  private outputNow: OutputChoice | undefined;
  private outputBefore: OutputChoice | undefined;
  private outputTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(private readonly output: vscode.OutputChannel) {
    this.debounce = new Debouncer(() => this.sendBuffer(), this.config().get<number>('debounceMs', 300));
    this.subs.push(
      vscode.workspace.onDidChangeTextDocument((e) => {
        if (!this.isPieceDoc(e.document)) {
          return;
        }
        if (this.revertMark && e.document.version !== this.revertMark.version) {
          this.revertMark = undefined; // edited since: no longer the reverted text
        }
        if (e.contentChanges.length && this.applyOn() === 'edit') {
          this.debounce.trigger();
        }
        this.syncUnsaved();
        this.tellEditor();
      }),
      vscode.workspace.onDidSaveTextDocument((doc) => {
        if (!this.isPieceDoc(doc)) {
          return;
        }
        if (this.state.phase === 'failed' && !this.launching && this.piece) {
          // A player that couldn't start (the doc had errors on disk) tries again on the next save.
          void this.start(this.piece);
        } else if (this.applyOn() === 'save') {
          this.sendBuffer(); // the saved text — the player hears saves, not keystrokes
        }
        this.syncUnsaved();
        this.tellEditor();
      }),
      vscode.workspace.onDidCloseTextDocument(() => {
        this.syncUnsaved();
        this.tellEditor();
      }),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration('engines.live.debounceMs')) {
          this.debounce.ms = this.config().get<number>('debounceMs', 300);
        }
        if (e.affectsConfiguration('engines.live.outputDevice') || e.affectsConfiguration('engines.live.routeSources')) {
          // Both settings change together from the picker: switch once, after the second.
          clearTimeout(this.outputTimer);
          this.outputTimer = setTimeout(() => this.followOutputSetting(), 100);
        }
        if (e.affectsConfiguration('engines.live.applyOn')) {
          if (this.applyOn() === 'edit') {
            this.debounce.trigger(); // catch the player up with the buffer
          } else {
            this.debounce.cancel();
          }
          this.syncUnsaved();
        }
      }),
    );
  }

  private config(): vscode.WorkspaceConfiguration {
    return vscode.workspace.getConfiguration('engines.live');
  }

  applyOn(): ApplyOn {
    return this.config().get<string>('applyOn', 'save') === 'edit' ? 'edit' : 'save';
  }

  /** The editor holds edits the player hasn't heard (and won't until they're saved). */
  private hasUnsaved(): boolean {
    return !!this.piece && this.state.phase !== 'off' && this.applyOn() === 'save' && !!this.document()?.isDirty;
  }

  private syncUnsaved(): void {
    if (this.hasUnsaved() !== this.state.unsaved) {
      this.setState(this.state);
    }
  }

  /** Tell the player whether the buffer has unsaved edits (whatever `applyOn` says): status.json
   *  carries it, and Claude asks for a save before writing the file — the edits would collide. */
  private tellEditor(force = false): void {
    if (!this.proc?.isReady) {
      return;
    }
    const unsaved = !!this.document()?.isDirty;
    if (force || unsaved !== this.toldUnsaved) {
      this.toldUnsaved = unsaved;
      this.proc.send({ type: 'editor', unsaved });
    }
  }

  /** A child process is alive or about to come back (not one that's quitting). */
  get active(): boolean {
    return !!this.proc && !this.proc.isStopping && (this.proc.running || this.proc.restarting);
  }

  /** Resolves once no quit is draining: what an action right after ⏏ waits for before it decides
   *  whether a player is there to take it. */
  async settled(): Promise<void> {
    while (this.stopping) {
      await this.stopping;
    }
  }

  isPieceDoc(doc: vscode.TextDocument): boolean {
    return !!this.piece && doc.uri.scheme === 'file' && samePath(doc.uri.fsPath, this.piece.doc);
  }

  /** The open document of the playing piece, if any. */
  document(): vscode.TextDocument | undefined {
    return vscode.workspace.textDocuments.find((d) => this.isPieceDoc(d));
  }

  /** Play `piece` (stopping any other piece's player). */
  async start(piece: Piece): Promise<void> {
    await this.settled(); // ▶ right after ⏏: the old player is still quitting — then start afresh
    if (this.active && this.piece && samePath(this.piece.doc, piece.doc)) {
      if (this.proc?.isReady && !this.state.playing) {
        this.send({ type: 'play' });
      }
      return;
    }
    if (this.launching) {
      return; // a start is already on its way (its save mustn't re-trigger one)
    }
    this.launching = true;
    try {
      await this.launch(piece);
    } finally {
      this.launching = false;
    }
  }

  private async launch(piece: Piece): Promise<void> {
    const why = this.refusal?.();
    if (why) {
      void vscode.window.showWarningMessage(why);
      return;
    }
    await this.stop();
    const bin = resolveTool('engines-live', piece.root);
    if (!bin) {
      if (await offerBuild('engines-live', piece.root)) {
        return this.launch(piece);
      }
      return;
    }
    if (!(await this.checkOtherPlayer(piece))) {
      return;
    }
    // engines-live starts from the file on disk. Playing as you type, that's the buffer: save it first.
    // Playing saves, the unsaved edits wait for the next save like any other.
    const open = vscode.workspace.textDocuments.find((d) => d.uri.scheme === 'file' && samePath(d.uri.fsPath, piece.doc));
    if (open?.isDirty && this.applyOn() === 'edit') {
      await open.save();
    }
    const args = this.args(piece);
    this.outputNow = this.outputSetting();
    this.outputBefore = undefined;
    this.output.appendLine(`[engines-live] ${bin} ${args.join(' ')}`);
    const proc = new LiveProcess({ command: bin, args, cwd: piece.root });
    this.piece = piece;
    this.proc = proc;
    this.sentVersion = undefined;
    this.crashed = false;
    this.wanted = { muted: [], soloed: [], optimize: undefined };
    proc.on('event', (ev) => this.onEvent_(proc, ev));
    proc.on('log', (line) => this.output.appendLine(`[engines-live] ${line}`));
    proc.on('exit', (info) => {
      if (proc !== this.proc) {
        return;
      }
      this.crashed = !info.expected && info.ready;
      this.setState(exited(this.state, { expected: info.expected, restarting: proc.restarting, message: info.message }));
      if (!info.expected && !info.ready) {
        void vscode.window.showErrorMessage(`engines-live couldn't play ${piece.slug}: ${info.message ?? 'it exited'}. It tries again when you save the doc.`, 'Show Output').then((pick) => {
          if (pick) {
            this.output.show(true);
          }
        });
      }
    });
    proc.on('restarting', ({ attempt, delay }) => {
      this.output.appendLine(`[engines-live] crashed — restart ${attempt} in ${delay} ms`);
      this.setState(restarting(this.state, piece.slug)); // the crash reason stays up meanwhile
    });
    proc.on('gaveUp', (info) => {
      this.setState(exited(this.state, { expected: false, restarting: false, message: `crashed repeatedly (${info.message ?? 'no message'})` }));
    });
    this.setState(starting(this.state, piece.slug));
    proc.start();
  }

  private args(piece: Piece): string[] {
    const cfg = this.config();
    return liveArgs(piece.doc, {
      headless: cfg.get<boolean>('headless', false),
      outputDevice: cfg.get<string>('outputDevice', ''),
      routeSources: cfg.get<boolean>('routeSources', false),
      extra: cfg.get<string[]>('args', []),
    });
  }

  /** The output the settings ask for. */
  private outputSetting(): OutputChoice {
    const cfg = this.config();
    return { device: cfg.get<string>('outputDevice', '').trim(), route: cfg.get<boolean>('routeSources', false) };
  }

  /** The output settings changed (the picker, or an edit of settings.json): a running player switches
   *  in place (ADR 0044 D7), and a crash restart or the next start uses them. */
  private followOutputSetting(): void {
    const want = this.outputSetting();
    if (this.proc && this.piece) {
      this.proc.spec.args = this.args(this.piece);
    }
    if (!this.proc?.isReady || this.config().get<boolean>('headless', false) || (this.outputNow && sameChoice(want, this.outputNow))) {
      return;
    }
    this.outputBefore = this.outputNow;
    this.outputNow = want;
    this.output.appendLine(`[engines-live] output → ${want.device || 'the system output'}${want.device && want.route ? ' (each source on its own pair)' : ''}`);
    this.proc.send({ ...outputCommand(want), id: 'output' });
  }

  /** The output picker: a device, then — for a multichannel one — each source on its own pair or the
   *  mix. Writes `engines.live.outputDevice` / `routeSources` (where they're set, else the user's
   *  settings: a device is this machine's), which switches a running player. */
  async chooseOutput(): Promise<void> {
    const root = this.piece?.root ?? vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    const bin = resolveTool('engines-live', root);
    if (!bin) {
      await offerBuild('engines-live', root);
      return;
    }
    const json = await new Promise<string>((resolve) => execFile(bin, ['--list-devices', '--json'], { timeout: 10_000 }, (_e, out) => resolve(String(out ?? ''))));
    const devices = parseDevices(json);
    const current = this.outputSetting();
    const pick = await vscode.window.showQuickPick(deviceItems(devices, current), { title: 'Engines: play on…', placeHolder: devices.length ? 'Output device' : 'No devices listed — engines-live --list-devices found none', matchOnDescription: true });
    if (!pick) {
      return;
    }
    let route = false;
    const piece = this.piece;
    const ctx = { piece: piece && { slug: piece.slug, dir: piece.dir, doc: piece.doc, root: piece.root }, device: pick.device, channels: pick.channels };
    if (pick.device && pick.channels > 2) {
      const pairs = this.companions?.pairsDescription(ctx, PAIRS_DESCRIPTION) ?? PAIRS_DESCRIPTION;
      const how = await vscode.window.showQuickPick(routeItems(pick.device, current, pairs), { title: `Engines: on ${pick.device}…` });
      if (!how) {
        return;
      }
      route = how.route;
    }
    // Companions' output steps run before the switch.
    if (pick.device) {
      await this.companions?.beforeSwitch({ ...ctx, route });
    }
    const cfg = this.config();
    await this.writeSetting(cfg, 'outputDevice', pick.device);
    await this.writeSetting(cfg, 'routeSources', route);
  }

  private async writeSetting(cfg: vscode.WorkspaceConfiguration, key: string, value: unknown): Promise<void> {
    const at = cfg.inspect(key);
    const target =
      at?.workspaceFolderValue !== undefined
        ? vscode.ConfigurationTarget.WorkspaceFolder
        : at?.workspaceValue !== undefined
          ? vscode.ConfigurationTarget.Workspace
          : vscode.ConfigurationTarget.Global;
    await cfg.update(key, value, target);
  }

  /** Quit the player (frees the audio device). A second call while it drains joins the first. */
  stop(): Promise<void> {
    this.debounce.cancel();
    if (!this.stopping) {
      const proc = this.proc;
      if (!proc) {
        return Promise.resolve();
      }
      const quitting: Promise<void> = this.quit(proc).finally(() => {
        if (this.stopping === quitting) {
          this.stopping = undefined;
        }
      });
      this.stopping = quitting;
    }
    return this.stopping;
  }

  private async quit(proc: LiveProcess): Promise<void> {
    await proc.stop();
    if (this.proc === proc) {
      this.proc = undefined;
      this.setState(exited(this.state, { expected: true, restarting: false }));
    }
  }

  /** Send a protocol command; `false` (and a hint) when no player is ready. */
  send(cmd: LiveCommand): boolean {
    if (!this.proc?.isReady) {
      void vscode.window.showInformationMessage('Start the player first (Engines: Play This Piece).');
      return false;
    }
    return this.proc.send(cmd);
  }

  /** Send the editor's text now (on save, or debounced as you type). */
  sendBuffer(): void {
    const doc = this.document();
    if (!doc || !this.proc?.isReady || doc.version === this.sentVersion) {
      return;
    }
    this.sentVersion = doc.version;
    const reverted = this.revertMark && samePath(this.revertMark.path, doc.uri.fsPath) && this.revertMark.version === doc.version;
    this.proc.send({ type: 'doc', text: doc.getText(), version: doc.version, source: reverted ? 'revert' : 'editor' });
  }

  /** `doc` now holds a reverted revision (`engines.live.revert`): its send is recorded as `revert`. */
  markRevert(doc: vscode.TextDocument): void {
    this.revertMark = { path: doc.uri.fsPath, version: doc.version };
  }

  // ---- actions (panel, CodeLens, commands) ------------------------------------------------------

  toggleMute(id: string): void {
    const on = !this.state.muted.includes(id);
    if (this.send({ type: 'mute', instrument: id, on })) {
      this.wanted.muted = on ? [...this.wanted.muted, id] : this.wanted.muted.filter((x) => x !== id);
    }
  }

  toggleSolo(id: string): void {
    const on = !this.state.soloed.includes(id);
    if (this.send({ type: 'solo', instrument: id, on })) {
      this.wanted.soloed = on ? [...this.wanted.soloed, id] : this.wanted.soloed.filter((x) => x !== id);
    }
  }

  clearSolo(): void {
    if (this.send({ type: 'clear_solo' })) {
      this.wanted.soloed = [];
    }
  }

  /** Hold (play exactly what's written — the fine-tuning mode) or drift. */
  setOptimize(optimize: 'hold' | 'drift'): void {
    if (this.send({ type: 'control', optimize })) {
      this.wanted.optimize = optimize;
    }
  }

  toggleHold(): void {
    this.setOptimize(this.state.optimize === 'hold' ? 'drift' : 'hold');
  }

  setOrrery(on: boolean): void {
    this.send({ type: 'control', orrery: on });
  }

  setLimiter(on: boolean): void {
    this.send({ type: 'limiter', on });
  }

  /** Loop song block `k` (again: back to the loop). */
  loopBlock(k: number): void {
    const s = this.state.song;
    this.send(s?.mode === 'block' && s.block === k ? { type: 'song', action: 'stop' } : { type: 'song', action: 'loop_block', block: k });
  }

  /** Play the song from block `k` (again: back to the loop). */
  playFrom(k: number): void {
    const s = this.state.song;
    this.send(s?.mode === 'song' && s.block === k ? { type: 'song', action: 'stop' } : { type: 'song', action: 'play_from', block: k });
  }

  songStop(): void {
    this.send({ type: 'song', action: 'stop' });
  }

  play(): void {
    this.send({ type: 'play' });
  }

  /** Stop the audio (the player stays open, ready to play again). */
  pause(): void {
    this.send({ type: 'stop' });
  }

  restart(): void {
    this.send({ type: 'restart' });
  }

  /** Render `seconds` offline into `.live/preview.wav` (and, with `stems`, `.live/preview_stems/`). */
  preview(seconds: number, stems = false): void {
    if (this.send({ type: 'preview', seconds, stems })) {
      this.setState(previewAsked(this.state));
      // engines-live logs the render's progress and results on stderr → this channel.
      this.output.show(true);
    }
  }

  // ---- internals ---------------------------------------------------------------------------------

  private onEvent_(proc: LiveProcess, ev: LiveEvent): void {
    if (proc !== this.proc) {
      return;
    }
    if (ev.type === 'ready') {
      this.lastReady = ev as unknown as Record<string, unknown>;
    }
    const first = ev.type === 'ready' && this.state.phase !== 'running';
    this.setState(reduce(this.state, ev));
    this.evented.fire(ev);
    if (first) {
      this.afterStart(proc);
    }
    if (ev.type === 'accepted') {
      proc.send({ type: 'status' }); // a fresh `ready`: instruments, song blocks, key
    }
    if (ev.type === 'accepted' || ev.type === 'external_change' || ev.type === 'ready') {
      this.historyChanged.fire();
    }
    if (ev.type === 'error' && ev.id === 'output') {
      // The player kept its old output: put the settings back so they say where it plays.
      const back = this.outputBefore;
      this.outputNow = back;
      this.outputBefore = undefined;
      void vscode.window.showErrorMessage(`Engines: ${ev.message}`);
      if (back) {
        const cfg = this.config();
        void this.writeSetting(cfg, 'outputDevice', back.device).then(() => this.writeSetting(cfg, 'routeSources', back.route));
      }
    }
    if (ev.type === 'routing' && ev.stale) {
      // The routing map changed on disk: its new routing plays from the next start.
      void vscode.window.showInformationMessage(`Engines: ${ev.message ?? 'the routing map changed'}`, 'Restart').then((pick) => {
        if (pick === 'Restart') {
          this.proc?.send({ type: 'restart' });
        }
      });
    }
  }

  /** On a (re)started player: catch it up with the buffer (when it plays as you type), and restore
   *  mutes/solos/hold after a crash. */
  private afterStart(proc: LiveProcess): void {
    const doc = this.document();
    if (this.applyOn() === 'save') {
      // It read the saved file, which is all it should hear; unsaved edits go with the next save.
      this.sentVersion = doc && !doc.isDirty ? doc.version : undefined;
    } else if (doc && (doc.isDirty || this.crashed)) {
      this.sentVersion = undefined;
      this.sendBuffer();
    } else {
      this.sentVersion = doc?.version;
    }
    this.tellEditor(true);
    if (this.crashed) {
      for (const id of this.wanted.muted) {
        proc.send({ type: 'mute', instrument: id, on: true });
      }
      for (const id of this.wanted.soloed) {
        proc.send({ type: 'solo', instrument: id, on: true });
      }
      if (this.wanted.optimize) {
        proc.send({ type: 'control', optimize: this.wanted.optimize });
      }
      this.crashed = false;
    }
  }

  /** Another engines-live (a terminal `make live`) already playing this piece? Ask before doubling up. */
  private async checkOtherPlayer(piece: Piece): Promise<boolean> {
    const pid = livePlayer(piece.dir); // (one under another user counts too)
    if (pid === undefined) {
      return true;
    }
    const pick = await vscode.window.showWarningMessage(
      `Another engines-live (pid ${pid}) is already playing ${piece.slug}. Start a second player anyway?`,
      { modal: true },
      'Start anyway',
    );
    return pick === 'Start anyway';
  }

  private setState(s: LiveState): void {
    this.state = s;
    this.state = { ...s, unsaved: this.hasUnsaved() }; // (hasUnsaved reads the new phase)
    this.changed.fire(this.state);
  }

  dispose(): void {
    clearTimeout(this.outputTimer);
    void this.stop();
    for (const s of this.subs) {
      s.dispose();
    }
    this.changed.dispose();
    this.historyChanged.dispose();
    this.evented.dispose();
  }
}
