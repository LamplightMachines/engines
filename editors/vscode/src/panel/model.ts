// The live panel's view model (ADR 0040): everything the webview draws, already decided — it only
// renders (and smooths its meters). Pure — tested without VS Code.

import type { StripActionView } from '../companion';
import { ago, type Revision } from '../history';
import type { LiveState } from '../live/state';
import { chordTones, keyboard, keyboardAria, pretty, type Keyboard } from '../music';
import { outputSummary } from '../live/devices';

/** The identity palette (SPEC §3.5): a 12-step hue wheel walked in steps of 5, so neighbours sit
 *  ~150° apart. Tracks take `PALETTE[i % 12]` in document order; song blocks start half-way round.
 *  Colour follows position — it identifies, it never signals state. The twelve are chosen so a
 *  DAW set built from the piece can wear exactly its strips' colours (ADR 0044 D10). */
export const PALETTE = ['#ff94a6', '#00bfaf', '#b677c6', '#7db04d', '#92a7ff', '#ffa374', '#88c2ba', '#bc7196', '#9bc48d', '#ae98e5', '#bfba69', '#8393cc'];
export const BLOCK_OFFSET = 6;
export const MASTER_COLOR = '#d0d0d0';

export const trackColor = (i: number): string => PALETTE[i % PALETTE.length];
export const blockColor = (i: number): string => PALETTE[(i + BLOCK_OFFSET) % PALETTE.length];

/** A path in the doc, for the jump-to-source links (`engines/pathRange`). */
export type DocPath = (string | number)[];

export type PillKind = 'playing' | 'stopped' | 'ended' | 'starting' | 'restarting' | 'failed' | 'off';

export interface Strip {
  id: string;
  color: string;
  path: DocPath;
  muted: boolean;
  soloed: boolean;
  /** Silent because of its mute or someone else's solo. */
  silenced: boolean;
  /** The level (max of both channels), and each channel — dBFS since the last tick. */
  db: number;
  lDb: number;
  rDb: number;
  /** The buttons companions added, drawn alike. */
  actions: StripActionView[];
}


export interface Segment {
  label: string;
  /** `null`: the tail (drawn grey, outside the cycle). */
  color: string | null;
  weight: number;
  current: boolean;
  /** How far the playhead is through it (0..1; current only). */
  frac: number;
  tail: boolean;
}

export interface BlockRow {
  index: number;
  label: string;
  loops: number;
  color: string;
  path: DocPath;
  /** The song (or the looped block) is in this block, and the player plays. */
  playing: boolean;
  /** The song was started from here (▶). */
  from: boolean;
  /** ⟲ — this block plays as a loop. */
  looping: boolean;
  /** `23 / 58` for the block playing, else its length. */
  count: string;
  /** What the block's `control` says about the Orrery (ADR 0055); `null` ⇒ the session's. */
  orrery: boolean | null;
}

export interface RevisionRow {
  rev: number;
  when: string;
  source: string;
  /** The revision the audio plays (`playing`) or would (`loaded`, stopped). */
  current: 'playing' | 'loaded' | null;
  newest: boolean;
  canRevert: boolean;
}

export interface PanelModel {
  /** Companions' commands placed in the panel: a link each, in the footer. */
  menu: { command: string; title: string }[];
  // Top level (the e2e tests read these).
  phase: LiveState['phase'];
  piece: string | null;
  unsaved: boolean;
  /** A player is up (or coming up): the live sections show; else the "play this piece" state. */
  live: boolean;
  header: {
    piece: string;
    note: string | null;
    /** `r12`, or `r12 → r13` while an accepted revision is on its way in. */
    rev: string;
    pill: { kind: PillKind; text: string; pulse: boolean };
    device: string;
    message: string | null;
  };
  transport: { playing: boolean; hold: boolean; orrery: boolean; limiter: boolean; grDb: number | null; underruns: number };
  refused: { count: number; path: string; message: string } | null;
  loop: { counter: number; posS: number; loopS: number; progress: number; steps: number };
  harmony: {
    root: string | null;
    mode: string | null;
    chord: string | null;
    tones: string[];
    /** Lit segments of 12 (`null`: no tension — the Orrery is off). */
    tensionLit: number | null;
    tension: number | null;
    keyboard: Keyboard | null;
    aria: string;
    /** The collapsed header's line. */
    summary: string;
  };
  instruments: {
    strips: Strip[];
    master: { db: number; lDb: number; rDb: number; peakDb: number | null };
    muted: number;
    soloed: number;
  };
  song: {
    mode: 'loop' | 'block' | 'song';
    summary: string;
    back: { enabled: boolean; title: string };
    aria: string;
    segments: Segment[];
    rows: BlockRow[];
  } | null;
  revisions: { summary: string; rows: RevisionRow[]; more: boolean };
  activity: {
    summary: string;
    warnings: string[];
    items: { when: string; kind: string; text: string }[];
    preview: string | null;
    /** The last preview's stems folder (`null`: none rendered). */
    stems: string | null;
    previewing: boolean;
  };
}

