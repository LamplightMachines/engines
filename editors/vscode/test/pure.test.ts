// The editor-independent pieces: piece paths, another player, history, state folding, the
// keyboard, the panel and status-bar views, the Add-a-Part re-check, debouncing. `npm test`
// (node:test) — no VS Code needed.

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { mock, test } from 'node:test';
import { ago, readHistory, revisionPath, schemaVersion, SCHEMA_VERSION } from '../src/history';
import { liveArgs } from '../src/live/args';
import { Debouncer, Throttle } from '../src/live/debounce';
import { deviceItems, outputCommand, outputSummary, parseDevices, routeItems, sameChoice } from '../src/live/devices';
import type { KeyInfo, LiveEvent } from '../src/live/protocol';
import { parseEvent } from '../src/live/protocol';
import { exited, initialState, previewAsked, reduce, restarting, starting } from '../src/live/state';
import { flashFrames, pathText, topLine } from '../src/jumpView';
import { lensView } from '../src/lensView';
import { chordTones, keyboard, keyboardAria, noteName, pretty, spoken } from '../src/music';
import { MAX_STEMS_S, parseSeconds, previewKinds, previewLengths, previewMessage } from '../src/previewView';
import { blockColor, fmtDb, MASTER_COLOR, meterLevel, PALETTE, panelModel, trackColor } from '../src/panel/model';
import { currentAction } from '../src/parts';
import { listPieces, livePlayer, pieceOfDoc } from '../src/piece';
import { statusView } from '../src/statusView';

const D_DORIAN: KeyInfo = {
  name: 'D dorian',
  root: 2,
  scale: [0, 2, 3, 5, 7, 9, 10],
  degrees: ['1', '2', '♭3', '4', '5', '6', '♭7'],
  names: ['C', 'C#', 'D', 'Eb', 'E', 'F', 'F#', 'G', 'G#', 'A', 'Bb', 'B'],
};

function tmp(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'engines-vscode-'));
}

test('a piece doc is pieces/<slug>/<slug>.json5 — nothing else', () => {
  const p = pieceOfDoc('/repo/pieces/idle-nap/idle-nap.json5');
  assert.deepEqual(p, { slug: 'idle-nap', dir: '/repo/pieces/idle-nap', doc: '/repo/pieces/idle-nap/idle-nap.json5', root: '/repo' });
  assert.equal(pieceOfDoc('/repo/pieces/idle-nap/other.json5'), undefined);
  assert.equal(pieceOfDoc('/repo/pieces/idle-nap/.live/history/r00001-ab.json5'), undefined);
  assert.equal(pieceOfDoc('/repo/lab/idle/idle.json5'), undefined);
  assert.equal(pieceOfDoc('/repo/pieces/bad slug/bad slug.json5'), undefined);
});

test('pieces are listed by slug', () => {
  const root = tmp();
  for (const slug of ['b-piece', 'a-piece', 'empty']) {
    fs.mkdirSync(path.join(root, 'pieces', slug), { recursive: true });
  }
  fs.writeFileSync(path.join(root, 'pieces/a-piece/a-piece.json5'), '{}');
  fs.writeFileSync(path.join(root, 'pieces/b-piece/b-piece.json5'), '{}');
  assert.deepEqual(listPieces(root).map((p) => p.slug), ['a-piece', 'b-piece']);
  assert.deepEqual(listPieces(path.join(root, 'nowhere')), []);
});

test('another player is one whose status says playing and whose pid exists (even as another user)', () => {
  const dir = tmp();
  const status = (o: object) => {
    fs.mkdirSync(path.join(dir, '.live'), { recursive: true });
    fs.writeFileSync(path.join(dir, '.live', 'status.json'), JSON.stringify(o));
  };
  const fails = (code: string) => () => {
    throw Object.assign(new Error(code), { code });
  };
  const alive = () => {};
  assert.equal(livePlayer(dir, alive), undefined, 'no status.json');
  status({ pid: 4242, state: 'stopped' });
  assert.equal(livePlayer(dir, alive), undefined, 'a player that stopped');
  status({ pid: 4242, state: 'playing' });
  assert.equal(livePlayer(dir, alive), 4242);
  assert.equal(livePlayer(dir, fails('ESRCH')), undefined, 'stale: the pid is gone');
  assert.equal(livePlayer(dir, fails('EPERM')), 4242, "EPERM: it exists, it's just not ours");
  status({ pid: 4242, state: 'paused' });
  assert.equal(livePlayer(dir, fails('EPERM')), 4242);
  fs.writeFileSync(path.join(dir, '.live', 'status.json'), 'null');
  assert.equal(livePlayer(dir, alive), undefined);
  status({ pid: process.pid, state: 'playing' });
  assert.equal(livePlayer(dir), process.pid, 'the real probe: this process exists');
});

test('history reads the index, skipping a torn line', () => {
  const dir = tmp();
  const h = path.join(dir, '.live', 'history');
  fs.mkdirSync(h, { recursive: true });
  fs.writeFileSync(
    path.join(h, 'index.jsonl'),
    '{"rev":1,"hash":"aa","file":"r00001-aa.json5","at":100,"source":"engines-live"}\n{"rev":2,"hash":"bb","file":"r00002-bb.json5","at":200,"source":"engines-song"}\n{"rev":3,"ha',
  );
  const revs = readHistory(dir);
  assert.deepEqual(revs.map((r) => [r.rev, r.source]), [[1, 'engines-live'], [2, 'engines-song']]);
  assert.equal(revisionPath(dir, 2), path.join(h, 'r00002-bb.json5'));
  assert.equal(revisionPath(dir, 9), undefined);
  assert.deepEqual(readHistory(path.join(dir, 'missing')), []);
  // A revision recorded before the rename lists like any other.
  fs.appendFileSync(path.join(h, 'index.jsonl'), '\n{"rev":4,"hash":"dd","file":"r00004-dd.json5","at":400,"source":"sme-live"}\n');
  assert.deepEqual(readHistory(dir).map((r) => [r.rev, r.source]).at(-1), [4, 'sme-live']);
  assert.equal(ago(1000, 1010), 'just now');
  assert.equal(ago(1000, 1000 + 300), '5 min ago');
  assert.equal(ago(0, 7200), '2 h ago');
});

