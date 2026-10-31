// What the panel, the status bar and the step highlighter show: a pure fold of engines-live's events
// (plus the host's own lifecycle — starting, crashed, failed) into one state object.

import type { Diag, KeyInfo, LiveEvent, RoutingInfo, SongBlockInfo, SongPos, Voicing } from './protocol';

export type Phase = 'off' | 'starting' | 'running' | 'restarting' | 'failed';

export interface Activity {
  /** ms since the epoch */
  at: number;
  kind: 'ok' | 'info' | 'warn' | 'error';
  text: string;
}

/** The doc's song as the player plays it. `block` is what the last `song` command named; `from` the
 *  block a song plays from; `current` the block playing now (a song moves on; ADR 0040). */
export interface SongState {
  blocks: SongBlockInfo[];
  mode: 'loop' | 'block' | 'song';
  block: number | null;
  from: number | null;
  current: number | null;
  tailLoops: number;
}

export interface LiveState {
  phase: Phase;
  /** The piece's slug (set from `starting` on). */
  piece: string | null;
  /** Why it failed / what it's waiting for. */
  message: string | null;
  device: string | null;
  /** `ready.routing`: each source on its own device pair (`--route-sources` / the output picker). */
  routing: RoutingInfo | null;
  /** `ready.capabilities`: what a companion player adds, for its companion. */
  capabilities: string[];
  /** The newest accepted revision, and the one the audio is playing. */
  rev: number;
  appliedRev: number;
  applied: { loop: number; atS: number; kind: string; xfadeS: number } | null;
  /** A session is running (false after `stop` or a song's end). */
  playing: boolean;
  finished: boolean;
  optimize: string;
  orrery: boolean;
  limiter: boolean;
  instruments: string[];
  muted: string[];
  soloed: string[];
  song: SongState | null;
  /** How far into its block the latest loop is (song or block mode). */
  songPos: SongPos | null;
  key: KeyInfo | null;
  /** The doc's `meta.note` (the panel's tooltip on the piece name). */
  note: string | null;
  masterSteps: number;
  loop: number;
  posS: number;
  loopS: number;
  steps: Record<string, number>;
  meters: Record<string, number>;
  /** Per-instrument `[left, right]` dBFS (ADR 0040). */
  metersLR: Record<string, [number, number]>;
  masterDb: number;
  masterLR: [number, number];
  grDb: number | null;
  chord: string | null;
  tension: number | null;
  voicing: Voicing | null;
  peakDb: number | null;
  rejected: { version: number | null; errors: Diag[]; source: string } | null;
  warnings: Diag[];
  underruns: number;
  preview: { path: string; seconds: number; peakDb: number; lufs: number | null; stemsDir: string | null; stems: number } | null;
  /** A preview is rendering (the host asked; its `preview` or `error` hasn't come back yet). */
  previewing: boolean;
  sampleRate: number;
  activity: Activity[];
  /** The host's own fact: the piece's editor holds edits the player hasn't been sent (with
   *  `engines.live.applyOn: "save"`, the unsaved ones — a save plays them). */
  unsaved: boolean;
}

export function initialState(): LiveState {
  return {
    phase: 'off',
    piece: null,
    message: null,
    device: null,
    routing: null,
    capabilities: [],
    rev: 0,
    appliedRev: 0,
    applied: null,
    playing: false,
    finished: false,
    optimize: 'drift',
    orrery: false,
    limiter: true,
    instruments: [],
    muted: [],
    soloed: [],
    song: null,
    songPos: null,
    key: null,
    note: null,
    masterSteps: 0,
    loop: 0,
    posS: 0,
    loopS: 0,
    steps: {},
    meters: {},
    metersLR: {},
    masterDb: -120,
    masterLR: [-120, -120],
    grDb: null,
    chord: null,
    tension: null,
    voicing: null,
    peakDb: null,
    rejected: null,
    warnings: [],
    underruns: 0,
    preview: null,
    previewing: false,
    sampleRate: 48_000,
    activity: [],
    unsaved: false,
  };
}

const MAX_ACTIVITY = 30;

/** Nothing sounding: no chord, tension or voicing to show (the keyboard keeps only the scale). */
const NO_HARMONY = { chord: null, tension: null, voicing: null } as const;

/** Nothing metering: silence on every meter, no song position. */
const SILENT = { steps: {}, meters: {}, metersLR: {}, masterDb: -120, masterLR: [-120, -120] as [number, number], songPos: null };

/** The song as `mode` / `block` put it: `from` is the play-from block, `current` the block playing. */
function songAt(song: SongState, mode: SongState['mode'], block: number | null): SongState {
  return { ...song, mode, block, from: mode === 'song' ? block : null, current: mode === 'loop' ? null : block };
}

