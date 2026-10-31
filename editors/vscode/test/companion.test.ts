// The companion API driven with a fake companion: a strip action appears, changes state and
// runs; an output step changes the pairs choice and runs before the switch; a binary swap restarts the
// player; another major version is refused. And the open files stay free of what companions add.
// `npm test` (node:test) — no VS Code needed.

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { test } from 'node:test';
import { API_MAJOR, API_VERSION, Companions, refusal, type Host, type SwappableTool } from '../src/companion';
import { initialState, type LiveState } from '../src/live/state';

const PIECE = { slug: 'demo', dir: '/r/pieces/demo', doc: '/r/pieces/demo/demo.json5', root: '/r' };

function host(over: Partial<Host> = {}): Host & { swaps: SwappableTool[] } {
  const swaps: SwappableTool[] = [];
  return { piece: () => PIECE, player: () => ({ state: initialState(), ready: { type: 'ready', x_future: 1 }, status: undefined }), binaryChanged: (t) => swaps.push(t), swaps, ...over };
}

test('a companion built for another major version is refused, saying which to update', () => {
  const c = new Companions(host());
  assert.equal(c.connect('Studio', API_MAJOR).version, API_VERSION);
  assert.throws(() => c.connect('Studio', API_MAJOR + 1), /Studio is built for Engines API v2.*update Engines/);
  assert.throws(() => c.connect('Studio', API_MAJOR - 1), /update Studio/);
  assert.equal(refusal('Studio', API_MAJOR), undefined);
});

test('a strip action appears, changes state and runs; disposing it removes it', () => {
  const c = new Companions(host());
  const api = c.connect('Fake', API_MAJOR);
  let on = false;
  const ran: string[] = [];
  const fire: (() => void)[] = [];
  let redraws = 0;
  c.subscribe(() => redraws++);
  const reg = api.registerStripAction({
    id: 'fake',
    icon: 'M4 12h16',
    state: (s) => (s.id === 'bus' ? null : { on, title: `fake on ${s.id}` }),
    run: (s) => {
      ran.push(s.id);
      on = !on;
      fire.forEach((f) => f());
    },
    onDidChange: (l) => {
      fire.push(l);
      return { dispose: () => fire.splice(fire.indexOf(l), 1) };
    },
  });
  const st: LiveState = initialState();
  assert.deepEqual(c.stripActions('pad', st), [{ id: 'fake', icon: 'M4 12h16', on: false, blocked: false, title: 'fake on pad' }]);
  assert.deepEqual(c.stripActions('bus', st), [], 'null: no button on that strip');
  const before = redraws;
  assert.equal(c.runStripAction('fake', 'pad', st), true);
  assert.deepEqual(ran, ['pad']);
  assert.equal(c.stripActions('pad', st)[0].on, true, 'its new state');
  assert.ok(redraws > before, 'its onDidChange redraws the panel');
  assert.equal(c.runStripAction('nope', 'pad', st), false);
  reg.dispose();
  assert.deepEqual(c.stripActions('pad', st), [], 'unregistered: no button');
  assert.equal(fire.length, 0, 'its change listener is let go');
});

test('an action whose state throws shows nothing (the panel keeps drawing)', () => {
  const c = new Companions(host());
  c.connect('Fake', API_MAJOR).registerStripAction({ id: 'bad', icon: 'M0 0', state: () => { throw new Error('boom'); }, run: () => undefined });
  assert.deepEqual(c.stripActions('pad', initialState()), []);
});

test('an output step changes the pairs choice and runs before the switch', async () => {
  const c = new Companions(host());
  const ctx = { piece: PIECE, device: 'BlackHole 64ch', channels: 64 };
  assert.equal(c.pairsDescription(ctx, 'one stereo pair each'), 'one stereo pair each', 'no step: the open wording');
  const ran: boolean[] = [];
  const step = c.connect('Fake', API_MAJOR).registerOutputStep({ id: 'set', pairsDescription: (o) => `pairs for ${o.piece?.slug}`, beforeSwitch: async (o) => void ran.push(o.route) });
  assert.equal(c.pairsDescription(ctx, 'one stereo pair each'), 'pairs for demo');
  await c.beforeSwitch({ ...ctx, route: true });
  assert.deepEqual(ran, [true]);
  step.dispose();
  await c.beforeSwitch({ ...ctx, route: true });
  assert.deepEqual(ran, [true], 'gone: it no longer runs');
});

test('a binary swap restarts onto it, once per change', () => {
  const h = host();
  const c = new Companions(h);
  const api = c.connect('Fake', API_MAJOR);
  api.setBinary('engines-live', '/studio/engines-live-studio');
  api.setBinary('engines-live', '/studio/engines-live-studio');
  assert.deepEqual(h.swaps, ['engines-live'], 'the same path again: no restart');
  assert.equal(c.binary('engines-live'), '/studio/engines-live-studio');
  api.setBinary('engines-live', undefined);
  assert.deepEqual(h.swaps, ['engines-live', 'engines-live']);
  assert.equal(c.binary('engines-live'), undefined, 'back to the open player');
});

test('menu items, and read access to the piece and the raw ready', () => {
  const c = new Companions(host());
  const api = c.connect('Fake', API_MAJOR);
  api.registerMenuItem({ command: 'fake.build', title: 'Build…', where: 'panel' });
  assert.deepEqual(c.menu('panel').map((m) => m.command), ['fake.build']);
  assert.deepEqual(c.menu('statusBar'), []);
  assert.equal(api.piece()?.slug, 'demo');
  assert.equal(api.player().ready?.x_future, 1, 'a protocol field the open code doesn’t know reaches the companion');
  let n = 0;
  const sub = api.onDidChange(() => n++);
  c.changed();
  sub.dispose();
  c.changed();
  assert.equal(n, 1);
});

test('outside the registrations, the panel, the picker and binaries.ts know no Live set, MIDI or engines-als', () => {
  const root = path.join(__dirname, '..', '..');
  const open = ['src/panel/model.ts', 'src/panel/panel.ts', 'media/panel.js', 'media/panel.css', 'src/live/devices.ts', 'src/live/controller.ts', 'src/binaries.ts', 'src/companion.ts'];
  for (const f of open) {
    const text = fs.readFileSync(path.join(root, f), 'utf8');
    for (const word of [/Ableton/, /engines-als/, /Live set/, /\bMIDI\b/, /toggleMidi/, /midi_mirror/]) {
      assert.doesNotMatch(text, word, `${f} mentions ${word}`);
    }
  }
});