test('a snapshot names its schema (a pre-v0.9 revert is migrated first, ADR 0054 U6)', () => {
  assert.equal(SCHEMA_VERSION, '0.9');
  assert.equal(schemaVersion('{\n  // a comment\n  schema_version: "0.8",\n}'), '0.8');
  assert.equal(schemaVersion('{"schema_version":"0.9","loop_id":1}'), '0.9');
  assert.equal(schemaVersion("{ 'schema_version': '0.7' }"), '0.7');
  assert.equal(schemaVersion('{ transport: {} }'), undefined);
});

test('protocol lines parse to typed events or nothing', () => {
  assert.equal(parseEvent('{"type":"stopped"}')?.type, 'stopped');
  assert.equal(parseEvent('not json'), undefined);
  assert.equal(parseEvent('[1,2]'), undefined);
  assert.equal(parseEvent('{"no":"type"}'), undefined);
});

const ready: LiveEvent = {
  type: 'ready',
  protocol: 1,
  piece: 'demo',
  rev: 3,
  sample_rate: 48000,
  loop_s: 4,
  device: null,
  playing: true,
  optimize: 'drift',
  orrery: false,
  instruments: ['pad', 'sub'],
  muted: [],
  soloed: [],
  song: { blocks: [{ label: 'settle', loops: 4 }, { label: 'deep', loops: 8 }], mode: 'loop', block: null },
  key: D_DORIAN,
};

test('events fold into the state the panel shows', () => {
  let s = starting(initialState(), 'demo', 0);
  assert.equal(s.phase, 'starting');
  s = reduce(s, ready, 1);
  assert.equal(s.phase, 'running');
  assert.equal(s.appliedRev, 3);
  assert.match(s.activity[0].text, /playing r3 \(headless\)/);
  s = reduce(s, { type: 'tick', loop: 2, pos_s: 1, loop_s: 4, steps: { seq: 3 }, meters: { pad: -12 }, master_db: -10, gr_db: null });
  assert.deepEqual([s.loop, s.steps, s.meters], [2, { seq: 3 }, { pad: -12 }]);
  s = reduce(s, { type: 'accepted', version: 5, rev: 4, warnings: [] });
  assert.deepEqual([s.rev, s.appliedRev], [4, 3], 'accepted, not yet heard');
  s = reduce(s, { type: 'applied', rev: 4, loop: 2, at_s: 1.25, abs_frame: 0, kind: 'value', xfade_s: 0.03 });
  assert.equal(s.appliedRev, 4);
  assert.match(s.activity[0].text, /r4 playing from loop 2 at 1\.25 s \(value\)/);
  s = reduce(s, { type: 'rejected', version: 6, errors: [{ severity: 'error', path: 'instruments.pad.params.cutof', code: 'unknown-key', message: 'unknown key' }], source: 'command' });
  assert.equal(s.rejected?.errors.length, 1);
  assert.match(s.activity[0].text, /edit refused \(1 error\): instruments\.pad\.params\.cutof: unknown key — still playing r4/);
  s = reduce(s, { type: 'unchanged', version: 7, rev: 4 });
  assert.equal(s.rejected, null, 'the buffer is good again');
  s = reduce(s, { type: 'gains', muted: ['sub'], soloed: [] });
  s = reduce(s, { type: 'song', mode: 'block', block: 1 });
  assert.deepEqual([s.song?.mode, s.song?.block], ['block', 1]);
  s = reduce(s, { type: 'loop', loop: 3, rev: 4, optimize: 'hold', chord: 'Fmaj7', tension: 0.4, peak_db: -6, song: null, voicing: { pad: [3, 7, 10, 14], bass: -9, tones: [3, 7, 10, 14] } });
  assert.deepEqual([s.optimize, s.chord, s.voicing?.bass], ['hold', 'Fmaj7', -9]);
  // The same key again (a status refresh) keeps the voicing; a new key drops it until the next loop.
  assert.equal(reduce(s, ready).voicing?.bass, -9);
  const newKey = reduce(s, { ...ready, key: { ...(ready as Extract<LiveEvent, { type: 'ready' }>).key!, name: 'E dorian', root: 4 } } as LiveEvent);
  assert.deepEqual([newKey.chord, newKey.voicing], [null, null]);
  s = reduce(s, { type: 'stopped' });
  assert.equal(s.playing, false);
  assert.deepEqual([s.chord, s.tension, s.voicing], [null, null, null], 'nothing sounds: no chord on the keyboard');
  for (const ev of [{ type: 'finished', mode: 'song' }, { type: 'playing', mode: 'loop', block: null }] as LiveEvent[]) {
    const withChord = reduce(s, { type: 'loop', loop: 4, rev: 4, optimize: 'hold', chord: 'Dm', tension: 0, peak_db: -6, song: null, voicing: { pad: [0], bass: 0, tones: [0] } });
    assert.equal(reduce(withChord, ev).voicing, null, ev.type);
  }
  assert.equal(exited({ ...s, chord: 'Dm' }, { expected: false, restarting: true }).chord, null);
  const crashed = exited(s, { expected: false, restarting: true, message: 'boom' });
  assert.equal(crashed.phase, 'restarting');
  assert.equal(exited(s, { expected: false, restarting: false, message: 'no device' }).phase, 'failed');
  assert.equal(exited(s, { expected: true, restarting: false }).phase, 'off');
  const again = starting(crashed, 'demo');
  assert.deepEqual([again.appliedRev, again.muted], [0, ['sub']], 'a restart keeps what it knew, but not the playing revision');
  // During the backoff the crash reason stays up (the status bar's tooltip, the panel's message).
  const backoff = restarting(crashed, 'demo');
  assert.deepEqual([backoff.phase, backoff.message, backoff.appliedRev, backoff.muted], ['restarting', 'boom', 0, ['sub']]);
  assert.equal(statusView(backoff, undefined)?.tooltip, 'boom');
  assert.equal(panelModel(backoff, []).header.message, 'boom');
  assert.equal(reduce(backoff, ready).message, null, 'back up: the reason goes');
  assert.equal(starting(s, 'other').instruments.length, 0, 'another piece starts fresh');
});

