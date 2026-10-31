// The panel harness's scenarios (ADR 0040): LiveStates built by folding protocol events through the
// real `reduce`, then a `tick` from the clock (position, wobbling meters), turned into the real
// `panelModel`. Modelled on pieces/idle-nap (the doc the design was drawn against).

import type { Revision } from '../src/history';
import type { LiveEvent } from '../src/live/protocol';
import { exited, initialState, reduce, starting, type LiveState } from '../src/live/state';
import { API_MAJOR, Companions } from '../src/companion';
import { panelModel, type PanelModel } from '../src/panel/model';

const A_MIXOLYDIAN = { name: 'A mixolydian', root: 9, scale: [0, 2, 4, 5, 7, 9, 10], degrees: ['1', '2', '3', '4', '5', '6', '♭7'], names: ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'Bb', 'B'] };
const INSTRUMENTS = ['sub', 'crank', 'partials', 'halo', 'piston'];
const LEVELS: Record<string, number> = { sub: -17, crank: -7.2, partials: -4.6, halo: -4.6, piston: -52.9 };
const BLOCKS = [
  { label: 'settle', loops: 33 },
  { label: 'idle', loops: 58 },
  { label: 'deep', loops: 115, orrery: false },
  { label: 'surface', loops: 41 },
];
const LOOP_S = 14.545;

const ready = (over: Partial<Extract<LiveEvent, { type: 'ready' }>> = {}): LiveEvent => ({
  type: 'ready',
  protocol: 1,
  piece: 'idle-nap',
  rev: 78,
  sample_rate: 48000,
  loop_s: LOOP_S,
  master_steps: 16,
  device: 'MacBook Pro Speakers',
  limiter: true,
  playing: true,
  optimize: 'drift',
  orrery: true,
  instruments: INSTRUMENTS,
  muted: [],
  soloed: [],
  note: 'Idle — a nap: a 1 h ADR 0029 song (settle → idle → deep → surface) over the Idle A2 harmonic-series engine drone.',
  song: { blocks: BLOCKS, mode: 'loop', block: null, from: null, current: null, tail_loops: 1 },
  key: A_MIXOLYDIAN,
  ...over,
});

/** Gmaj7 over A mixolydian: G B D F♯, the pad G4 B4 D5 F♯5, the bass G3. */
const loopEv = (loop: number, song: { block: number; loop_in_block: number; block_loops: number } | null, orrery = true, optimize = 'drift'): LiveEvent => ({
  type: 'loop',
  loop,
  rev: 78,
  optimize,
  chord: orrery ? 'Gmaj7' : null,
  tension: orrery ? 0.3 : null,
  voicing: orrery ? { pad: [-2, 2, 5, 9], bass: -14, tones: [-2, 2, 5, 9] } : null,
  peak_db: -8.8,
  song,
});

function tick(s: LiveState, tMs: number, song: { block: number; loop_in_block: number; block_loops: number; tail?: boolean } | null, base = 2905): LiveState {
  const secs = tMs / 1000;
  const loops = Math.floor(secs / LOOP_S);
  const pos = secs % LOOP_S;
  const ph = secs * 10;
  const wob = (i: number) => Math.sin(ph * 0.61 + i * 1.7) * 1.1 + Math.sin(ph * 0.19 + i * 0.9) * 0.7;
  const meters_lr: Record<string, [number, number]> = {};
  const meters: Record<string, number> = {};
  INSTRUMENTS.forEach((id, i) => {
    const off = s.muted.includes(id) || (s.soloed.length > 0 && !s.soloed.includes(id));
    const l = off ? -120 : LEVELS[id] + wob(i);
    const r = off ? -120 : LEVELS[id] + wob(i) - 0.8;
    meters_lr[id] = [l, r];
    meters[id] = Math.max(l, r);
  });
  const mw = Math.sin(ph * 0.5) * 0.9 + Math.sin(ph * 0.17) * 0.6;
  const songPos = song && { ...song, loop_in_block: (song.loop_in_block + loops) % song.block_loops };
  return reduce(s, {
    type: 'tick',
    loop: base + loops,
    pos_s: pos,
    loop_s: LOOP_S,
    steps: {},
    meters,
    meters_lr,
    master_db: -10.8 + mw,
    master_lr: [-10.8 + mw, -11.4 + mw * 0.9],
    gr_db: s.limiter ? -0.4 : null,
    song: songPos,
  });
}

function history(n: number, last = 78, now = Date.now() / 1000): Revision[] {
  return [...Array(n)].map((_, i) => {
    const rev = last - n + 1 + i;
    return { rev, hash: '', file: '', at: now - 8 * 3600 + i * 60, source: i === n - 2 ? 'revert' : i % 3 === 0 ? 'disk' : 'editor' };
  });
}

type Built = { s: LiveState; hist: Revision[] };

