// The rename's carry-over: the `sme.*` settings to copy to `engines.*` at each scope, and the
// `files.associations` entries that named `sme-loop`. `npm test` (node:test) — no VS Code needed.

import * as assert from 'node:assert/strict';
import { test } from 'node:test';
import { associationWrites, carriedOverMessage, doneId, oldKey, settingWrites, type Seen } from '../src/carryOver';

const seen = (key: string, scope: Seen['scope'], old: unknown, current?: unknown, folder?: string): Seen => ({ key, scope, folder, old, current });

test('oldKey: engines.* came from sme.*', () => {
  assert.equal(oldKey('engines.live.path'), 'sme.live.path');
  assert.equal(oldKey('engines.inlayHints.notes'), 'sme.inlayHints.notes');
});

test('settingWrites: each scope the user set an sme.* key at is copied to the same scope', () => {
  const writes = settingWrites(
    [
      seen('engines.live.path', 'global', '/bin/sme-live'),
      seen('engines.live.applyOn', 'workspace', 'edit'),
      seen('engines.live.headless', 'folder', true, undefined, 'file:///a'),
      seen('engines.live.headless', 'folder', undefined, undefined, 'file:///b'),
      seen('engines.trace.server', 'global', undefined),
    ],
    new Set(),
  );
  assert.deepEqual(writes, [
    { key: 'engines.live.path', scope: 'global', folder: undefined, value: '/bin/sme-live' },
    { key: 'engines.live.applyOn', scope: 'workspace', folder: undefined, value: 'edit' },
    { key: 'engines.live.headless', scope: 'folder', folder: 'file:///a', value: true },
  ]);
});

test('settingWrites: falsy values carry over too (false, 0, "")', () => {
  const writes = settingWrites([seen('engines.inlayHints.cycles', 'global', false), seen('engines.live.debounceMs', 'global', 0), seen('engines.live.outputDevice', 'workspace', '')], new Set());
  assert.deepEqual(writes.map((w) => w.value), [false, 0, '']);
});

test('settingWrites: an already-migrated profile copies nothing', () => {
  // The new key is set (the user, or an earlier carry-over): it wins.
  assert.deepEqual(settingWrites([seen('engines.live.path', 'global', '/old', '/new')], new Set()), []);
  // Carried over before and since cleared by the user: not copied again.
  const done = new Set([doneId({ key: 'engines.live.applyOn', scope: 'workspace' })]);
  assert.deepEqual(settingWrites([seen('engines.live.applyOn', 'workspace', 'edit')], done), []);
  // …but the same key at another scope still is.
  assert.equal(settingWrites([seen('engines.live.applyOn', 'global', 'edit')], done).length, 1);
});

test('associationWrites: entries naming sme-loop name engines-loop; the rest stay', () => {
  const writes = associationWrites([
    { scope: 'global', value: { '*.loop': 'sme-loop', '*.md': 'markdown' } },
    { scope: 'workspace', value: { '*.md': 'markdown' } },
    { scope: 'folder', folder: 'file:///a', value: { 'x/*.json5': 'sme-loop' } },
    { scope: 'workspace', value: undefined },
  ]);
  assert.deepEqual(writes, [
    { key: 'files.associations', scope: 'global', folder: undefined, value: { '*.loop': 'engines-loop', '*.md': 'markdown' } },
    { key: 'files.associations', scope: 'folder', folder: 'file:///a', value: { 'x/*.json5': 'engines-loop' } },
  ]);
  assert.deepEqual(associationWrites(writes.map((w) => ({ scope: w.scope, folder: w.folder, value: w.value }))), [], 'migrated: nothing left to do');
});

test('carriedOverMessage: said once, only when something moved', () => {
  assert.equal(carriedOverMessage(0, 0), undefined);
  assert.match(carriedOverMessage(1, 0) ?? '', /^Engines carried over 1 setting \(sme\.\* → engines\.\*\) from SME Loop Docs\./);
  assert.match(carriedOverMessage(3, 1) ?? '', /3 settings .* and 1 file association \(sme-loop → engines-loop\)/);
});
