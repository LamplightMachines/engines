// The shipped skills for Claude Code. Claude Code doesn't read `chatSkills`: it finds skills
// in a folder's `.claude/skills/` (and `~/.claude/skills/`, which outranks a project's and would hide a
// checkout's own linked skills, so it isn't used). When the Claude Code extension is installed and the
// opened folder is trusted and isn't a checkout, the extension copies its skills there — their marked
// line already naming its tools (skills.ts) — and refreshes them each time it starts.
// A copy carries a stamp file. Only a stamped folder is ever refreshed: a skill of the same name the
// user wrote, a link, or a copy whose stamp they deleted is left alone.
// Plain node: no `vscode` import, so `npm test` covers it.

import * as fs from 'node:fs';
import * as path from 'node:path';

/** The Claude Code extension's id. */
export const CLAUDE_CODE_ID = 'anthropic.claude-code';

/** The stamp a copy carries. */
export const STAMP = '.engines-copy';

/** Where a copy is written in full before it replaces the old one (outside `skills/`, so no agent reads a
 *  half-written skill); removed once done. */
export const STAGING = '.engines-staging';

/** `<folder>/.claude` or `<folder>/.claude/skills`, whichever is a link: copying through it would write
 *  outside the folder (into a shared or personal skills folder, say), so nothing is. */
export function linkedSkillsDir(folder: string): string | undefined {
  for (const p of [path.join(folder, '.claude'), path.join(folder, '.claude', 'skills')]) {
    if (fs.lstatSync(p, { throwIfNoEntry: false })?.isSymbolicLink()) {
      return p;
    }
  }
  return undefined;
}

/** Why the skills are, or aren't, copied into `folder`. */
export function shareReason(o: { folder: string | undefined; trusted: boolean; claudeCode: boolean; enabled: boolean; checkout: boolean; linked?: string }): string | undefined {
  if (!o.enabled) {
    return 'turned off in Settings';
  }
  if (!o.claudeCode) {
    return 'the Claude Code extension isn\'t installed';
  }
  if (!o.folder) {
    return 'no folder is open';
  }
  if (!o.trusted) {
    return 'the folder isn\'t trusted';
  }
  if (o.checkout) {
    return 'the folder is a checkout, which links its own skills';
  }
  if (o.linked) {
    return `${o.linked} is a link, and nothing is copied through it`;
  }
  return undefined;
}

export type Outcome = 'copied' | 'refreshed' | 'current' | 'kept';

/** The stamp's text: who wrote the copy, and how to keep it. */
export function stampText(by: string): string {
  return `Copied here by ${by} for Claude Code, which reads .claude/skills/. It refreshes this folder each time it starts.\nDelete this file to keep your own edits: the folder is then left alone.\n`;
}

/** Every file under `dir`, as relative path → contents. */
function files(dir: string, rel = ''): Map<string, Buffer> {
  const out = new Map<string, Buffer>();
  for (const e of fs.readdirSync(path.join(dir, rel), { withFileTypes: true })) {
    const r = path.join(rel, e.name);
    if (e.isDirectory()) {
      for (const [k, v] of files(dir, r)) {
        out.set(k, v);
      }
    } else if (e.isFile()) {
      out.set(r, fs.readFileSync(path.join(dir, r)));
    }
  }
  return out;
}

function same(a: Map<string, Buffer>, b: Map<string, Buffer>): boolean {
  return a.size === b.size && [...a].every(([k, v]) => b.get(k)?.equals(v) === true);
}

/** Copy each `<skillsDir>/<name>/` into `<folder>/.claude/skills/<name>/`, stamped as `by`'s. A missing
 *  source is skipped, and nothing is copied through a linked `.claude/` or `.claude/skills/`. A copy is
 *  written in full under STAGING first, then swapped in; a failed write leaves the old copy as it was.
 *  Returns what happened to each skill. */
export function copyForClaude(skillsDir: string, names: readonly string[], folder: string, by: string): { name: string; outcome: Outcome }[] {
  const results: { name: string; outcome: Outcome }[] = [];
  if (linkedSkillsDir(folder)) {
    return results;
  }
  const staging = path.join(folder, '.claude', STAGING);
  try {
    for (const name of names) {
      const outcome = copyOne(skillsDir, name, folder, by, staging);
      if (outcome) {
        results.push({ name, outcome });
      }
    }
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }
  return results;
}

/** One skill's copy (see copyForClaude); `undefined` when the extension doesn't have it. */
function copyOne(skillsDir: string, name: string, folder: string, by: string, staging: string): Outcome | undefined {
  const src = path.join(skillsDir, name);
  if (!fs.existsSync(path.join(src, 'SKILL.md'))) {
    return undefined;
  }
  const want = files(src);
  want.set(STAMP, Buffer.from(stampText(by)));
  const dst = path.join(folder, '.claude', 'skills', name);
  const there = fs.lstatSync(dst, { throwIfNoEntry: false });
  if (there && (!there.isDirectory() || !fs.existsSync(path.join(dst, STAMP)))) {
    return 'kept';
  }
  if (there && same(files(dst), want)) {
    return 'current';
  }
  const fresh = path.join(staging, name);
  const old = path.join(staging, `${name}.old`);
  fs.rmSync(fresh, { recursive: true, force: true });
  for (const [rel, data] of want) {
    fs.mkdirSync(path.dirname(path.join(fresh, rel)), { recursive: true });
    fs.writeFileSync(path.join(fresh, rel), data);
  }
  fs.mkdirSync(path.dirname(dst), { recursive: true });
  if (there) {
    fs.renameSync(dst, old);
  }
  try {
    fs.renameSync(fresh, dst);
  } catch (e) {
    if (there) {
      fs.renameSync(old, dst);
    }
    throw e;
  }
  return there ? 'refreshed' : 'copied';
}
