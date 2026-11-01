// The shipped skills: the manifest declares the repo's three portable skills, packaging copies
// them, and activation points each one's marked line at the extension's tools. `npm test` (node:test).

import * as assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { test } from 'node:test';
import { BIN_CLOSE, BIN_OPEN, binLine, rewriteSkills, SHIPPED_SKILLS, toolsDir, withBinDir } from '../src/skills';

const extRoot = path.join(__dirname, '..', '..');
const repoSkills = path.join(extRoot, '..', '..', 'skills');
const manifest = JSON.parse(fs.readFileSync(path.join(extRoot, 'package.json'), 'utf8'));

function tmp(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'engines-skills-'));
}

test('the manifest declares the three portable skills, from VS Code 1.109 (chatSkills)', () => {
  assert.deepEqual(
    manifest.contributes.chatSkills,
    SHIPPED_SKILLS.map((n) => ({ path: `./skills/${n}/SKILL.md` })),
  );
  assert.equal(manifest.engines.vscode, '^1.109.0');
  assert.match(manifest.devDependencies['@types/vscode'], /1\.109/);
  for (const n of SHIPPED_SKILLS) {
    const skill = fs.readFileSync(path.join(repoSkills, n, 'SKILL.md'), 'utf8');
    assert.match(skill, new RegExp(`^---\\nname: ${n}\\n`), `skills/${n}: its folder is its name`);
    assert.equal(skill.split(BIN_OPEN).length, 2, `skills/${n}: one marked line`);
  }
});

test('packaging copies the skills from the repo, references and all', () => {
  execFileSync(process.execPath, [path.join(extRoot, 'scripts', 'copy-skills.mjs')], { cwd: extRoot });
  for (const n of SHIPPED_SKILLS) {
    for (const f of ['SKILL.md', 'reference/engine-facts.md', 'reference/music-theory.md']) {
      const copy = path.join(extRoot, 'skills', n, f);
      assert.deepEqual(fs.readFileSync(copy), fs.readFileSync(path.join(repoSkills, n, f)), copy);
    }
  }
  assert.ok(fs.existsSync(path.join(extRoot, 'skills', 'engines-new', 'reference', 'skeleton.json5')));
});

test('the marked line is rewritten to name the tools, and only it', () => {
  const src = `# New piece\n\n${BIN_OPEN}The \`engines-*\` commands are on PATH.${BIN_CLOSE}\n\nRun \`engines-check\`.\n`;
  const out = withBinDir(src, '/opt/engines/bin');
  assert.equal(out, `# New piece\n\n${binLine('/opt/engines/bin')}\n\nRun \`engines-check\`.\n`);
  assert.match(out, /`\/opt\/engines\/bin\/engines-check`/);
  assert.equal(withBinDir(out, '/opt/engines/bin'), out, 'idempotent');
  assert.equal(withBinDir('no marker here', '/x'), 'no marker here');
  assert.equal(withBinDir(`${BIN_OPEN} unclosed`, '/x'), `${BIN_OPEN} unclosed`);
});

test("activation rewrites the extension's own copies, once", () => {
  const dir = tmp();
  for (const n of SHIPPED_SKILLS) {
    fs.mkdirSync(path.join(dir, n));
    fs.writeFileSync(path.join(dir, n, 'SKILL.md'), `x\n${BIN_OPEN}on PATH${BIN_CLOSE}\ny\n`);
  }
  const changed = rewriteSkills(dir, SHIPPED_SKILLS, '/ext/bin');
  assert.equal(changed.length, 3);
  assert.ok(fs.readFileSync(path.join(dir, 'engines-song', 'SKILL.md'), 'utf8').includes('/ext/bin/engines-check'));
  assert.deepEqual(rewriteSkills(dir, SHIPPED_SKILLS, '/ext/bin'), [], 'nothing left to change');
  assert.deepEqual(rewriteSkills(dir, ['missing'], '/ext/bin'), [], 'a missing skill is skipped');
});

test("the tools' folder: the bundled bin/ first, else where engines-live resolved", () => {
  const ext = tmp();
  assert.equal(toolsDir(ext, undefined), undefined);
  assert.equal(toolsDir(ext, '/repo/target/release/engines-live'), '/repo/target/release');
  fs.mkdirSync(path.join(ext, 'bin'));
  fs.writeFileSync(path.join(ext, 'bin', 'engines-check'), '');
  assert.equal(toolsDir(ext, '/repo/target/release/engines-live'), path.join(ext, 'bin'));
});