test('the keyboard marks the scale, the chord and the voicing', () => {
  assert.equal(noteName('C', 60), 'C4');
  assert.equal(noteName('B#', 60), 'B#3');
  assert.equal(noteName('Cb', 59), 'Cb4');
  assert.equal(noteName('Bb', 58), 'Bb3');
  assert.equal(keyboard(null, null), null);
  // Fmaj7 over D dorian: F A C E, bass F2.
  const kb = keyboard(D_DORIAN, { pad: [3, 7, 10, 14], bass: -21, tones: [3, 7, 10, 14] })!;
  assert.equal(kb.low, 36, 'widened down to the bass octave');
  assert.equal(kb.high, 81, 'up to A5 (nothing voiced above it)');
  assert.deepEqual([keyboard(D_DORIAN, null)!.low, keyboard(D_DORIAN, null)!.high], [48, 81], 'C3–A5 by default');
  assert.equal(keyboard(D_DORIAN, { pad: [21], bass: 0, tones: [0] })!.high, 83, 'a pad note above A5 (B5) widens to its octave');
  const at = (midi: number) => kb.cells.find((c) => c.midi === midi)!;
  assert.deepEqual([at(62).name, at(62).root, at(62).degree], ['D4', true, '1']);
  assert.deepEqual([at(65).name, at(65).inScale, at(65).chord, at(65).pad], ['F4', true, true, true]);
  assert.deepEqual([at(66).name, at(66).inScale, at(66).black], ['F#4', false, true]);
  assert.equal(at(41).bass, true);
  assert.equal(at(41).name, 'F2');
  assert.equal(kb.cells.filter((c) => c.pad).length, 4);
});

test('the panel model', () => {
  let s = reduce(starting(initialState(), 'demo'), ready);
  s = reduce(s, { type: 'tick', loop: 1, pos_s: 1, loop_s: 4, steps: {}, meters: { pad: -30, sub: -125 }, master_db: -0.5, gr_db: -1.2, meters_lr: { pad: [-30, -31.5], sub: [-125, -125] }, master_lr: [-0.5, -0.9] });
  s = reduce(s, { type: 'gains', muted: [], soloed: ['pad'] });
  s = reduce(s, { type: 'accepted', version: 2, rev: 4, warnings: [] });
  const history = [1, 2, 3, 4].map((rev) => ({ rev, hash: '', file: `r${rev}.json5`, at: 1000 + rev, source: 'editor' }));
  const m = panelModel(s, history, 1_000_000 * 1000);
  assert.equal(m.live, true);
  assert.equal(m.header.rev, 'r3 → r4');
  assert.deepEqual(m.header.pill, { kind: 'playing', text: 'Playing', pulse: true });
  assert.equal(m.loop.progress, 0.25);
  // Strips in the player's (document) order, coloured by position; the master outside the cycle.
  assert.deepEqual(m.instruments.strips.map((i) => [i.id, i.color, i.soloed, i.silenced]), [['pad', PALETTE[0], true, false], ['sub', PALETTE[1], false, true]]);
  assert.deepEqual([m.instruments.strips[0].lDb, m.instruments.strips[0].rDb], [-30, -31.5]);
  assert.deepEqual(m.instruments.strips[0].path, ['instruments', 'pad']);
  assert.deepEqual([m.instruments.master.lDb, m.instruments.master.rDb, m.instruments.soloed], [-0.5, -0.9, 1]);
  assert.equal(m.transport.grDb, -1.2);
  assert.equal(MASTER_COLOR, '#d0d0d0');
  // Revisions: the newest first; the one playing can't be reverted to, nor can the newest.
  assert.deepEqual(m.revisions.rows.map((r) => [r.rev, r.current, r.newest, r.canRevert]), [[4, null, true, false], [3, 'playing', false, false], [2, null, false, true], [1, null, false, true]]);
  assert.equal(m.revisions.summary, 'r4 · 4 revisions');
  assert.equal(m.revisions.more, false);
  const long = [...Array(30)].map((_, i) => ({ rev: i + 1, hash: '', file: '', at: 0, source: 'disk' }));
  const lm = panelModel(s, long);
  assert.deepEqual([lm.revisions.rows.length, lm.revisions.rows[0].rev, lm.revisions.more], [10, 30, true]);
  assert.deepEqual([m.harmony.root, m.harmony.mode, m.harmony.summary, m.harmony.tensionLit], ['D', 'Dorian', 'Orrery off', null]);
  assert.ok(m.harmony.keyboard);
  assert.equal(m.unsaved, false);
  assert.equal(panelModel({ ...s, unsaved: true }, history).unsaved, true);
  // Stopped, then gone.
  assert.deepEqual(panelModel(reduce(s, { type: 'stopped' }), history).header.pill.kind, 'stopped');
  assert.deepEqual(panelModel(reduce(s, { type: 'finished', mode: 'song' }), history).header.pill, { kind: 'ended', text: 'Ended', pulse: false });
  const off = panelModel(exited(s, { expected: true, restarting: false }), history);
  assert.deepEqual([off.live, off.header.pill.kind, off.header.device], [false, 'off', '']);
});

