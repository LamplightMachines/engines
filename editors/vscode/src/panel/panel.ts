// The live panel (ADR 0038, redesigned in ADR 0040): a webview beside the doc that shows what
// engines-live is doing (transport, hold ⇄ drift, the Orrery's chord on a keyboard, mixer strips with
// stereo meters, song blocks, revisions, activity) and sends the clicks back as protocol commands —
// or, for a name in the panel, as a jump to where the doc writes it. The webview only renders
// `PanelModel`s; it builds its own DOM (media/panel.js) on the design tokens (media/tokens.css).

import type { Companions } from '../companion';
import * as crypto from 'node:crypto';
import * as vscode from 'vscode';
import { readHistory, type Revision } from '../history';
import { Throttle } from '../live/debounce';
import type { LiveController } from '../live/controller';
import { panelModel } from './model';

/** What a click in the webview asks for: a command, a jump to a path in the doc, or (a section
 *  opened / closed) the sections' state to keep. */
export type PanelMessage =
  | { type: 'cmd'; name: string; arg?: string | number | boolean; action?: string }
  | { type: 'reveal'; path: (string | number)[]; label?: string }
  | { type: 'sections'; open: Record<string, boolean> }
  | { type: 'ready' };

const SECTIONS_KEY = 'engines.live.panel.sections';

export class LivePanel implements vscode.Disposable {
  static current: LivePanel | undefined;
  /** Where the sections' open/closed state outlives the panel (the extension's global state). */
  static memento: vscode.Memento | undefined;
  /** Companions' strip buttons. */
  static companions: Companions | undefined;

  /** Show the panel (beside the doc); `focus` moves the keyboard focus into it (the status bar). */
  static show(extensionUri: vscode.Uri, live: LiveController, opts: { focus?: boolean } = {}): LivePanel {
    if (LivePanel.current) {
      LivePanel.current.panel.reveal(undefined, !opts.focus);
      return LivePanel.current;
    }
    LivePanel.current = new LivePanel(extensionUri, live, !opts.focus);
    return LivePanel.current;
  }

  readonly panel: vscode.WebviewPanel;
  private history: Revision[] = [];
  private readonly throttle: Throttle<void>;
  private readonly subs: vscode.Disposable[] = [];
  /** The last model posted (tests read it). */
  lastModel: ReturnType<typeof panelModel> | undefined;

