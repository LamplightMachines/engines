// The CodeLens titles over the doc (ADR 0040): icon-led, showing the player's live mute / solo /
// audition state, with a tooltip that says what a click does. The server sends plain titles
// (`solo`, `mute`, `▶ loop block 1 · deep · 4 loops`, `play from here`); this rewrites them. Pure.

import type { LiveState } from './live/state';

export interface LensView {
  title: string;
  tooltip: string;
}

/** `▶ loop block 1 · deep · 4 loops` → the label and loop count the server wrote. */
function parseBlock(title: string): { label?: string; loops?: number } {
  const m = /^▶ loop block \d+(?: · (.+?))?(?: · (\d+) loops?)?$/.exec(title);
  return { label: m?.[1], loops: m?.[2] ? Number(m[2]) : undefined };
}

/** The title + tooltip for one of the server's lenses, given what's playing (`undefined`: not ours). */
export function lensView(command: string, title: string, arg: unknown, s: LiveState): LensView | undefined {
  const running = s.phase === 'running';
  const id = String(arg);
  switch (command) {
    case 'engines.solo': {
      const on = running && s.soloed.includes(id);
      return { title: on ? '$(circle-large-filled) solo' : '$(circle-large-outline) solo', tooltip: on ? `Unsolo ${id}` : `Solo ${id}` };
    }
    case 'engines.mute': {
      const on = running && s.muted.includes(id);
      return { title: on ? '$(mute) muted' : '$(unmute)', tooltip: on ? `Unmute ${id}` : `Mute ${id}` };
    }
    case 'engines.loopBlock':
    case 'engines.playFrom': {
      const k = Number(arg);
      const parsed = parseBlock(title);
      const block = s.song?.blocks[k];
      const label = block?.label ?? parsed.label ?? `block ${k}`;
      const loops = block?.loops ?? parsed.loops;
      if (command === 'engines.loopBlock') {
        const on = running && s.song?.mode === 'block' && s.song.block === k;
        return on
          ? { title: `$(debug-stop) looping ${label}`, tooltip: `Stop looping ${label} — back to the loop` }
          : { title: `$(sync) loop · ${label}${loops !== undefined ? ` · ${loops} loops` : ''}`, tooltip: `Loop block ${k} (${label}) while you edit` };
      }
      const on = running && s.song?.mode === 'song' && s.song.from === k;
      return on
        ? { title: `$(debug-stop) playing from ${label}`, tooltip: `Stop the song — back to the loop` }
        : { title: `$(play) play from ${label}`, tooltip: `Play the song from ${label} (edits wait until it stops)` };
    }
    default:
      return undefined;
  }
}