/** −60..0 dBFS → 0..1 (linear in dB; SPEC §3.4). */
export function meterLevel(db: number): number {
  return Math.max(0, Math.min(1, (db + 60) / 60));
}

/** `-7.25` → `−7.3`, silence → `−∞` (the real minus sign; SPEC §3.7). */
export function fmtDb(db: number | null): string {
  if (db === null || db <= -119) {
    return '−∞';
  }
  const t = db.toFixed(1);
  return t === '-0.0' ? '0.0' : t.replace('-', '−');
}

function pill(s: LiveState): PanelModel['header']['pill'] {
  const kind: PillKind = s.phase === 'running' ? (s.playing ? 'playing' : s.finished ? 'ended' : 'stopped') : s.phase;
  const text = { playing: 'Playing', stopped: 'Stopped', ended: 'Ended', starting: 'Starting', restarting: 'Restarting', failed: 'Failed', off: 'Not running' }[kind];
  return { kind, text, pulse: kind === 'playing' };
}

/** `A mixolydian` → `["A", "Mixolydian"]` (the root spelled in the key). */
function scaleChips(s: LiveState): [string | null, string | null] {
  if (!s.key) {
    return [null, null];
  }
  const [, mode = ''] = /^\S+\s+(.*)$/.exec(s.key.name) ?? [];
  const root = s.key.names[((s.key.root % 12) + 12) % 12] ?? s.key.name.split(' ')[0];
  return [pretty(root), mode ? mode[0].toUpperCase() + mode.slice(1) : null];
}

function songModel(s: LiveState): PanelModel['song'] {
  const song = s.song;
  if (!song) {
    return null;
  }
  const pos = s.songPos;
  const inSong = song.mode !== 'loop';
  const current = inSong ? song.current : null;
  const tail = inSong && song.mode === 'song' && !!pos?.tail;
  const within = (loops: number) => {
    const inBlock = pos?.loop_in_block ?? 0;
    const of = pos?.block_loops || loops;
    const t = s.loopS > 0 ? Math.max(0, Math.min(1, s.posS / s.loopS)) : 0;
    return of > 0 ? Math.max(0, Math.min(1, (inBlock + t) / of)) : 0;
  };
  const rows: BlockRow[] = song.blocks.map((b, i) => {
    const here = current === i && !tail;
    return {
      index: i,
      label: b.label,
      loops: b.loops,
      color: blockColor(i),
      path: ['song', 'sequence', i],
      playing: here && s.playing,
      from: song.mode === 'song' && song.from === i,
      looping: song.mode === 'block' && song.block === i,
      count: here && pos ? `${(pos.loop_in_block ?? 0) + 1} / ${b.loops}` : String(b.loops),
      orrery: b.orrery ?? null,
    };
  });
  const segments: Segment[] = song.blocks.map((b, i) => {
    const here = current === i && !tail;
    return { label: b.label, color: blockColor(i), weight: Math.max(1, b.loops), current: here, frac: here ? within(b.loops) : 0, tail: false };
  });
  if (song.tailLoops > 0) {
    segments.push({ label: 'tail', color: null, weight: song.tailLoops, current: tail, frac: tail ? within(song.tailLoops) : 0, tail: true });
  }
  const label = (i: number | null) => (i === null ? null : (song.blocks[i]?.label ?? `block ${i}`));
  const at = tail ? 'the tail' : (label(current) ?? 'the loop');
  const summary = `${at} ${s.playing ? 'playing' : 'stopped'}${song.mode === 'song' && song.from !== null ? ` · from ${label(song.from)}` : ''}`;
  const structure = song.blocks.map((b) => `${b.label} ${b.loops}`).join(', ');
  const where = !inSong
    ? 'The song is not playing (the loop plays).'
    : tail
      ? `Playhead in the tail, loop ${(pos?.loop_in_block ?? 0) + 1} of ${song.tailLoops}.`
      : current !== null
        ? `Playhead in ${label(current)}${pos ? `, loop ${(pos.loop_in_block ?? 0) + 1} of ${song.blocks[current]?.loops}` : ''}.`
        : '';
  return {
    mode: song.mode,
    summary,
    back: {
      enabled: inSong,
      title: inSong ? 'Back to loop: stop the song and play the doc’s own loop again' : 'Back to loop (the loop is already playing)',
    },
    aria: `Song structure: ${structure} loops${song.tailLoops ? `, tail ${song.tailLoops}` : ''}. ${where}`.trim(),
    segments,
    rows,
  };
}

