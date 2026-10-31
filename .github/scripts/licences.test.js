// The licence rules outside cargo-deny: `node --test .github/scripts/licences.test.js`.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { allowedLicences, licencePasses, runtimePackages, manifestLicence, problems } = require('./licences.js');

const ALLOWED = ['MIT', 'Apache-2.0', 'ISC', 'BlueOak-1.0.0'];

test('the allow list is read from deny.toml', () => {
  const list = allowedLicences(fs.readFileSync(path.join(__dirname, '..', '..', 'deny.toml'), 'utf8'));
  assert.ok(list.includes('MIT') && list.includes('MPL-2.0'), list.join(', '));
  assert.ok(!list.some((l) => /GPL/.test(l)), `no GPL family in the list: ${list.join(', ')}`);
});

test('an SPDX expression passes when OR has an allowed side and AND has both', () => {
  assert.equal(licencePasses('MIT', ALLOWED), true);
  assert.equal(licencePasses('(MIT OR GPL-3.0-only)', ALLOWED), true);
  assert.equal(licencePasses('MIT AND ISC', ALLOWED), true);
  assert.equal(licencePasses('Apache-2.0 WITH LLVM-exception', ALLOWED), true);
  assert.equal(licencePasses('BlueOak-1.0.0', ALLOWED), true);
});

test('GPL, LGPL-only, AGPL, UNLICENSED and a missing licence fail', () => {
  for (const bad of ['GPL-3.0-only', 'LGPL-2.1-only', 'AGPL-3.0-or-later', 'MIT AND GPL-2.0-or-later', 'UNLICENSED', '', undefined, '(MIT']) {
    assert.equal(licencePasses(bad, ALLOWED), false, String(bad));
  }
});

test('only the runtime packages of a lockfile are checked', () => {
  const lock = {
    packages: {
      '': { license: 'GPL-3.0-or-later' },
      'node_modules/semver': { license: 'ISC' },
      'node_modules/typescript': { dev: true, license: 'Apache-2.0' },
      'node_modules/a/node_modules/b': { license: 'MIT' },
      'node_modules/fsevents': { devOptional: true, license: 'MIT' },
    },
  };
  assert.deepEqual(runtimePackages(lock), [['semver', 'ISC'], ['b', 'MIT']]);
});

test('a manifest licence is read from package.json, Cargo.toml and pyproject.toml', () => {
  assert.equal(manifestLicence('a/package.json', '{"license": "GPL-3.0-or-later"}'), 'GPL-3.0-or-later');
  assert.equal(manifestLicence('a/Cargo.toml', '[package]\nname = "x"\nlicense.workspace = true\n'), '(the workspace)');
  assert.equal(manifestLicence('a/Cargo.toml', '[package]\nlicense = "LicenseRef-Proprietary" # closed\n'), 'LicenseRef-Proprietary');
  assert.equal(manifestLicence('pyproject.toml', '[project]\nname = "x"\n'), undefined);
});

test('the repo passes', () => {
  assert.deepEqual(problems(), []);
});

test('a companion repo is checked from its own root: its extension, its manifests, no editors/vscode', () => {
  const os = require('node:os');
  const { execFileSync } = require('node:child_process');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'licences-companion-'));
  try {
    const write = (f, text) => {
      fs.mkdirSync(path.dirname(path.join(root, f)), { recursive: true });
      fs.writeFileSync(path.join(root, f), text);
    };
    write('deny.toml', '[licenses]\nallow = ["MIT"]\n\n[other]\n');
    write('LICENSE-PROPRIETARY', 'closed\n');
    write('editors/studio/LICENSE-PROPRIETARY', 'closed\n');
    write('editors/studio/package.json', '{"license": "SEE LICENSE IN LICENSE-PROPRIETARY"}');
    const lock = (licence) => JSON.stringify({ packages: { '': {}, 'node_modules/x': { license: licence } } });
    write('editors/studio/package-lock.json', lock('MIT'));
    execFileSync('git', ['init', '-q'], { cwd: root });
    execFileSync('git', ['add', '-A'], { cwd: root });
    assert.deepEqual(problems(root), [], 'no editors/vscode, so no copy of a LICENSE is expected');
    write('editors/studio/package-lock.json', lock('GPL-3.0-only'));
    assert.deepEqual(problems(root), ['editors/studio: the runtime package x is GPL-3.0-only, which deny.toml doesn\'t allow']);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