function log(s: LiveState, kind: Activity['kind'], text: string, now: number): Activity[] {
  return [{ at: now, kind, text }, ...s.activity].slice(0, MAX_ACTIVITY);
}

function plural(n: number, what: string): string {
  return `${n} ${what}${n === 1 ? '' : 's'}`;
}

/** Fold one protocol event in. Returns a new object (the old one is untouched). */
export function reduce(s: LiveState, ev: LiveEvent, now: number = Date.now()): LiveState {
  switch (ev.type) {
    case 'ready': {
      // A new key (a root/mode edit) makes the last loop's voicing meaningless: it returns with the
      // next loop.
      const newKey = (s.key?.name ?? null) !== (ev.key?.name ?? null);
      return {
        ...s,
        ...(newKey ? NO_HARMONY : {}),
        phase: 'running',
        message: null,
        piece: ev.piece,
        device: ev.device,
        routing: ev.routing ?? null,
        capabilities: ev.capabilities ?? [],
        rev: ev.rev,
        appliedRev: s.appliedRev || ev.rev,
        playing: ev.playing,
        finished: false,
        optimize: ev.optimize,
        orrery: ev.orrery,
        instruments: ev.instruments,
        muted: ev.muted,
        soloed: ev.soloed,
        song: ev.song && {
          blocks: ev.song.blocks,
          mode: ev.song.mode,
          block: ev.song.block,
          from: ev.song.from !== undefined ? ev.song.from : ev.song.mode === 'song' ? ev.song.block : null,
          current: ev.song.current !== undefined ? ev.song.current : ev.song.mode === 'loop' ? null : ev.song.block,
          tailLoops: ev.song.tail_loops ?? 0,
        },
        key: ev.key ?? null,
        note: ev.note ?? null,
        masterSteps: ev.master_steps ?? s.masterSteps,
        limiter: ev.limiter ?? s.limiter,
        loopS: ev.loop_s,
        sampleRate: ev.sample_rate,
        meters: Object.fromEntries(Object.entries(s.meters).filter(([k]) => ev.instruments.includes(k))),
        metersLR: Object.fromEntries(Object.entries(s.metersLR).filter(([k]) => ev.instruments.includes(k))),
        activity: s.phase === 'running' ? s.activity : log(s, 'ok', `playing r${ev.rev}${ev.device ? ` on ${ev.device}` : ' (headless)'}`, now),
      };
    }
    case 'accepted':
      return {
        ...s,
        rev: ev.rev,
        warnings: ev.warnings,
        rejected: null,
        activity: ev.warnings.length ? log(s, 'warn', `r${ev.rev} accepted with ${plural(ev.warnings.length, 'warning')}`, now) : s.activity,
      };
    case 'applied':
      return {
        ...s,
        appliedRev: ev.rev,
        applied: { loop: ev.loop, atS: ev.at_s, kind: ev.kind, xfadeS: ev.xfade_s },
        activity: log(s, 'ok', `r${ev.rev} playing from loop ${ev.loop} at ${ev.at_s.toFixed(2)} s (${ev.kind})`, now),
      };
    case 'unchanged':
      return { ...s, rejected: null };
    case 'rejected': {
      const first = ev.errors[0];
      const what = first ? `: ${first.path ? first.path + ': ' : ''}${first.message}` : '';
      return {
        ...s,
        rejected: { version: ev.version ?? null, errors: ev.errors, source: ev.source },
        activity: log(s, 'error', `${ev.source === 'disk' ? 'saved file' : 'edit'} refused (${plural(ev.errors.length, 'error')})${what} — still playing r${s.appliedRev}`, now),
      };
    }
    case 'external_change':
      return { ...s, activity: log(s, 'info', `r${ev.rev} came from the file on disk`, now) };
    case 'tick': {
      // An older player sends no L/R: both channels show the max.
      const metersLR = ev.meters_lr ?? Object.fromEntries(Object.entries(ev.meters).map(([k, d]) => [k, [d, d] as [number, number]]));
      const songPos = ev.song === undefined ? s.songPos : ev.song;
      return {
        ...s,
        loop: ev.loop,
        posS: ev.pos_s,
        loopS: ev.loop_s,
        steps: ev.steps,
        meters: ev.meters,
        metersLR,
        masterDb: ev.master_db,
        masterLR: ev.master_lr ?? [ev.master_db, ev.master_db],
        grDb: ev.gr_db,
        songPos,
        song: s.song && songPos && s.song.mode !== 'loop' ? { ...s.song, current: songPos.block } : s.song,
      };
    }
    case 'loop':
      return {
        ...s,
        loop: ev.loop,
        optimize: ev.optimize,
        orrery: ev.orrery ?? s.orrery,
        chord: ev.chord ?? null,
        tension: ev.tension ?? null,
        voicing: ev.voicing ?? null,
        peakDb: ev.peak_db,
        song: s.song && ev.song && s.song.mode !== 'loop' ? { ...s.song, current: ev.song.block } : s.song,
      };
    case 'gains':
      return { ...s, muted: ev.muted, soloed: ev.soloed };
    case 'routing':
      // A restart's re-read map (a companion's strip buttons may follow it), or the map went stale on disk.
      return { ...s, routing: ev.routing !== undefined ? ev.routing : s.routing && { ...s.routing, stale: ev.stale } };
    case 'limiter':
      return { ...s, limiter: ev.on };
    case 'control':
      return { ...s, optimize: ev.optimize, orrery: ev.orrery, activity: log(s, 'info', `${ev.optimize}${ev.orrery ? ' · Orrery on' : ''} from the next loop`, now) };
    case 'song':
      return {
        ...s,
        song: s.song ? songAt(s.song, ev.mode, ev.block ?? null) : s.song,
        songPos: ev.mode === 'loop' ? null : s.songPos,
        activity: log(s, 'info', ev.mode === 'loop' ? 'back to the loop' : ev.mode === 'block' ? `looping block ${ev.block}` : `song from block ${ev.block}`, now),
      };
    case 'playing': // a fresh session: its first loop brings the chord
      return {
        ...s,
        ...NO_HARMONY,
        playing: true,
        finished: false,
        appliedRev: s.rev,
        steps: {},
        songPos: null,
        song: s.song ? songAt(s.song, ev.mode === 'block' || ev.mode === 'song' ? ev.mode : 'loop', ev.block) : s.song,
      };
    case 'stopped':
      return { ...s, ...NO_HARMONY, ...SILENT, playing: false };
    case 'finished':
      return { ...s, ...NO_HARMONY, playing: false, finished: true, steps: {}, songPos: null, activity: log(s, 'info', 'the song has ended', now) };
    case 'preview':
      return {
        ...s,
        preview: { path: ev.path, seconds: ev.seconds, peakDb: ev.peak_db, lufs: ev.lufs ?? null, stemsDir: ev.stems?.dir ?? null, stems: ev.stems?.files.length ?? 0 },
        previewing: false,
        activity: log(
          s,
          'ok',
          `preview: ${ev.seconds.toFixed(0)} s, peak ${ev.peak_db.toFixed(1)} dBFS${ev.lufs != null ? `, ${ev.lufs.toFixed(1)} LUFS` : ''}${ev.stems ? ` + ${plural(ev.stems.files.length, 'stem')}` : ''}`,
          now,
        ),
      };
    case 'underrun':
      return { ...s, underruns: ev.count, activity: log(s, 'warn', `audio underrun (${ev.count} so far)`, now) };
    case 'error':
      return { ...s, previewing: s.previewing && !ev.message.startsWith('preview'), activity: log(s, 'error', ev.message, now) };
    default:
      return s;
  }
}

