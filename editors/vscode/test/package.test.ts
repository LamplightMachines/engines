// `npm run package` reads the runtime dependencies from package-lock.json (dev/package.ts): the
// same folders `npm list` gives vsce, even where npm would mask a UUID in the checkout's path.

import * as assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import * as path from 'node:path';
import { test } from 'node:test';
import { runtimeDirs } from '../dev/package';

const ROOT = path.join(__dirname, '..', '..');
const UUID = /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi; // as @npmcli/redact masks it

test('the lock file names the folders npm list does', () => {
  const npm = execFileSync('npm', ['list', '--omit=dev', '--parseable', '--all', '--loglevel=error'], { cwd: ROOT }).toString().split(/\r?\n/).filter((l) => path.isAbsolute(l));
  const ours = runtimeDirs(ROOT).map((d) => d.replace(UUID, '***'));
  assert.deepEqual([...ours].sort(), [...npm].sort());
});

test('the runtime folders hold the language client and no dev tool', () => {
  const names = runtimeDirs(ROOT).map((d) => path.relative(ROOT, d));
  assert.equal(names[0], '');
  assert.ok(names.includes(path.join('node_modules', 'vscode-languageclient')));
  for (const dev of ['@vscode/vsce', 'typescript', '@vscode/test-electron']) {
    assert.ok(!names.includes(path.join('node_modules', dev)), `${dev} is a dev dependency`);
  }
});