test('ready’s capabilities, and a restart’s routing event replaces what plays', () => {
  const pairs = [{ pair: '1/2', source: 'inst:pad', name: 'pad' }];
  const routing = { channels: 16, pairs, overflow: '3/4', unrouted: [], stale: false };
  let s = reduce(initialState(), { type: 'ready', protocol: 1, capabilities: ['midi_mirror'], piece: 'p', rev: 1, sample_rate: 48000, loop_s: 4, master_steps: 4, device: 'BlackHole 64ch', limiter: true, playing: true, optimize: 'drift', orrery: false, instruments: ['pad'], muted: [], soloed: [], note: null, song: null, key: null, routing } as never, 1);
  assert.deepEqual(s.capabilities, ['midi_mirror'], 'a companion player’s, kept for its companion');
  assert.deepEqual(reduce(initialState(), { type: 'ready', protocol: 1, piece: 'p', rev: 1, sample_rate: 48000, loop_s: 4, device: null, playing: true, optimize: 'drift', orrery: false, instruments: [], muted: [], soloed: [], song: null } as never, 1).capabilities, [], 'an older player: none');
  s = reduce(s, { type: 'routing', stale: true, message: 'the routing map changed' });
  assert.equal(s.routing?.stale, true, 'stale: the strips keep what plays');
  s = reduce(s, { type: 'routing', stale: false, routing: { ...routing, pairs: [{ ...pairs[0], name: 'pad dry' }] } });
  assert.deepEqual([s.routing?.pairs[0].name, s.routing?.stale], ['pad dry', false]);
});

test('identity colours cycle by position; blocks start half-way round', () => {
  assert.equal(PALETTE.length, 12);
  assert.equal(new Set(PALETTE).size, 12);
  assert.deepEqual([trackColor(0), trackColor(12), trackColor(13)], [PALETTE[0], PALETTE[0], PALETTE[1]]);
  assert.deepEqual([blockColor(0), blockColor(5), blockColor(6)], [PALETTE[6], PALETTE[11], PALETTE[0]]);
  // idle-nap: tracks sub coral, crank jade …; blocks settle teal, idle pink (SPEC §3.5).
  assert.deepEqual([trackColor(0), trackColor(1), blockColor(0), blockColor(1)], ['#ff94a6', '#00bfaf', '#88c2ba', '#bc7196'], 'Live palette colours (ADR 0044 D10)');
});

test('the song section: structure bar, playhead, tags and Back to loop', () => {
  const songReady = { ...ready, song: { blocks: [{ label: 'settle', loops: 4 }, { label: 'deep', loops: 8 }], mode: 'loop', block: null, tail_loops: 1 } } as LiveEvent;
  let s = reduce(starting(initialState(), 'demo'), songReady);
  let m = panelModel(s, []);
  assert.deepEqual(m.song?.segments.map((g) => [g.label, g.weight, g.color, g.current, g.tail]), [['settle', 4, blockColor(0), false, false], ['deep', 8, blockColor(1), false, false], ['tail', 1, null, false, true]]);
  assert.deepEqual([m.song?.back.enabled, m.song?.summary], [false, 'the loop playing']);
  assert.match(m.song!.aria, /^Song structure: settle 4, deep 8 loops, tail 1\. The song is not playing/);
  // ▶ deep… no: ▶ settle, and the song reaches deep: PLAY FROM stays on settle, PLAYING moves.
  s = reduce(s, { type: 'song', mode: 'song', block: 0 });
  s = reduce(s, { type: 'tick', loop: 6, pos_s: 2, loop_s: 4, steps: {}, meters: {}, master_db: -9, gr_db: null, song: { block: 1, loop_in_block: 2, block_loops: 8 } });
  m = panelModel(s, []);
  assert.deepEqual(m.song?.rows.map((r) => [r.label, r.playing, r.from, r.looping, r.count]), [['settle', false, true, false, '4'], ['deep', true, false, false, '3 / 8']]);
  const deep = m.song!.segments[1];
  assert.deepEqual([deep.current, deep.frac], [true, (2 + 0.5) / 8]);
  assert.deepEqual([m.song?.back.enabled, m.song?.summary], [true, 'deep playing · from settle']);
  assert.match(m.song!.aria, /Playhead in deep, loop 3 of 8\.$/);
  // The tail: its own grey segment carries the playhead.
  s = reduce(s, { type: 'tick', loop: 13, pos_s: 0, loop_s: 4, steps: {}, meters: {}, master_db: -9, gr_db: null, song: { block: 1, loop_in_block: 0, block_loops: 1, tail: true } });
  m = panelModel(s, []);
  assert.deepEqual(m.song?.segments.map((g) => g.current), [false, false, true]);
  assert.equal(m.song?.summary, 'the tail playing · from settle');
  // ⟲ deep: LOOPING, and it's the block playing.
  s = reduce(s, { type: 'song', mode: 'block', block: 1 });
  m = panelModel(s, []);
  assert.deepEqual(m.song?.rows.map((r) => [r.playing, r.from, r.looping]), [[false, false, false], [true, false, true]]);
});

