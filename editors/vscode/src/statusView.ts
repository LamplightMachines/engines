// What the status bar item shows: the player's piece · revision · hold/drift · chord, or a play
// button when a piece doc is in front and nothing plays. Pure (tested without VS Code).

import type { LiveState } from './live/state';

export interface StatusView {
  text: string;
  tooltip: string;
  command: string;
  tone: 'normal' | 'warning' | 'error';
  /** Playing: the text takes the "live" green (`engines.statusBar.playingForeground`; ADR 0040). */
  playing?: boolean;
}

/** What the item shows for `s` (`activePiece`: the slug of the piece doc in front, if any). Pure. */
export function statusView(s: LiveState, activePiece: string | undefined): StatusView | undefined {
  const slug = s.piece;
  switch (s.phase) {
    case 'off':
      return activePiece ? { text: `$(play) ${activePiece}`, tooltip: `Play ${activePiece} live (engines-live)`, command: 'engines.live.start', tone: 'normal' } : undefined;
    case 'starting':
      return { text: `$(sync~spin) ${slug}`, tooltip: 'Starting engines-live…', command: 'engines.live.showPanel', tone: 'normal' };
    case 'restarting':
      return { text: `$(sync~spin) ${slug} restarting`, tooltip: s.message ?? 'engines-live crashed — restarting', command: 'engines.live.showPanel', tone: 'warning' };
    case 'failed':
      return { text: `$(error) ${slug}`, tooltip: `engines-live: ${s.message ?? 'failed'} (tries again on save)`, command: 'engines.live.showPanel', tone: 'error' };
    case 'running': {
      if (!s.playing) {
        return { text: `$(debug-pause) ${slug}${s.finished ? ' · ended' : ' · stopped'}`, tooltip: 'engines-live is open but not playing — click for the panel', command: 'engines.live.showPanel', tone: 'normal' };
      }
      const rev = s.appliedRev && s.appliedRev !== s.rev ? `r${s.appliedRev}→${s.rev}` : `r${s.rev}`;
      const at = s.song?.current ?? s.song?.block ?? 0;
      const song = s.song && s.song.mode !== 'loop' ? ` · ${s.song.mode === 'block' ? '⟲' : '♫'} ${s.song.blocks[at]?.label ?? at}` : '';
      const chord = s.chord ? ` · ${s.chord}` : '';
      const unsaved = s.unsaved ? ' · ● unsaved' : '';
      const text = `$(play) ${slug} ${rev} · ${s.optimize}${song}${chord}${unsaved}`;
      if (s.rejected) {
        return { text: `$(warning) ${slug} ${rev} · edit refused${unsaved}`, tooltip: `Still playing r${s.appliedRev}: ${s.rejected.errors[0]?.message ?? ''}`, command: 'engines.live.showPanel', tone: 'warning' };
      }
      const hint = s.unsaved ? ' Unsaved edits aren\'t playing yet — save (⌘S) to hear them.' : '';
      return { text, tooltip: `Playing ${slug} — loop ${s.loop}, ${s.device ?? 'headless'}.${hint} Click for the panel.`, command: 'engines.live.showPanel', tone: 'normal', playing: true };
    }
  }
}
