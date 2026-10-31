// Step highlighting: while engines-live plays, each loop-synced step sequence's current step (from the
// `tick` events) is highlighted where it's written in the doc. Ranges come from `engines-lsp`'s
// `engines/stepRanges` (cached per document version); repaints are throttled to ≤ 15 Hz and happen
// only when a step actually changes.

import * as vscode from 'vscode';
import { Throttle } from './live/debounce';
import type { LiveController } from './live/controller';
import type { Lsp } from './lsp';

type Ranges = Map<string, Map<number, vscode.Range[]>>;

export class StepHighlighter implements vscode.Disposable {
  private readonly deco = vscode.window.createTextEditorDecorationType({
    backgroundColor: new vscode.ThemeColor('engines.activeStepBackground'),
    borderColor: new vscode.ThemeColor('engines.activeStepBorder'),
    borderStyle: 'solid',
    borderWidth: '0 0 0 2px',
    rangeBehavior: vscode.DecorationRangeBehavior.ClosedClosed,
  });
  private cache: { uri: string; version: number; ranges: Ranges } | undefined;
  private lastKey = '';
  private painted = false;
  /** Bumped by every paint: one that finds a newer paint after awaiting the ranges drops out,
   *  so a slow paint can't put back a highlight that a later one (a stop) cleared. */
  private generation = 0;
  private readonly throttle: Throttle<Record<string, number>>;
  private readonly subs: vscode.Disposable[] = [];
  /** The ranges painted last (tests read them). */
  current: vscode.Range[] = [];

  constructor(
    private readonly live: LiveController,
    private readonly lsp: Lsp,
  ) {
    this.throttle = new Throttle((steps) => void this.paint(steps), 1000 / 15);
    this.subs.push(
      live.onDidChange((s) => {
        const key = s.phase === 'running' && s.playing ? JSON.stringify(s.steps) : '';
        if (key !== this.lastKey) {
          this.lastKey = key;
          this.throttle.push(key ? s.steps : {});
        }
      }),
      vscode.window.onDidChangeVisibleTextEditors(() => void this.paint(this.live.state.steps)),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration('engines.live.highlightSteps')) {
          void this.paint(this.live.state.steps);
        }
      }),
    );
  }

  private enabled(): boolean {
    return vscode.workspace.getConfiguration('engines.live').get<boolean>('highlightSteps', true);
  }

  private async ranges(doc: vscode.TextDocument): Promise<Ranges | undefined> {
    const uri = doc.uri.toString();
    if (this.cache?.uri === uri && this.cache.version === doc.version) {
      return this.cache.ranges;
    }
    const version = doc.version;
    const raw = await this.lsp.stepRanges(doc);
    if (!raw) {
      return undefined;
    }
    const ranges: Ranges = new Map();
    for (const [id, steps] of Object.entries(raw)) {
      const byStep = new Map<number, vscode.Range[]>();
      for (const { step, range } of steps) {
        const r = new vscode.Range(range.start.line, range.start.character, range.end.line, range.end.character);
        byStep.set(step, [...(byStep.get(step) ?? []), r]);
      }
      ranges.set(id, byStep);
    }
    this.cache = { uri, version, ranges };
    return ranges;
  }

  private async paint(steps: Record<string, number>): Promise<void> {
    const generation = ++this.generation;
    const piece = this.live.piece;
    const editors = piece ? vscode.window.visibleTextEditors.filter((e) => this.live.isPieceDoc(e.document)) : [];
    const want = this.enabled() && Object.keys(steps).length > 0 && editors.length > 0;
    if (!want) {
      this.clear();
      return;
    }
    const ranges = await this.ranges(editors[0].document);
    if (generation !== this.generation) {
      return; // a newer paint (or a clear) has happened meanwhile
    }
    if (!ranges) {
      this.clear(); // no ranges for this text (the server is restarting): don't leave old ones lit
      return;
    }
    const out: vscode.Range[] = [];
    for (const [id, step] of Object.entries(steps)) {
      out.push(...(ranges.get(id)?.get(step) ?? []));
    }
    for (const e of editors) {
      e.setDecorations(this.deco, out);
    }
    this.painted = true;
    this.current = out;
  }

  private clear(): void {
    if (this.painted) {
      for (const e of vscode.window.visibleTextEditors) {
        e.setDecorations(this.deco, []);
      }
      this.painted = false;
      this.current = [];
    }
  }

  dispose(): void {
    this.throttle.dispose();
    this.deco.dispose();
    for (const s of this.subs) {
      s.dispose();
    }
  }
}