test('a block that switches the Orrery says so, and the loops say what plays (ADR 0055)', () => {
  const songReady = { ...ready, orrery: true, song: { blocks: [{ label: 'settle', loops: 4 }, { label: 'deep', loops: 8, orrery: false }], mode: 'song', block: 0, tail_loops: 1 } } as LiveEvent;
  let s = reduce(starting(initialState(), 'demo'), songReady);
  assert.deepEqual(panelModel(s, []).song?.rows.map((r) => r.orrery), [null, false]);
  // The song reaches the Orrery-off block: the transport's Orrery switch shows what plays.
  s = reduce(s, { type: 'loop', loop: 4, rev: 3, optimize: 'drift', orrery: false, peak_db: -9, song: { block: 1, loop_in_block: 0, block_loops: 8 } });
  assert.equal(panelModel(s, []).transport.orrery, false);
  // An older player's loop (no `orrery`) leaves it alone.
  s = reduce(s, { type: 'loop', loop: 5, rev: 3, optimize: 'drift', peak_db: -9, song: { block: 1, loop_in_block: 1, block_loops: 8 } });
  assert.equal(s.orrery, false);
});

test('dB readouts use the real minus sign and −∞ for silence', () => {
  assert.deepEqual([fmtDb(-7.25), fmtDb(0), fmtDb(-0.01), fmtDb(-120), fmtDb(null), fmtDb(1.5)], ['−7.3', '0.0', '0.0', '−∞', '−∞', '1.5']);
  assert.deepEqual([meterLevel(0), meterLevel(-60), meterLevel(-30), meterLevel(-90)], [1, 0, 0.5, 0]);
});

test('the status bar says what plays', () => {
  assert.equal(statusView(initialState(), undefined), undefined);
  assert.equal(statusView(initialState(), 'idle')?.command, 'engines.live.start');
  let s = reduce(starting(initialState(), 'demo'), ready);
  assert.equal(statusView(s, undefined)?.text, '$(play) demo r3 · drift');
  assert.equal(statusView(s, undefined)?.playing, true, 'the text takes the live colour');
  s = reduce(s, { type: 'loop', loop: 1, rev: 3, optimize: 'hold', chord: 'Dm7', tension: 0.1, peak_db: -6, song: null });
  s = reduce(s, { type: 'song', mode: 'block', block: 1 });
  assert.equal(statusView(s, undefined)?.text, '$(play) demo r3 · hold · ⟲ deep · Dm7');
  // Edits the player hasn't heard (they play on save) are marked, and the events don't clear it.
  const unsaved = reduce({ ...s, unsaved: true }, { type: 'tick', loop: 2, pos_s: 0, loop_s: 4, steps: {}, meters: {}, master_db: -9, gr_db: null });
  assert.equal(statusView(unsaved, undefined)?.text, '$(play) demo r3 · hold · ⟲ deep · Dm7 · ● unsaved');
  assert.match(statusView(unsaved, undefined)?.tooltip ?? '', /save \(⌘S\) to hear them/);
  s = reduce(s, { type: 'rejected', errors: [{ severity: 'error', path: 'x', code: 'c', message: 'bad' }], source: 'command' });
  assert.equal(statusView(s, undefined)?.tone, 'warning');
  assert.equal(statusView(exited(s, { expected: false, restarting: false, message: 'no device' }), undefined)?.tone, 'error');
});

test('a song remembers where it plays from while it moves on (ADR 0040)', () => {
  let s = reduce(starting(initialState(), 'demo'), { ...ready, master_steps: 16, limiter: false, note: 'a nap' } as LiveEvent);
  assert.deepEqual([s.masterSteps, s.limiter, s.note, s.song?.from, s.song?.current, s.song?.tailLoops], [16, false, 'a nap', null, null, 0]);
  s = reduce(s, { type: 'song', mode: 'song', block: 0 });
  assert.deepEqual([s.song?.block, s.song?.from, s.song?.current], [0, 0, 0]);
  // The song moves into block 1: the play-from block stays 0.
  s = reduce(s, { type: 'loop', loop: 5, rev: 3, optimize: 'drift', peak_db: -9, song: { block: 1, seam: true, loop_in_block: 0, block_loops: 8 } });
  assert.deepEqual([s.song?.block, s.song?.from, s.song?.current], [0, 0, 1]);
  s = reduce(s, { type: 'tick', loop: 5, pos_s: 1, loop_s: 4, steps: {}, meters: { pad: -6 }, master_db: -3, gr_db: null, meters_lr: { pad: [-6, -7.5] }, master_lr: [-3, -4], song: { block: 1, loop_in_block: 2, block_loops: 8 } });
  assert.deepEqual([s.metersLR.pad, s.masterLR, s.songPos?.loop_in_block], [[-6, -7.5], [-3, -4], 2]);
  assert.equal(statusView(s, undefined)?.text, '$(play) demo r3 · drift · ♫ deep', 'the status names the block playing');
  // An older player's tick (no L/R): both channels show the max.
  const old = reduce(s, { type: 'tick', loop: 6, pos_s: 0, loop_s: 4, steps: {}, meters: { pad: -9 }, master_db: -5, gr_db: null });
  assert.deepEqual([old.metersLR.pad, old.masterLR], [[-9, -9], [-5, -5]]);
  // Back to the loop: no block, no position; a stop silences every meter.
  s = reduce(s, { type: 'song', mode: 'loop', block: null });
  assert.deepEqual([s.song?.from, s.song?.current, s.songPos], [null, null, null]);
  s = reduce(s, { type: 'stopped' });
  assert.deepEqual([s.metersLR, s.masterLR], [{}, [-120, -120]]);
  // A block looped: current is that block; ready's own from/current win when the player sends them.
  s = reduce(s, { type: 'song', mode: 'block', block: 1 });
  assert.deepEqual([s.song?.from, s.song?.current], [null, 1]);
  const r = reduce(s, { ...ready, song: { blocks: [{ label: 'a', loops: 1 }, { label: 'b', loops: 2 }], mode: 'song', block: 0, from: 0, current: 1, tail_loops: 2 } } as LiveEvent);
  assert.deepEqual([r.song?.from, r.song?.current, r.song?.tailLoops], [0, 1, 2]);
});

