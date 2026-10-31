// The status bar item (see `statusView`). Click → the live panel (focused), or play when nothing plays.

import * as vscode from 'vscode';
import type { Companions } from './companion';
import type { LiveController } from './live/controller';
import { pieceOfDoc } from './piece';
import { statusView } from './statusView';

export class StatusBar implements vscode.Disposable {
  private readonly item = vscode.window.createStatusBarItem('engines.live', vscode.StatusBarAlignment.Left, 50);
  private last = '';
  private readonly subs: vscode.Disposable[] = [];

  constructor(
    private readonly live: LiveController,
    private readonly companions?: Companions,
  ) {
    this.item.name = 'Engines Live';
    this.subs.push(
      live.onDidChange(() => this.update()),
      vscode.window.onDidChangeActiveTextEditor(() => this.update()),
      companions?.subscribe(() => this.update()) ?? { dispose: () => undefined },
    );
    this.update();
  }

  private update(): void {
    const ed = vscode.window.activeTextEditor;
    const active = ed?.document.uri.scheme === 'file' ? pieceOfDoc(ed.document.uri.fsPath)?.slug : undefined;
    const v = statusView(this.live.state, active);
    const items = this.companions?.menu('statusBar') ?? [];
    const key = JSON.stringify([v ?? null, items.map((i) => [i.command, i.title])]);
    if (key === this.last) {
      return;
    }
    this.last = key;
    void vscode.commands.executeCommand('setContext', 'engines.live.playing', this.live.state.phase !== 'off' && this.live.state.phase !== 'failed');
    if (!v) {
      this.item.hide();
      return;
    }
    this.item.text = v.text;
    // Companions' commands placed here: links under the tooltip, only those commands enabled.
    if (items.length) {
      const md = new vscode.MarkdownString(`${v.tooltip}\n\n${items.map((i) => `[${i.title.replace(/[[\]]/g, '')}](command:${i.command})`).join(' · ')}`);
      md.isTrusted = { enabledCommands: items.map((i) => i.command) };
      this.item.tooltip = md;
    } else {
      this.item.tooltip = v.tooltip;
    }
    this.item.command = v.command === 'engines.live.showPanel' ? { command: v.command, title: 'Show the live panel', arguments: [{ focus: true }] } : v.command;
    // Only the error / warning backgrounds are honoured on a status item: playing is a text colour.
    this.item.backgroundColor = v.tone === 'normal' ? undefined : new vscode.ThemeColor(v.tone === 'error' ? 'statusBarItem.errorBackground' : 'statusBarItem.warningBackground');
    this.item.color = v.playing && v.tone === 'normal' ? new vscode.ThemeColor('engines.statusBar.playingForeground') : undefined;
    this.item.show();
  }

  dispose(): void {
    this.item.dispose();
    for (const s of this.subs) {
      s.dispose();
    }
  }
}
