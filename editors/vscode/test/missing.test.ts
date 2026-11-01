// A missing tool: a checkout offers the build; an installed extension anywhere else says to
// reinstall, and never sends the user to the source. `npm test` (node:test).

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { test } from 'node:test';
import { isCheckout, missingTool } from '../src/missing';

test('in a checkout, a missing tool is a build away', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'engines-checkout-'));
  fs.mkdirSync(path.join(root, 'crates', 'engines_authoring'), { recursive: true });
  assert.ok(isCheckout(root));
  const m = missingTool('engines-lsp', root);
  assert.equal(m.build, true);
  assert.ok(m.text.includes(path.join(root, 'target', 'release', 'engines-lsp')), m.text);
});

test('in a fresh folder (or none), a missing tool means reinstall, with no build and no source', () => {
  const fresh = fs.mkdtempSync(path.join(os.tmpdir(), 'engines-fresh-'));
  for (const root of [fresh, undefined]) {
    assert.ok(!isCheckout(root));
    const m = missingTool('engines-live', root);
    assert.equal(m.build, false);
    assert.match(m.text, /reinstall the Engines extension/);
    assert.doesNotMatch(m.text, /target\/release|cargo|\bmake\b/);
  }
});
