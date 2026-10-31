// Jump-to-source (ADR 0040): a name in the live panel (an instrument, `harmony`, `global`, a song
// block) moves the editor to where its path is written — the key a few lines from the top, the
// cursor on it, the editor focused, and the line flashing the panel's accent, fading out.

import * as vscode from 'vscode';
import { flashFrames, pathText, topLine } from './jumpView';
import type { Lsp } from './lsp';

const FLASH_MS = 1600;

export class Jumper implements vscode.Disposable {
  private readonly frames = flashFrames(8, FLASH_MS);
  private readonly types = this.frames.map((f) => vscode.window.createTextEditorDecorationType({ isWholeLine: true, backgroundColor: `rgba(255, 163, 58, ${f.alpha})` }));
  private timers: ReturnType<typeof setTimeout>[] = [];
  private flashed: vscode.TextEditor | undefined;

  constructor(private readonly lsp: Lsp) {}

  /** Reveal `path` in the doc at `docPath` (its open editor, else column one). `false` when the
   *  language server couldn't place it. */
  async reveal(docPath: string, path: (string | number)[], label?: string): Promise<boolean> {
    const uri = vscode.Uri.file(docPath);
    const open = vscode.window.visibleTextEditors.find((e) => e.document.uri.fsPath === uri.fsPath);
    const doc = open?.document ?? (await vscode.workspace.openTextDocument(uri));
    const editor = await vscode.window.showTextDocument(doc, { viewColumn: open?.viewColumn ?? vscode.ViewColumn.One, preserveFocus: false, preview: false });
    const r = await this.lsp.pathRange(editor.document, path, label);
    if (!r) {
      void vscode.window.setStatusBarMessage(`Engines: couldn't find ${pathText(path)} — is the language server running?`, 4000);
      return false;
    }
    const key = new vscode.Position(r.keyRange.start.line, r.keyRange.start.character);
    editor.selection = new vscode.Selection(key, key);
    const top = topLine(key.line);
    editor.revealRange(new vscode.Range(top, 0, top, 0), vscode.TextEditorRevealType.AtTop);
    this.flash(editor, key.line);
    if (!r.complete) {
      void vscode.window.setStatusBarMessage(`Engines: ${pathText(path)} isn't in the doc as written — showing the nearest part of it`, 4000);
    }
    return true;
  }

  private flash(editor: vscode.TextEditor, line: number): void {
    this.clear();
    if (vscode.workspace.getConfiguration('workbench').get<string>('reduceMotion') === 'on') {
      return;
    }
    this.flashed = editor;
    const at = [new vscode.Range(line, 0, line, 0)];
    this.frames.forEach((f, i) => {
      this.timers.push(
        setTimeout(() => {
          if (i > 0) {
            editor.setDecorations(this.types[i - 1], []);
          }
          editor.setDecorations(this.types[i], at);
        }, f.at),
      );
    });
    this.timers.push(setTimeout(() => this.clear(), FLASH_MS));
  }

  /** Stop a flash in progress (a repeat click restarts it). */
  private clear(): void {
    for (const t of this.timers) {
      clearTimeout(t);
    }
    this.timers = [];
    for (const t of this.types) {
      this.flashed?.setDecorations(t, []);
    }
    this.flashed = undefined;
  }

  dispose(): void {
    this.clear();
    for (const t of this.types) {
      t.dispose();
    }
  }
}
