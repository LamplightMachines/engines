// The skills' copy for Claude Code: when it's made, and that only a stamped copy is ever
// refreshed. `npm test` (node:test).

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { test } from 'node:test';
import { copyForClaude, linkedSkillsDir, shareReason, STAGING, STAMP, stampText } from '../src/claudeSkills';
import { binLine, rewriteSkills, SHIPPED_SKILLS } from '../src/skills';

function tmp(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'engines-claude-'));
}

/** A shipped skills folder like the VSIX's: SKILL.md with a marked line, plus a reference file. */
function shipped(): string {
  const dir = tmp();
  for (const n of SHIPPED_SKILLS) {
    fs.mkdirSync(path.join(dir, n, 'reference'), { recursive: true });
    fs.writeFileSync(path.join(dir, n, 'SKILL.md'), `---\nname: ${n}\n---\n<!-- engines:bin -->in a checkout<!-- /engines:bin -->\n`);
    fs.writeFileSync(path.join(dir, n, 'reference', 'engine-facts.md'), `facts for ${n}\n`);
  }
  return dir;
}

const BY = 'the Engines extension (LamplightMachines.engines)';
const base = { folder: '/f', trusted: true, claudeCode: true, enabled: true, checkout: false };

test('the skills are shared only with Claude Code installed, in a trusted folder that isn\'t a checkout', () => {
  assert.equal(shareReason(base), undefined);
  assert.match(shareReason({ ...base, claudeCode: false }) ?? '', /Claude Code extension isn't installed/);
  assert.match(shareReason({ ...base, trusted: false }) ?? '', /isn't trusted/);
  assert.match(shareReason({ ...base, checkout: true }) ?? '', /checkout/);
  assert.match(shareReason({ ...base, folder: undefined }) ?? '', /no folder/);
  assert.match(shareReason({ ...base, enabled: false }) ?? '', /turned off/);
  assert.match(shareReason({ ...base, linked: '/f/.claude/skills' }) ?? '', /is a link/);
});

test('nothing is copied through a linked .claude/ or .claude/skills/: it would land outside the folder', () => {
  const skills = shipped();
  for (const linkAt of [['.claude'], ['.claude', 'skills']]) {
    const folder = tmp();
    const elsewhere = tmp();
    fs.mkdirSync(path.join(folder, ...linkAt.slice(0, -1)), { recursive: true });
    fs.symlinkSync(elsewhere, path.join(folder, ...linkAt));
    assert.equal(linkedSkillsDir(folder), path.join(folder, ...linkAt));
    assert.deepEqual(copyForClaude(skills, SHIPPED_SKILLS, folder, BY), []);
    assert.deepEqual(fs.readdirSync(elsewhere), [], `${linkAt.join('/')}: the link's target is untouched`);
  }
  assert.equal(linkedSkillsDir(tmp()), undefined);
});

test('a refresh that fails partway leaves the old copy whole, stamp and all; the next start finishes it', () => {
  const skills = shipped();
  const folder = tmp();
  rewriteSkills(skills, SHIPPED_SKILLS, '/ext-0.1.0/bin');
  copyForClaude(skills, SHIPPED_SKILLS, folder, BY);
  assert.ok(!fs.existsSync(path.join(folder, '.claude', STAGING)), 'the staging folder is gone after a copy');
  rewriteSkills(skills, SHIPPED_SKILLS, '/ext-0.2.0/bin');
  fs.writeFileSync(path.join(folder, '.claude', STAGING), 'in the way'); // the new copy can't be written
  assert.throws(() => copyForClaude(skills, SHIPPED_SKILLS, folder, BY));
  for (const n of SHIPPED_SKILLS) {
    const dst = path.join(folder, '.claude', 'skills', n);
    assert.ok(fs.existsSync(path.join(dst, STAMP)), `${n}: still stamped, so it's still refreshed later`);
    assert.ok(fs.readFileSync(path.join(dst, 'SKILL.md'), 'utf8').includes('/ext-0.1.0/bin'), `${n}: the old copy, whole`);
  }
  assert.ok(copyForClaude(skills, SHIPPED_SKILLS, folder, BY).every((x) => x.outcome === 'refreshed'));
  assert.ok(fs.readFileSync(path.join(folder, '.claude', 'skills', 'engines-new', 'SKILL.md'), 'utf8').includes('/ext-0.2.0/bin'));
});

test('a fresh folder gets each skill, references and stamp included, with the tools line already filled in', () => {
  const skills = shipped();
  const folder = tmp();
  rewriteSkills(skills, SHIPPED_SKILLS, '/ext/bin');
  const r = copyForClaude(skills, SHIPPED_SKILLS, folder, BY);
  assert.deepEqual(r, SHIPPED_SKILLS.map((name) => ({ name, outcome: 'copied' })));
  for (const n of SHIPPED_SKILLS) {
    const dst = path.join(folder, '.claude', 'skills', n);
    assert.ok(fs.readFileSync(path.join(dst, 'SKILL.md'), 'utf8').includes(binLine('/ext/bin')), `${n}: the copy names the tools`);
    assert.equal(fs.readFileSync(path.join(dst, 'reference', 'engine-facts.md'), 'utf8'), `facts for ${n}\n`);
    assert.equal(fs.readFileSync(path.join(dst, STAMP), 'utf8'), stampText(BY));
  }
  assert.deepEqual(fs.readdirSync(folder), ['.claude'], 'nothing else is written');
});

test('a second start finds the copies current; a new version (another tools folder) refreshes them, stale files gone', () => {
  const skills = shipped();
  const folder = tmp();
  copyForClaude(skills, SHIPPED_SKILLS, folder, BY);
  assert.ok(copyForClaude(skills, SHIPPED_SKILLS, folder, BY).every((x) => x.outcome === 'current'));
  const stale = path.join(folder, '.claude', 'skills', 'engines-new', 'reference', 'old.md');
  fs.writeFileSync(stale, 'from an older version');
  rewriteSkills(skills, SHIPPED_SKILLS, '/ext-0.2.0/bin');
  assert.ok(copyForClaude(skills, SHIPPED_SKILLS, folder, BY).every((x) => x.outcome === 'refreshed'));
  assert.ok(!fs.existsSync(stale), 'a file the new version lacks is removed');
  assert.ok(fs.readFileSync(path.join(folder, '.claude', 'skills', 'engines-song', 'SKILL.md'), 'utf8').includes('/ext-0.2.0/bin'));
});

test('a skill the user wrote, a link, or a copy whose stamp was deleted is left alone', () => {
  const skills = shipped();
  const folder = tmp();
  const own = path.join(folder, '.claude', 'skills', 'engines-new');
  fs.mkdirSync(own, { recursive: true });
  fs.writeFileSync(path.join(own, 'SKILL.md'), 'my own');
  fs.symlinkSync(path.join(skills, 'engines-edit'), path.join(folder, '.claude', 'skills', 'engines-edit'));
  copyForClaude(skills, ['engines-song'], folder, BY);
  const edited = path.join(folder, '.claude', 'skills', 'engines-song');
  fs.rmSync(path.join(edited, STAMP));
  fs.writeFileSync(path.join(edited, 'SKILL.md'), 'edited by hand');

  assert.deepEqual(copyForClaude(skills, SHIPPED_SKILLS, folder, BY).map((x) => x.outcome), ['kept', 'kept', 'kept']);
  assert.equal(fs.readFileSync(path.join(own, 'SKILL.md'), 'utf8'), 'my own');
  assert.ok(fs.lstatSync(path.join(folder, '.claude', 'skills', 'engines-edit')).isSymbolicLink());
  assert.ok(fs.existsSync(path.join(skills, 'engines-edit', 'SKILL.md')), 'the link\'s target is untouched');
  assert.equal(fs.readFileSync(path.join(edited, 'SKILL.md'), 'utf8'), 'edited by hand');
});

test('a skill missing from the extension is skipped', () => {
  const skills = shipped();
  fs.rmSync(path.join(skills, 'engines-song'), { recursive: true });
  const folder = tmp();
  assert.deepEqual(copyForClaude(skills, SHIPPED_SKILLS, folder, BY).map((x) => x.name), ['engines-new', 'engines-edit']);
  assert.ok(!fs.existsSync(path.join(folder, '.claude', 'skills', 'engines-song')));
});
