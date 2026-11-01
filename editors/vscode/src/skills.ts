// The agent skills this extension ships: `/engines-new`, `/engines-edit` and
// `/engines-song`, copied from the repo's `skills/` when the VSIX is packaged and declared in
// `contributes.chatSkills`. Each SKILL.md has one marked line saying where the `engines-*` commands
// are. An agent's shell doesn't get this extension's PATH, and the agent reads a copy of
// the skill folder, so at activation — before an agent host copies the folder — the
// extension rewrites that line in its own copy to the folder it found its tools in.
// Plain node: no `vscode` import, so `npm test` covers it.

import * as fs from 'node:fs';
import * as path from 'node:path';

export const BIN_OPEN = '<!-- engines:bin -->';
export const BIN_CLOSE = '<!-- /engines:bin -->';

/** The skills this extension declares (folder = `name`, as `chatSkills` requires). */
export const SHIPPED_SKILLS = ['engines-new', 'engines-edit', 'engines-song'] as const;

/** The marked line naming `dir`. */
export function binLine(dir: string): string {
  const example = path.join(dir, 'engines-check');
  return `${BIN_OPEN}The \`engines-*\` commands are in \`${dir}\`: run them by that path (\`${example}\` …), whatever the PATH says.${BIN_CLOSE}`;
}

/** `text` with its marked line naming `dir`; unchanged when it has no marked line. */
export function withBinDir(text: string, dir: string): string {
  const a = text.indexOf(BIN_OPEN);
  const z = text.indexOf(BIN_CLOSE, a);
  if (a < 0 || z < 0) {
    return text;
  }
  return text.slice(0, a) + binLine(dir) + text.slice(z + BIN_CLOSE.length);
}

/** Point every `<skillsDir>/<name>/SKILL.md` at `dir`. Returns the files it changed (a file already
 *  naming `dir` is left alone). */
export function rewriteSkills(skillsDir: string, names: readonly string[], dir: string): string[] {
  const changed: string[] = [];
  for (const name of names) {
    const file = path.join(skillsDir, name, 'SKILL.md');
    let text: string;
    try {
      text = fs.readFileSync(file, 'utf8');
    } catch {
      continue;
    }
    const next = withBinDir(text, dir);
    if (next !== text) {
      fs.writeFileSync(file, next);
      changed.push(file);
    }
  }
  return changed;
}

/** Where the tools are: the extension's bundled `bin/` when it has one, else the folder holding the
 *  `engines-live` it resolved (a checkout's `target/release/`, or a configured path). */
export function toolsDir(extensionPath: string, resolvedLive: string | undefined): string | undefined {
  const bundled = path.join(extensionPath, 'bin');
  if (fs.existsSync(path.join(bundled, 'engines-check')) || fs.existsSync(path.join(bundled, 'engines-check.exe'))) {
    return bundled;
  }
  return resolvedLive ? path.dirname(resolvedLive) : undefined;
}
