// The engines-live protocol v1 (contracts/engines_live_protocol_v1.md, ADR 0036): JSON lines over stdio.
// Only the fields the extension reads are typed; unknown fields are ignored both ways.

export interface Diag {
  severity: 'error' | 'warning';
  path: string;
  code: string;
  message: string;
}

/** `ready.routing` (ADR 0044): which device pair each audio source plays on (`--route-sources`). */
export interface RoutingInfo {
  channels: number;
  /** The routing map's path; absent until the piece has one (the doc's sources then play in order). */
  map?: string;
  /** A companion player may add fields of its own to each pair; they reach companions raw. */
  pairs: { pair: string; source: string; name: string }[];
  overflow: string;
  shared_with?: string;
  /** Sources with no pair of their own (they play on the overflow track). */
  unrouted: string[];
  stale: boolean;
  note?: string;
}

/** A song block: `orrery` is what its `control` says about the Orrery (ADR 0055); absent ⇒ the
 *  session's. */
export interface SongBlockInfo {
  label: string;
  loops: number;
  orrery?: boolean;
}

export interface SongInfo {
  blocks: SongBlockInfo[];
  mode: 'loop' | 'block' | 'song';
  /** The block the last `song` command named: the one looped (block mode) or played from (song). */
  block: number | null;
  /** The block a song plays from (song mode; ADR 0040). */
  from?: number | null;
  /** The block playing now (song or block mode; ADR 0040). */
  current?: number | null;
  tail_loops?: number;
}

/** Where a loop is in its block (ADR 0040): `loop_in_block` of `block_loops`; `tail` in a song's
 *  release tail (where `block_loops` is the tail's length). */
export interface SongPos {
  block: number;
  seam?: boolean;
  loop_in_block?: number;
  block_loops?: number;
  tail?: boolean;
}

/** The doc's key: `scale` in semitones above `root` (a pitch class), `names[pc]` spelled in the key. */
export interface KeyInfo {
  name: string;
  root: number;
  scale: number[];
  degrees: string[];
  names: string[];
}

/** The Orrery's voicing: semitone offsets from the root. */
export interface Voicing {
  pad: number[];
  bass: number;
  tones: number[];
}

export type LiveEvent =
  | {
      type: 'ready';
      protocol: number;
      /** What a companion player adds; `[]` (or absent, from an older player) from the free one. */
      capabilities?: string[];
      piece: string;
      rev: number;
      sample_rate: number;
      loop_s: number;
      master_steps?: number;
      device: string | null;
      limiter?: boolean;
      playing: boolean;
      optimize: string;
      orrery: boolean;
      instruments: string[];
      muted: string[];
      soloed: string[];
      song: SongInfo | null;
      key?: KeyInfo | null;
      note?: string | null;
      routing?: RoutingInfo | null;
    }
  /** `stale`: the map changed on disk (restart to play it). Otherwise a restart re-read it: `routing` is
   *  what now plays (a MIDI switch's new state among it). */
  | { type: 'routing'; stale: boolean; message?: string; routing?: RoutingInfo | null }
  | { type: 'rejected'; version?: number | null; errors: Diag[]; source: 'command' | 'disk' }
  | { type: 'unchanged'; version?: number | null; rev: number; source?: 'command' | 'disk' }
  | { type: 'accepted'; version?: number | null; rev: number; warnings: Diag[] }
  | { type: 'applied'; rev: number; loop: number; at_s: number; abs_frame: number; kind: 'value' | 'structural'; xfade_s: number }
  | {
      type: 'tick';
      loop: number;
      pos_s: number;
      loop_s: number;
      steps: Record<string, number>;
      meters: Record<string, number>;
      master_db: number;
      gr_db: number | null;
      /** `{ instrument: [left, right] }` dBFS (ADR 0040). */
      meters_lr?: Record<string, [number, number]>;
      master_lr?: [number, number];
      song?: SongPos | null;
    }
  | {
      type: 'loop';
      loop: number;
      rev: number;
      optimize: string;
      /** Whether the Orrery ran this loop (a song block can switch it; absent from older players). */
      orrery?: boolean;
      chord?: string | null;
      tension?: number | null;
      peak_db: number;
      song: SongPos | null;
      voicing?: Voicing | null;
    }
  | { type: 'gains'; muted: string[]; soloed: string[] }
  | { type: 'limiter'; on: boolean }
  | { type: 'finished'; mode: string }
  | { type: 'control'; optimize: string; orrery: boolean }
  | { type: 'song'; mode: 'loop' | 'block' | 'song'; block?: number | null }
  | {
      type: 'preview';
      path: string;
      seconds: number;
      peak_db: number;
      lufs?: number | null;
      /** ADR 0040: the stems rendered beside the mix (`null` for a mix-only preview). */
      stems?: { dir: string; target_lufs: number; files: { id: string; kind: string; file: string }[]; skipped: unknown[] } | null;
    }
  | { type: 'underrun'; count: number }
  | { type: 'external_change'; rev: number }
  | { type: 'stopped' }
  | { type: 'playing'; mode: string; block: number | null }
  | { type: 'error'; message: string; id?: unknown };

export type LiveCommand =
  | { type: 'doc'; text: string; version?: number; source?: string }
  | { type: 'play' }
  | { type: 'stop' }
  | { type: 'restart' }
  | { type: 'mute'; instrument: string; on: boolean }
  | { type: 'solo'; instrument: string; on: boolean }
  | { type: 'clear_solo' }
  | { type: 'control'; optimize?: 'hold' | 'drift'; orrery?: boolean }
  | { type: 'song'; action: 'loop_block' | 'play_from'; block: number }
  | { type: 'song'; action: 'stop' }
  | { type: 'preview'; seconds: number; stems?: boolean }
  | { type: 'limiter'; on: boolean }
  | { type: 'status' }
  | { type: 'editor'; unsaved: boolean }
  /** Play on another device (`null`: the system output), each source on its own pair or not (ADR 0044 D7). */
  | { type: 'output'; device: string | null; route_sources: boolean; id?: string }
  | { type: 'quit' };

/** Parse one stdout line; `undefined` for anything that isn't a typed JSON object. */
export function parseEvent(line: string): LiveEvent | undefined {
  try {
    const v = JSON.parse(line);
    return v && typeof v === 'object' && typeof v.type === 'string' ? (v as LiveEvent) : undefined;
  } catch {
    return undefined;
  }
}
