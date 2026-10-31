// LiveProcess: line framing, clean quit, crash restarts with backoff, startup failures — against a
// fake child, then against the real `engines-live --stdio --headless` (skipped until it's built).

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { test } from 'node:test';
import { LiveProcess, type ExitInfo } from '../src/live/process';
import type { LiveEvent } from '../src/live/protocol';

const REPO = path.resolve(__dirname, '../../../..');
const ENGINES_LIVE = process.env.ENGINES_LIVE ?? path.join(REPO, 'target/release/engines-live');

/** A stand-in player: `ready` (split across writes), then obeys `crash` / `quit` / EOF. */
const FAKE = `
const out = (o) => process.stdout.write(JSON.stringify(o) + '\\n');
if (process.argv[2] === 'fail') { out({ type: 'error', message: 'doc has errors' }); process.exit(1); }
process.stdout.write('{"type":"rea');
setTimeout(() => process.stdout.write('dy","rev":1}\\nnot json\\n'), 20);
let buf = '';
process.stdin.on('data', (c) => {
  buf += c;
  for (const line of buf.split('\\n').slice(0, -1)) {
    const cmd = JSON.parse(line);
    if (cmd.type === 'crash') process.exit(3);
    if (cmd.type === 'quit') { out({ type: 'stopped' }); process.exit(0); }
    out({ type: 'echo', of: cmd.type });
  }
  buf = buf.slice(buf.lastIndexOf('\\n') + 1);
});
process.stdin.on('end', () => process.exit(0));
`;

function fake(arg = ''): LiveProcess {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'engines-fake-'));
  const script = path.join(dir, 'fake.js');
  fs.writeFileSync(script, FAKE);
  return new LiveProcess({ command: process.execPath, args: [script, arg], cwd: dir }, { backoff: [50], maxRestarts: 2, quitTimeoutMs: 2000 });
}

function next<T>(p: LiveProcess, what: 'event' | 'exit' | 'restarting' | 'gaveUp', pred: (v: any) => boolean = () => true, ms = 10_000): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), ms);
    const on = (v: any) => {
      if (pred(v)) {
        clearTimeout(timer);
        p.off(what, on);
        resolve(v);
      }
    };
    p.on(what, on);
  });
}

test('framing, commands and a clean quit', async () => {
  const p = fake();
  const logs: string[] = [];
  p.on('log', (l) => logs.push(l));
  const ready = next<LiveEvent>(p, 'event', (e) => e.type === 'ready');
  p.start();
  assert.equal((await ready).type, 'ready');
  assert.ok(p.isReady);
  const echo = next<any>(p, 'event', (e) => e.type === 'echo');
  assert.ok(p.send({ type: 'status' }));
  assert.equal((await echo).of, 'status');
  assert.deepEqual(logs, ['not json'], 'stray stdout goes to the log');
  const exit = next<ExitInfo>(p, 'exit');
  await p.stop();
  const info = await exit;
  assert.deepEqual([info.expected, info.code], [true, 0]);
  assert.equal(p.running, false);
  assert.equal(p.send({ type: 'status' }), false);
});

test('a quitting process is still running but takes no commands, and starts again once it has gone', async () => {
  const p = fake();
  const ready = next(p, 'event', (e) => e.type === 'ready');
  p.start();
  await ready;
  assert.deepEqual([p.isReady, p.isStopping], [true, false]);
  const stopped = p.stop();
  // ⏏ → ▶ in this window used to send `play` to a closed stdin: the host must wait for `stopped`.
  assert.deepEqual([p.running, p.isStopping, p.isReady], [true, true, false], 'on its way out');
  await stopped;
  assert.deepEqual([p.running, p.isStopping, p.isReady], [false, false, false]);
  const again = next(p, 'event', (e) => e.type === 'ready');
  p.start();
  await again;
  assert.deepEqual([p.isReady, p.isStopping], [true, false]);
  await p.stop();
});

