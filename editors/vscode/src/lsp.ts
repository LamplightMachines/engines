// The language client: starts `engines-lsp` over stdio for `engines-loop` documents (ADR 0037). Inlay-hint
// settings go in `initializationOptions` and follow `engines.*` changes; the CodeLens titles show the
// player's live mute/solo/audition state; `engines/pathRange` resolves the panel's jump-to-source.

import * as vscode from 'vscode';
import { LanguageClient, type LanguageClientOptions, type ServerOptions, TransportKind } from 'vscode-languageclient/node';
import { offerBuild, resolveTool } from './binaries';
import type { LiveState } from './live/state';
import { lensView } from './lensView';

export interface LspRange {
  start: { line: number; character: number };
  end: { line: number; character: number };
}

export interface StepRange {
  step: number;
  range: LspRange;
}

/** Where a JSON path is written: the whole member (`range`), its key (`keyRange`), and whether the
 *  whole path matched (else these are its deepest prefix's). */
export interface PathRange {
  range: LspRange;
  keyRange: LspRange;
  complete: boolean;
}

export class Lsp implements vscode.Disposable {
  client: LanguageClient | undefined;
  private live: () => LiveState;

  constructor(
    private readonly root: string | undefined,
    private readonly output: vscode.LogOutputChannel,
    live: () => LiveState,
  ) {
    this.live = live;
  }

  async start(): Promise<void> {
    const bin = resolveTool('engines-lsp', this.root);
    if (!bin) {
      if (await offerBuild('engines-lsp', this.root)) {
        return this.start();
      }
      return;
    }
    const serverOptions: ServerOptions = { command: bin, args: [], transport: TransportKind.stdio, options: { cwd: this.root } };
    const clientOptions: LanguageClientOptions = {
      documentSelector: [
        { language: 'engines-loop', scheme: 'file' },
        { language: 'engines-loop', scheme: 'untitled' },
      ],
      initializationOptions: { inlayHints: vscode.workspace.getConfiguration('engines').get('inlayHints') },
      synchronize: { configurationSection: 'engines' },
      outputChannel: this.output,
      middleware: {
        provideCodeLenses: async (doc, token, next) => {
          const lenses = await next(doc, token);
          return lenses?.map((l) => this.decorate(l));
        },
      },
    };
    this.client = new LanguageClient('engines', 'Engines', serverOptions, clientOptions);
    await this.client.start();
  }

  async restart(): Promise<void> {
    await this.stop();
    await this.start();
  }

  async stop(): Promise<void> {
    const c = this.client;
    this.client = undefined;
    if (c) {
      await c.stop().catch(() => undefined);
    }
  }

  /** `{ modulator: [{ step, range }] }` for `doc` (the custom `engines/stepRanges` request). */
  async stepRanges(doc: vscode.TextDocument): Promise<Record<string, StepRange[]> | undefined> {
    const c = this.client;
    if (!c?.isRunning()) {
      return undefined;
    }
    try {
      return await c.sendRequest<Record<string, StepRange[]>>('engines/stepRanges', { textDocument: { uri: doc.uri.toString() } });
    } catch {
      return undefined;
    }
  }

  /** Ask VS Code to re-query the CodeLenses (after mute/solo/song changes). */
  refreshLenses(): void {
    const feature = this.client?.getFeature('textDocument/codeLens');
    for (const doc of vscode.workspace.textDocuments) {
      if (doc.languageId === 'engines-loop') {
        feature?.getProvider(doc)?.onDidChangeCodeLensEmitter.fire();
      }
    }
  }

  /** Lens titles that show what's live (icon-led, with a tooltip — `lensView`). */
  private decorate(lens: vscode.CodeLens): vscode.CodeLens {
    const cmd = lens.command;
    const v = cmd && lensView(cmd.command, cmd.title, cmd.arguments?.[1], this.live());
    return cmd && v ? new vscode.CodeLens(lens.range, { ...cmd, title: v.title, tooltip: v.tooltip }) : lens;
  }

  /** `text` through `engines-migrate`'s conversion (the custom `engines/migrate` request; ADR 0054 U6), or why
   *  it couldn't be. */
  async migrate(text: string): Promise<{ text: string; changed: boolean; notes: string[] } | string> {
    const c = this.client;
    if (!c?.isRunning()) {
      return "engines-lsp isn't running (Engines: Restart the Language Server)";
    }
    try {
      return await c.sendRequest<{ text: string; changed: boolean; notes: string[] }>('engines/migrate', { text });
    } catch (e) {
      return e instanceof Error ? e.message : String(e);
    }
  }

  /** Where `path` is written in `doc` (the custom `engines/pathRange` request; ADR 0040). */
  async pathRange(doc: vscode.TextDocument, path: (string | number)[], label?: string): Promise<PathRange | null | undefined> {
    const c = this.client;
    if (!c?.isRunning()) {
      return undefined;
    }
    try {
      return await c.sendRequest<PathRange | null>('engines/pathRange', { textDocument: { uri: doc.uri.toString() }, path, label });
    } catch {
      return undefined;
    }
  }

  dispose(): void {
    void this.stop();
  }
}