test('the keyboard is spelled for the eye and for a screen reader', () => {
  assert.deepEqual(['F#4', 'Bb3', 'C', 'F##'].map(pretty), ['F♯4', 'B♭3', 'C', 'F♯♯']);
  assert.deepEqual(['F#4', 'Bb', 'A5'].map(spoken), ['F sharp 4', 'B flat', 'A 5']);
  const A_MIXO: KeyInfo = { name: 'A mixolydian', root: 9, scale: [0, 2, 4, 5, 7, 9, 10], degrees: [], names: ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'Bb', 'B'] };
  // Gmaj7 over A mixolydian: G B D F#, the pad in octave 4–5, the bass G3.
  const v = { pad: [-2, 2, 5, 9], bass: -14, tones: [-2, 2, 5, 9] };
  assert.deepEqual(chordTones(A_MIXO, v), ['G', 'B', 'D', 'F♯']);
  assert.deepEqual(chordTones(A_MIXO, null), []);
  const aria = keyboardAria(keyboard(A_MIXO, v), A_MIXO, v);
  assert.equal(aria, 'Keyboard C 3 to A 5. A mixolydian scale highlighted. Chord tones G, B, D, F sharp. Pad notes G 4, B 4, D 5, F sharp 5. Bass note G 3.');
  assert.equal(keyboardAria(null, null), 'Keyboard: no key');
});

test('CodeLens titles lead with an icon and say what a click does', () => {
  const off = initialState();
  assert.deepEqual(lensView('engines.solo', 'solo', 'pad', off), { title: '$(circle-large-outline) solo', tooltip: 'Solo pad' });
  assert.deepEqual(lensView('engines.mute', 'mute', 'pad', off), { title: '$(unmute)', tooltip: 'Mute pad' });
  // Not playing: the block's label and length come from the server's title.
  assert.equal(lensView('engines.loopBlock', '▶ loop block 1 · deep · 4 loops', 1, off)?.title, '$(sync) loop · deep · 4 loops');
  assert.equal(lensView('engines.playFrom', 'play from here', 1, off)?.title, '$(play) play from block 1');
  assert.equal(lensView('other', 'x', 0, off), undefined);
  let s = reduce(starting(initialState(), 'demo'), ready);
  s = reduce(s, { type: 'gains', muted: ['pad'], soloed: ['sub'] });
  assert.deepEqual(lensView('engines.solo', 'solo', 'sub', s), { title: '$(circle-large-filled) solo', tooltip: 'Unsolo sub' });
  assert.deepEqual(lensView('engines.mute', 'mute', 'pad', s), { title: '$(mute) muted', tooltip: 'Unmute pad' });
  assert.equal(lensView('engines.playFrom', 'play from here', 1, s)?.title, '$(play) play from deep', 'playing: the label from the player');
  s = reduce(s, { type: 'song', mode: 'block', block: 1 });
  assert.equal(lensView('engines.loopBlock', '▶ loop block 1', 1, s)?.title, '$(debug-stop) looping deep');
  s = reduce(s, { type: 'song', mode: 'song', block: 0 });
  s = reduce(s, { type: 'loop', loop: 9, rev: 3, optimize: 'drift', peak_db: -9, song: { block: 1 } });
  assert.equal(lensView('engines.playFrom', 'play from here', 0, s)?.title, '$(debug-stop) playing from settle', 'the play-from block, not the one the song reached');
  assert.equal(lensView('engines.playFrom', 'play from here', 1, s)?.title, '$(play) play from deep');
});

test('the jump lands with context above and flashes out', () => {
  assert.deepEqual([topLine(10), topLine(2)], [6, 0]);
  const f = flashFrames(8, 1600, 0.38);
  assert.equal(f.length, 8);
  assert.deepEqual([f[0], f[7].at], [{ at: 0, alpha: 0.38 }, 1400]);
  assert.ok(f.every((x, i) => i === 0 || x.alpha < f[i - 1].alpha), 'fading');
  assert.equal(pathText(['song', 'sequence', 1]), 'song.sequence[1]');
});