/** The strip buttons companions registered, for strip `id`. */
export type StripActions = (id: string, s: LiveState) => StripActionView[];

export function panelModel(s: LiveState, history: Revision[], now: number = Date.now(), actions: StripActions = () => [], menu: PanelModel['menu'] = []): PanelModel {
  const anySolo = s.soloed.length > 0;
  const strips: Strip[] = s.instruments.map((id, i) => {
    const db = s.meters[id] ?? -120;
    const [lDb, rDb] = s.metersLR[id] ?? [db, db];
    const muted = s.muted.includes(id);
    const soloed = s.soloed.includes(id);
    return { id, color: trackColor(i), path: ['instruments', id], muted, soloed, silenced: muted || (anySolo && !soloed), db, lDb, rDb, actions: actions(id, s) };
  });
  const [root, mode] = scaleChips(s);
  const kb = keyboard(s.key, s.voicing);
  const newest = history.at(-1)?.rev;
  const revRows: RevisionRow[] = history
    .slice(-10)
    .reverse()
    .map((r) => {
      const current = r.rev === s.appliedRev ? (s.playing ? 'playing' : 'loaded') : null;
      return { rev: r.rev, when: ago(r.at, now / 1000), source: r.source, current, newest: r.rev === newest, canRevert: r.rev !== newest && !current };
    });
  const rej = s.rejected;
  const first = rej?.errors[0];
  const warnings = s.warnings.map((w) => `${w.path ? w.path + ': ' : ''}${w.message}`);
  const items = s.activity.slice(0, 12).map((a) => ({ when: ago(a.at / 1000, now / 1000), kind: a.kind, text: a.text }));
  const chord = s.chord;
  return {
    menu,
    phase: s.phase,
    piece: s.piece,
    unsaved: s.unsaved,
    live: s.phase === 'running' || s.phase === 'starting' || s.phase === 'restarting',
    header: {
      piece: s.piece ?? 'No piece playing',
      note: s.note,
      rev: s.rev === 0 ? '' : s.appliedRev && s.appliedRev !== s.rev ? `r${s.appliedRev} → r${s.rev}` : `r${s.rev}`,
      pill: pill(s),
      device: s.phase === 'running' ? outputSummary(s.device, s.routing, s.sampleRate) : '',
      message: s.message,
    },
    transport: {
      playing: s.playing,
      hold: s.optimize === 'hold',
      orrery: s.orrery,
      limiter: s.limiter,
      grDb: s.limiter && s.grDb !== null && s.grDb < -0.05 ? s.grDb : null,
      underruns: s.underruns,
    },
    refused: rej && { count: rej.errors.length, path: first?.path ?? '', message: first?.message ?? '' },
    loop: {
      counter: s.loop,
      posS: s.posS,
      loopS: s.loopS,
      progress: s.loopS > 0 ? Math.max(0, Math.min(1, s.posS / s.loopS)) : 0,
      steps: s.masterSteps,
    },
    harmony: {
      root,
      mode,
      chord,
      tones: chordTones(s.key, s.voicing),
      tension: s.tension,
      tensionLit: s.tension === null ? null : Math.max(0, Math.min(12, Math.round(s.tension * 12))),
      keyboard: kb,
      aria: keyboardAria(kb, s.key, s.voicing),
      summary: chord ?? (s.orrery ? '…' : 'Orrery off'),
    },
    instruments: {
      strips,
      master: { db: s.masterDb, lDb: s.masterLR[0], rDb: s.masterLR[1], peakDb: s.peakDb },
      muted: s.muted.length,
      soloed: s.soloed.length,
    },
    song: songModel(s),
    revisions: {
      summary: s.rev ? `r${s.rev} · ${history.length} revision${history.length === 1 ? '' : 's'}` : 'no revisions yet',
      rows: revRows,
      more: history.length > revRows.length,
    },
    activity: {
      summary: s.previewing ? 'rendering a preview…' : warnings.length ? `${warnings.length} warning${warnings.length === 1 ? '' : 's'}` : (items[0]?.text ?? ''),
      warnings,
      items,
      preview: s.preview?.path ?? null,
      stems: s.preview?.stemsDir ?? null,
      previewing: s.previewing,
    },
  };
}