test('a crash after ready restarts with backoff, then gives up', async () => {
  const p = fake();
  p.start();
  await next(p, 'event', (e) => e.type === 'ready');
  const exit = next<ExitInfo>(p, 'exit');
  const restarting = next<{ attempt: number }>(p, 'restarting');
  p.send({ type: 'crash' } as any);
  const info = await exit;
  assert.deepEqual([info.expected, info.ready, info.code], [false, true, 3]);
  assert.equal((await restarting).attempt, 1);
  await next(p, 'event', (e) => e.type === 'ready');
  // Crash twice more: the limit is 2 restarts.
  p.send({ type: 'crash' } as any);
  await next(p, 'restarting', (r) => r.attempt === 2);
  await next(p, 'event', (e) => e.type === 'ready');
  const gaveUp = next<ExitInfo>(p, 'gaveUp');
  p.send({ type: 'crash' } as any);
  assert.equal((await gaveUp).code, 3);
  assert.equal(p.running || p.restarting, false);
});

test('a failure before ready is reported, not retried', async () => {
  const p = fake('fail');
  let restarted = false;
  p.on('restarting', () => (restarted = true));
  const exit = next<ExitInfo>(p, 'exit');
  p.start();
  const info = await exit;
  assert.deepEqual([info.ready, info.message], [false, 'doc has errors']);
  await new Promise((r) => setTimeout(r, 150));
  assert.equal(restarted, false);
});

test('a missing binary is a failure with a message', async () => {
  const p = new LiveProcess({ command: '/nonexistent/engines-live', args: [], cwd: os.tmpdir() });
  const exit = next<ExitInfo>(p, 'exit');
  p.start();
  const info = await exit;
  assert.equal(info.ready, false);
  assert.match(info.message ?? '', /ENOENT/);
});

/** A held pad at amplitude `level` (written as its `level_db`). */
const DOC = (level: number) => `{
  schema_version: "0.9",
  transport: { bpm: 240, master_steps: 4 },
  harmony: { root: "D", mode: "dorian" },
  global: { master_gain_db: -6.02 },
  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: "saw_soft", level_db: ${(20 * Math.log10(level)).toFixed(2)}, cutoff_hz: 800, sustain: 1.0 } } },
  modulators: { seq: { type: "step_seq", output: "note", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 0 } } }, { step: 2, voicing: { v1: { offset: 7 } } } ] } },
  matrix: { m: { source: "seq", dest: "instruments.pad.notes", kind: "note", voices: 1 } },
}
`;

test('the real engines-live: ready → edit applied → refused → quit', { skip: !fs.existsSync(ENGINES_LIVE) && `no ${ENGINES_LIVE} (cargo build --release -p engines_live)` }, async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'engines-live-ext-'));
  fs.mkdirSync(path.join(root, 'pieces/demo'), { recursive: true });
  const doc = path.join(root, 'pieces/demo/demo.json5');
  fs.writeFileSync(doc, DOC(0.4));
  const p = new LiveProcess({ command: ENGINES_LIVE, args: [doc, '--stdio', '--headless'], cwd: root });
  try {
    const ready = next<any>(p, 'event', (e) => e.type === 'ready');
    p.start();
    const r = await ready;
    assert.deepEqual([r.piece, r.rev, r.instruments, r.key?.name], ['demo', 1, ['pad'], 'D dorian']);
    const tick = await next<any>(p, 'event', (e) => e.type === 'tick' && 'seq' in e.steps);
    assert.ok(tick.steps.seq >= 0 && tick.steps.seq < 4);
    const applied = next<any>(p, 'event', (e) => e.type === 'applied' && e.rev === 2);
    const accepted = next<any>(p, 'event', (e) => e.type === 'accepted');
    p.send({ type: 'doc', text: DOC(0.3), version: 2, source: 'editor' });
    assert.equal((await accepted).version, 2);
    assert.equal((await applied).kind, 'value');
    const rejected = next<any>(p, 'event', (e) => e.type === 'rejected');
    p.send({ type: 'doc', text: DOC(0.3).replace('cutoff', 'cutof'), version: 3 });
    assert.match((await rejected).errors[0].path, /cutof_hz$/);
    const exit = next<ExitInfo>(p, 'exit');
    await p.stop();
    assert.equal((await exit).expected, true);
    const status = JSON.parse(fs.readFileSync(path.join(root, 'pieces/demo/.live/status.json'), 'utf8'));
    assert.equal(status.state, 'stopped');
  } finally {
    await p.stop();
    fs.rmSync(root, { recursive: true, force: true });
  }
});