test('the panel uses only the design tokens that exist', () => {
  const media = path.join(__dirname, '..', '..', 'media');
  const tokens = fs.readFileSync(path.join(media, 'tokens.css'), 'utf8');
  const defined = new Set([...tokens.matchAll(/(--engines-[\w-]+)\s*:/g)].map((m) => m[1]));
  for (const file of ['panel.css', 'panel.js']) {
    const used = new Set([...fs.readFileSync(path.join(media, file), 'utf8').matchAll(/var\((--engines-[\w-]+)/g)].map((m) => m[1]));
    assert.ok(used.size > 0 || file === 'panel.js');
    assert.deepEqual([...used].filter((v) => !defined.has(v)), [], `${file} uses tokens tokens.css doesn't define`);
  }
  // The palette the model assigns is the one the tokens document.
  PALETTE.forEach((c, i) => assert.match(tokens, new RegExp(`--engines-id-${i}: ${c};`), `--engines-id-${i}`));
});

test('the panel paints text and surfaces only with tokens (so high-contrast themes can remap them)', () => {
  const css = fs.readFileSync(path.join(__dirname, '..', '..', 'media', 'panel.css'), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
  const literal = /(?:^|[;{\s])(color|background(?:-color)?)\s*:\s*[^;]*(#[0-9a-f]{3,8}\b|rgba?\()/i;
  const offenders = [...css.matchAll(/([^{}]+)\{([^}]*)\}/g)]
    .filter(([, sel, body]) => literal.test(body) && !/^\s*\.key/.test(sel)) // the keyboard is a fixed-colour graphic
    .map(([, sel]) => sel.trim());
  assert.deepEqual(offenders, []);
});

test('the preview dialog: a length, then the mix alone or with stems', () => {
  assert.deepEqual(previewLengths().map((l) => l.seconds), [30, 60, 120, 300, undefined]);
  assert.deepEqual([parseSeconds('45'), parseSeconds('600'), typeof parseSeconds('601'), typeof parseSeconds('x'), typeof parseSeconds('0')], [45, 600, 'string', 'string', 'string']);
  const [mix, stems] = previewKinds(30, 5);
  assert.deepEqual([mix.stems, stems.stems], [false, true]);
  assert.equal(mix.description, '≈ 12 MB', '30 s of float stereo at 48 kHz');
  assert.equal(stems.description, '≈ 55 MB+', 'the mix + five 24-bit stems (and a bus each, unknown here)');
  assert.match(stems.detail, /one −18 LUFS 24-bit WAV per instrument \(5\) and per bus$/);
  const [long, longStems] = previewKinds(600, 5);
  assert.equal(long.description, '≈ 230 MB', 'a mix alone runs the whole 10 min');
  assert.equal(longStems.description, '≈ 547 MB+', 'with stems, the mix is capped at 5 min too');
  assert.match(longStems.detail, new RegExp(`mix and stems are capped at ${MAX_STEMS_S / 60} min$`));
  assert.equal(previewMessage({ seconds: 30, peak_db: -8.84, lufs: -17.23, stems: { files: [1, 2, 3] } }), 'Preview rendered: 30 s, peak -8.8 dBFS, -17.2 LUFS + 3 stems.');
  assert.equal(previewMessage({ seconds: 2, peak_db: -6, stems: null }), 'Preview rendered: 2 s, peak -6.0 dBFS.');
});

test('a preview shows as rendering until its event (or its error) comes back', () => {
  let s = previewAsked(reduce(starting(initialState(), 'demo'), ready));
  assert.equal(panelModel(s, []).activity.previewing, true);
  assert.equal(panelModel(s, []).activity.summary, 'rendering a preview…');
  assert.equal(reduce(s, { type: 'error', message: 'a preview is already rendering — wait' }).previewing, true, 'the one rendering still is');
  assert.equal(reduce(s, { type: 'error', message: 'preview: not enough disk' }).previewing, false);
  s = reduce(s, { type: 'preview', path: '/p/.live/preview.wav', seconds: 2, peak_db: -9, lufs: -20, stems: { dir: '/p/.live/preview_stems', target_lufs: -18, files: [{ id: 'pad', kind: 'instrument', file: 'pad.wav' }], skipped: [] } });
  const m = panelModel(s, []);
  assert.deepEqual([m.activity.previewing, m.activity.preview, m.activity.stems], [false, '/p/.live/preview.wav', '/p/.live/preview_stems']);
  assert.match(s.activity[0].text, /\+ 1 stem$/);
});

test('a preview the player died rendering stops showing as rendering, through a crash restart', () => {
  const s = previewAsked(reduce(starting(initialState(), 'demo'), ready));
  const crashed = exited(s, { expected: false, restarting: true, message: 'boom' });
  assert.equal(crashed.previewing, false);
  assert.equal(reduce(restarting(crashed, 'demo'), ready).previewing, false);
  assert.equal(exited(s, { expected: true, restarting: false }).previewing, false);
});

test('a part picked from an older text is asked for again', async () => {
  const pulse = { title: 'Add part: plucked pulse', edit: 'old' };
  const breath = { title: 'Add part: breath — on `pad`', edit: 'old' };
  let version = 7;
  let asked = 0;
  const refetch = async () => {
    asked++;
    return [{ ...pulse, edit: `v${version}` }, { ...breath, title: 'Add part: breath — on `bell`' }];
  };
  assert.equal(await currentAction(pulse, 7, () => version, refetch), pulse, 'unchanged: the picked one');
  assert.equal(asked, 0);
  version = 8;
  assert.deepEqual(await currentAction(pulse, 7, () => version, refetch), { ...pulse, edit: 'v8' }, 'changed: the fresh one');
  assert.equal(asked, 1);
  assert.equal(await currentAction(breath, 7, () => version, refetch), undefined, 'it would land elsewhere now');
  let n = 0;
  const restless = () => n++;
  assert.equal(await currentAction(pulse, -1, restless, refetch), undefined, "a doc that won't hold still");
});

test('debounce and throttle', () => {
  mock.timers.enable({ apis: ['setTimeout', 'Date'] });
  try {
    let runs = 0;
    const d = new Debouncer(() => runs++, 300);
    d.trigger();
    mock.timers.tick(200);
    d.trigger();
    mock.timers.tick(200);
    assert.equal(runs, 0, 'still typing');
    mock.timers.tick(100);
    assert.equal(runs, 1);
    d.trigger();
    d.cancel();
    mock.timers.tick(500);
    assert.equal(runs, 1, 'cancelled: the player quit before the pause ended');

    const seen: number[] = [];
    const t = new Throttle<number>((v) => seen.push(v), 100);
    t.push(1);
    t.push(2);
    t.push(3);
    assert.deepEqual(seen, [1], 'leading edge');
    mock.timers.tick(100);
    assert.deepEqual(seen, [1, 3], 'trailing edge: the latest value');
    mock.timers.tick(500);
    t.push(4);
    assert.deepEqual(seen, [1, 3, 4]);
    t.dispose();
  } finally {
    mock.timers.reset();
  }
});

test('liveArgs: routing needs a named device or headless', () => {
  const base = { headless: false, outputDevice: '', routeSources: false, extra: [] as string[] };
  assert.deepEqual(liveArgs('d.json5', base), ['d.json5', '--stdio']);
  assert.deepEqual(liveArgs('d.json5', { ...base, outputDevice: ' BlackHole 64ch ', routeSources: true, extra: ['--ring-ms', '120'] }), [
    'd.json5',
    '--stdio',
    '--output-device',
    'BlackHole 64ch',
    '--route-sources',
    '--ring-ms',
    '120',
  ]);
  assert.deepEqual(liveArgs('d.json5', { ...base, routeSources: true }), ['d.json5', '--stdio'], 'the system output is never routed');
  assert.deepEqual(liveArgs('d.json5', { ...base, headless: true, outputDevice: 'X', routeSources: true }), ['d.json5', '--stdio', '--headless', '--route-sources']);
});

test('the output picker: devices, routing, the command and the panel line (ADR 0044 D7)', () => {
  const json = JSON.stringify([
    { name: 'BlackHole 64ch', channels: 64, sample_rate: 48000, default: false },
    { name: 'MacBook Pro Speakers', channels: 2, sample_rate: 48000, default: true },
    { name: 'AirPods', channels: 2, sample_rate: 44100, default: false },
  ]);
  const devices = parseDevices(json);
  assert.equal(devices.length, 3);
  assert.deepEqual(parseDevices('not json'), []);
  assert.deepEqual(parseDevices('[{"name": 1}]'), [], 'malformed entries are dropped');

  const items = deviceItems(devices, { device: 'BlackHole 64ch', route: true });
  assert.deepEqual(items.map((i) => i.device), ['', 'BlackHole 64ch', 'MacBook Pro Speakers', 'AirPods'], 'the system output first');
  assert.equal(items[0].description, 'now MacBook Pro Speakers');
  assert.match(items[1].label, /^\$\(check\) BlackHole 64ch/, 'the current one is marked');
  assert.equal(items[1].description, '64 ch · 48 kHz');
  assert.match(items[1].detail ?? '', /loopback/);
  assert.equal(items[3].description, '2 ch · 44.1 kHz');
  assert.equal(items[2].detail, undefined);

  const routes = routeItems('BlackHole 64ch', { device: 'BlackHole 64ch', route: false }, 'one stereo pair each');
  assert.deepEqual(routes.map((r) => [r.route, r.label.startsWith('$(check)')]), [[true, false], [false, true]]);
  assert.equal(routes[0].description, 'one stereo pair each', 'what the pairs do: the open wording, or a companion step’s');
  assert.equal(routes[1].description, 'on 1/2', 'the mix is always the mix');

  assert.deepEqual(outputCommand({ device: '', route: true }), { type: 'output', device: null, route_sources: false }, 'never routed on the system output');
  assert.deepEqual(outputCommand({ device: 'AirPods', route: false }), { type: 'output', device: 'AirPods', route_sources: false });
  assert.ok(sameChoice({ device: '', route: true }, { device: '', route: false }), 'routing means nothing without a device');
  assert.ok(!sameChoice({ device: 'BlackHole 64ch', route: true }, { device: 'BlackHole 64ch', route: false }));

  const routing = { channels: 64, pairs: [{ pair: '1/2', source: 'inst:a', name: 'a' }, { pair: '3/4', source: 'bus:rv', name: 'bus_rv' }], overflow: '5/6', unrouted: [], stale: false };
  assert.equal(outputSummary('BlackHole 64ch', routing, 48000), 'BlackHole 64ch · 2 sources on their own pairs');
  assert.equal(outputSummary('BlackHole 64ch', routing, 44100), 'BlackHole 64ch · 2 sources on their own pairs · 44.1 kHz — set 48 kHz');
  assert.equal(outputSummary('AirPods', null, 44100), 'AirPods');
  assert.equal(outputSummary(null, null, 48000), 'headless (no audio device)');

  // The panel's device line follows `ready`.
  const ready = (device: string, r: typeof routing | null) =>
    ({ type: 'ready', protocol: 1, piece: 'demo', rev: 1, sample_rate: 48000, loop_s: 8, master_steps: 16, device, limiter: true, playing: true, optimize: 'drift', orrery: false, instruments: ['a'], muted: [], soloed: [], song: null, key: null, note: null, routing: r }) as unknown as LiveEvent;
  const s = reduce(starting(initialState(), 'demo'), ready('BlackHole 64ch', routing));
  assert.equal(panelModel(s, []).header.device, 'BlackHole 64ch · 2 sources on their own pairs');
  assert.equal(panelModel(reduce(s, ready('AirPods', null)), []).header.device, 'AirPods');
});

