// The free build: the packaged VSIX names no Live set, MIDI or Ableton feature — those come
// from Engines Studio through the companion API. Runs on `engines.vsix` (`npm run package`; `make
// vscode-test` packages it first), skipped when there is none. The shipped skills (`extension/skills/`)
// are left out: they name Studio's skills in conditional hand-offs, and
// crates/engines_authoring/tests/skills.rs holds every such mention to a passage that says so.

import * as assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { test } from 'node:test';

const VSIX = path.join(__dirname, '..', '..', 'engines.vsix');
const WORDS = [/Ableton/, /Live set/, /engines-als/, /engines-midi/, /\bMIDI\b/, /toggleMidi/, /midi_mirror/, /buildLiveSet/];

test('the packaged VSIX names no Live set, MIDI or Ableton feature', { skip: !fs.existsSync(VSIX) && 'no engines.vsix (npm run package)' }, () => {
  const files = execFileSync('unzip', ['-Z1', VSIX]).toString().split('\n').filter((f) => f.startsWith('extension/') && !f.startsWith('extension/node_modules/') && !f.startsWith('extension/skills/') && /\.(js|json|css|md|html|txt)$/.test(f));
  assert.ok(files.some((f) => f.endsWith('out/src/extension.js')), `the VSIX holds the extension: ${files.join(', ')}`);
  for (const f of files) {
    const text = execFileSync('unzip', ['-p', VSIX, f], { maxBuffer: 64 << 20 }).toString();
    for (const w of WORDS) {
      assert.doesNotMatch(text, w, `${f} mentions ${w}`);
    }
  }
});