  private constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly live: LiveController,
    preserveFocus: boolean,
  ) {
    const media = vscode.Uri.joinPath(extensionUri, 'media');
    this.panel = vscode.window.createWebviewPanel(
      'engines.live',
      'Engines Live',
      { viewColumn: vscode.ViewColumn.Beside, preserveFocus },
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [media] },
    );
    this.panel.webview.html = this.html();
    this.throttle = new Throttle(() => this.post(), 1000 / 15);
    this.loadHistory();
    this.subs.push(
      live.onDidChange(() => this.throttle.push()),
      LivePanel.companions?.subscribe(() => this.throttle.push()) ?? { dispose: () => undefined },
      live.onDidChangeHistory(() => {
        this.loadHistory();
        this.throttle.push();
      }),
      this.panel.onDidChangeViewState(() => this.post()),
      this.panel.webview.onDidReceiveMessage((m: PanelMessage) => {
        if (m.type === 'ready') {
          const reduceMotion = vscode.workspace.getConfiguration('workbench').get<string>('reduceMotion') === 'on';
          void this.panel.webview.postMessage({ type: 'init', open: LivePanel.memento?.get(SECTIONS_KEY), reduceMotion });
          this.post();
        } else if (m.type === 'cmd') {
          void this.handle(m);
        } else if (m.type === 'reveal' && Array.isArray(m.path) && m.path.every((p) => typeof p === 'string' || typeof p === 'number')) {
          void vscode.commands.executeCommand('engines.live.reveal', m.path, typeof m.label === 'string' ? m.label : undefined);
        } else if (m.type === 'sections' && m.open && typeof m.open === 'object') {
          void LivePanel.memento?.update(SECTIONS_KEY, m.open);
        }
      }),
      this.panel.onDidDispose(() => this.dispose()),
    );
  }

  private loadHistory(): void {
    this.history = this.live.piece ? readHistory(this.live.piece.dir) : [];
  }

  private post(): void {
    const s = this.live.state;
    this.panel.title = s.piece ? `Engines Live · ${s.piece}` : 'Engines Live';
    const c = LivePanel.companions;
    const model = panelModel(s, this.history, Date.now(), (id, st) => c?.stripActions(id, st) ?? [], (c?.menu('panel') ?? []).map(({ command, title }) => ({ command, title })));
    this.lastModel = model;
    if (this.panel.visible) {
      void this.panel.webview.postMessage({ type: 'model', model });
    }
  }

  /** The playing piece's doc (whose history the panel lists). */
  private pieceDoc(): vscode.Uri | undefined {
    return this.live.piece && vscode.Uri.file(this.live.piece.doc);
  }

  private async handle(m: Extract<PanelMessage, { type: 'cmd' }>): Promise<void> {
    const live = this.live;
    const id = String(m.arg ?? '');
    const n = Number(m.arg);
    switch (m.name) {
      case 'start':
        return void vscode.commands.executeCommand('engines.live.start');
      case 'quit':
        return live.stop();
      case 'play':
        return live.play();
      case 'pause':
        return live.pause();
      case 'restart':
        return live.restart();
      case 'hold':
        return live.setOptimize('hold');
      case 'drift':
        return live.setOptimize('drift');
      case 'orrery':
        return live.setOrrery(m.arg === true);
      case 'limiter':
        return live.setLimiter(m.arg === true);
      case 'mute':
        return live.toggleMute(id);
      case 'solo':
        return live.toggleSolo(id);
      case 'clearSolo':
        return live.clearSolo();
      case 'menu':
        // A companion's command placed in the panel: only one it registered runs.
        if (LivePanel.companions?.menu('panel').some((i) => i.command === id)) {
          void vscode.commands.executeCommand(id);
        }
        return;
      case 'stripAction':
        // A companion's strip button: `action` on strip `arg`.
        LivePanel.companions?.runStripAction(String(m.action ?? ''), id, live.state);
        return;
      case 'loopBlock':
        return live.loopBlock(n);
      case 'playFrom':
        return live.playFrom(n);
      case 'songStop':
        return live.songStop();
      case 'chooseOutput':
        return live.chooseOutput();
      case 'preview':
        return void vscode.commands.executeCommand('engines.live.preview');
      case 'revealStems':
        return void vscode.commands.executeCommand('engines.live.revealStems');
      case 'openPreview': {
        // Only the preview the player rendered — never a path taken from the webview's message.
        const file = live.state.preview?.path;
        return void (file && vscode.env.openExternal(vscode.Uri.file(file)));
      }
      // The revisions listed are the playing piece's history: act on that piece, not on whichever
      // piece doc is in front.
      case 'revert':
        return void vscode.commands.executeCommand('engines.live.revert', n, this.pieceDoc());
      case 'revisions':
        return void vscode.commands.executeCommand('engines.live.revert', undefined, this.pieceDoc());
      case 'diff':
        return void vscode.commands.executeCommand('engines.live.diff', n, this.pieceDoc());
      case 'output':
        return void vscode.commands.executeCommand('engines.live.showOutput');
    }
  }

  private html(): string {
    const w = this.panel.webview;
    const media = vscode.Uri.joinPath(this.extensionUri, 'media');
    const css = (name: string) => w.asWebviewUri(vscode.Uri.joinPath(media, name));
    const js = w.asWebviewUri(vscode.Uri.joinPath(media, 'panel.js'));
    const nonce = crypto.randomBytes(16).toString('base64');
    return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${w.cspSource}; script-src 'nonce-${nonce}';">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="stylesheet" href="${css('tokens.css')}">
<link rel="stylesheet" href="${css('panel.css')}">
<title>Engines Live</title>
</head>
<body>
<div id="app"></div>
<script nonce="${nonce}" src="${js}"></script>
</body>
</html>`;
  }

  dispose(): void {
    if (LivePanel.current === this) {
      LivePanel.current = undefined;
    }
    this.throttle.dispose();
    for (const s of this.subs) {
      s.dispose();
    }
    this.panel.dispose();
  }
}