/** The host asked for a preview (the panel shows it rendering until its event comes back). */
export function previewAsked(s: LiveState): LiveState {
  return { ...s, previewing: true };
}

/** The host started (or restarted) a player for `piece`. */
export function starting(s: LiveState, piece: string, now: number = Date.now()): LiveState {
  const fresh = s.piece !== piece;
  return { ...(fresh ? initialState() : s), phase: 'starting', piece, message: null, appliedRev: 0, activity: fresh ? [] : log(s, 'info', 'starting…', now) };
}

/** A crashed player is coming back after the backoff: like `starting`, but the crash reason stays
 *  up (the status bar's tooltip, the panel's message line) until it's `ready` again. */
export function restarting(s: LiveState, piece: string, now: number = Date.now()): LiveState {
  return { ...starting(s, piece, now), phase: 'restarting', message: s.message };
}

/** The player exited: stopped by us (`off`), crashed (`restarting` when a restart is coming), or failed.
 *  A preview it was rendering died with it (no `preview` or `error` will come). */
export function exited(s: LiveState, how: { expected: boolean; restarting: boolean; message?: string }, now: number = Date.now()): LiveState {
  const base = { ...s, ...NO_HARMONY, ...SILENT, playing: false, previewing: false };
  if (how.expected) {
    return { ...base, phase: 'off', message: null };
  }
  if (how.restarting) {
    return { ...base, phase: 'restarting', message: how.message ?? 'the player crashed', activity: log(s, 'error', `the player crashed${how.message ? `: ${how.message}` : ''} — restarting`, now) };
  }
  return { ...base, phase: 'failed', message: how.message ?? 'the player exited', activity: log(s, 'error', how.message ?? 'the player exited', now) };
}