const SCENARIOS: Record<string, (t: number) => Built> = {
  /** The mockup: the song playing in idle (started from settle), drift + Orrery + limiter. */
  'song-playing': (t) => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready());
    s = reduce(s, { type: 'song', mode: 'song', block: 0 });
    s = reduce(s, loopEv(2933, { block: 1, loop_in_block: 22, block_loops: 58 }));
    return { s: tick(s, t, { block: 1, loop_in_block: 22, block_loops: 58 }), hist: history(9) };
  },
  /** The doc's own loop (the song not playing). */
  loop: (t) => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready());
    s = reduce(s, loopEv(2933, null));
    return { s: tick(s, t, null), hist: history(9) };
  },
  /** ⟲ deep: one block as a loop. */
  'block-looping': (t) => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready());
    s = reduce(s, { type: 'song', mode: 'block', block: 2 });
    s = reduce(s, loopEv(2933, { block: 2, loop_in_block: 3, block_loops: 115 }));
    return { s: tick(s, t, { block: 2, loop_in_block: 3, block_loops: 115 }), hist: history(9) };
  },
  /** In the song's release tail. */
  tail: (t) => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready());
    s = reduce(s, { type: 'song', mode: 'song', block: 3 });
    s = reduce(s, loopEv(3150, { block: 3, loop_in_block: 0, block_loops: 1 }));
    return { s: tick(s, t % (LOOP_S * 1000), { block: 3, loop_in_block: 0, block_loops: 1, tail: true }), hist: history(9) };
  },
  /** Mute + solo, hold, the limiter off, no Orrery. */
  'solo-mute-hold': (t) => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready({ optimize: 'hold', orrery: false, limiter: false }));
    s = reduce(s, { type: 'gains', muted: ['crank'], soloed: ['halo', 'sub'] });
    s = reduce(s, loopEv(2933, null, false, 'hold'));
    return { s: tick(s, t, null), hist: history(9) };
  },
  /** ADR 0044: each source on its own pair of a 64-channel loopback. */
  routed: (t) => {
    const pair = (i: number, id: string) => ({ pair: `${2 * i + 1}/${2 * i + 2}`, source: `inst:${id}`, name: id });
    const routing = {
      channels: 64,
      map: 'pieces/idle-nap/ableton/idle-nap.routing.json',
      pairs: [pair(0, 'sub'), pair(1, 'crank'), pair(2, 'partials'), pair(3, 'halo'), pair(4, 'piston')],
      overflow: '11/12',
      unrouted: [],
      stale: false,
    };
    let s = reduce(starting(initialState(), 'idle-nap'), ready({ device: 'BlackHole 64ch', routing }));
    s = reduce(s, loopEv(2933, null));
    return { s: tick(s, t, null), hist: history(9) };
  },
  /** A companion's button on every strip and a footer command: what an extension built on the API adds. */
  companion: (t) => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready());
    s = reduce(s, loopEv(2933, null));
    return { s: tick(s, t, null), hist: history(9) };
  },
  /** A refused edit while the buffer is unsaved; warnings; an underrun. */
  'refused-unsaved': (t) => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready());
    s = reduce(s, { type: 'accepted', version: 3, rev: 79, warnings: [{ severity: 'warning', path: 'safe_bounds.instruments.halo.params.level_db', code: 'range', message: 'authored -13.98, outside [-26, -17.08] — it plays clamped to -17.08' }] });
    s = reduce(s, { type: 'rejected', version: 4, errors: [{ severity: 'error', path: 'instruments.halo.params.cutof_hz', code: 'unknown-key', message: 'unknown key `cutof_hz` — did you mean `cutoff_hz`?' }], source: 'command' });
    s = reduce(s, { type: 'underrun', count: 2 });
    s = reduce(s, loopEv(2933, null));
    return { s: { ...tick(s, t, null), unsaved: true }, hist: history(10, 79) };
  },
  /** 30 revisions (the "All…" link). */
  'many-revisions': (t) => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready());
    s = reduce(s, loopEv(2933, null));
    return { s: tick(s, t, null), hist: history(30) };
  },
  stopped: () => {
    let s = reduce(starting(initialState(), 'idle-nap'), ready());
    s = reduce(s, loopEv(2933, null));
    return { s: reduce(s, { type: 'stopped' }), hist: history(9) };
  },
  starting: () => ({ s: starting(initialState(), 'idle-nap'), hist: [] }),
  failed: () => ({ s: exited(starting(initialState(), 'idle-nap'), { expected: false, restarting: false, message: 'pieces/idle-nap/idle-nap.json5 has errors — fix them first:\n  instruments.halo.params.cutof_hz: unknown key' }), hist: [] }),
  off: () => ({ s: initialState(), hist: [] }),
};

export const NAMES = Object.keys(SCENARIOS);

export function scenario(name: string, tMs: number): PanelModel | undefined {
  const f = SCENARIOS[name];
  if (!f) {
    return undefined;
  }
  const { s, hist } = f(tMs);
  // In the `companion` scenario, a fake companion's button and command.
  const companions = new Companions({ piece: () => undefined, player: () => ({ state: s, ready: undefined, status: undefined }), binaryChanged: () => undefined });
  const api = companions.connect('Harness', API_MAJOR);
  if (name === 'companion') {
    api.registerStripAction({ id: 'fake', icon: 'M5 12h14 M12 5v14', state: (x) => ({ on: x.id === 'halo', blocked: x.id === 'piston', title: `A fake companion's button on ${x.id}` }), run: () => undefined });
    api.registerMenuItem({ command: 'fake.build', title: 'fake build…', where: 'panel' });
  }
  return panelModel(s, hist, Date.now(), (id, st) => companions.stripActions(id, st), companions.menu('panel'));
}
