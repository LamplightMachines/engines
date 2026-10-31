// `npm run package`: runs `vsce package` with the extension's runtime dependencies read from
// package-lock.json instead of `npm list`. npm 11 masks anything shaped like a UUID in what it
// prints (@npmcli/redact), so in a checkout whose path holds one (a Claude Code scratchpad, say)
// `npm list --parseable` prints `…/***/…`, vsce globs folders that don't exist and packages nothing:
// "Extension entrypoint(s) missing". The lock file lists the same folders, unmasked.

import * as fs from 'node:fs';
import * as path from 'node:path';

interface LockPackage {
  dev?: boolean;
  devOptional?: boolean;
}

/** The extension's folder and every installed non-dev package's, as `npm list --omit=dev --parseable --all` prints them. */
export function runtimeDirs(root: string): string[] {
  const lock = JSON.parse(fs.readFileSync(path.join(root, 'package-lock.json'), 'utf8')) as { lockfileVersion: number; packages?: Record<string, LockPackage> };
  if (!lock.packages) {
    throw new Error(`package-lock.json v${lock.lockfileVersion} has no "packages": run npm install with npm 7 or later`);
  }
  const dirs = Object.entries(lock.packages)
    .filter(([key, p]) => key !== '' && !p.dev && !p.devOptional)
    .map(([key]) => path.join(root, key))
    .filter((dir) => fs.existsSync(dir)); // an optional dependency may not be installed
  return [root, ...dirs];
}

if (require.main === module) {
  // vsce has no option for this, so swap the function its npm path calls (`out/npm.js`, vsce 4).
  const npm = require('@vscode/vsce/out/npm') as {
    getDependencies?: (cwd: string, dependencies?: string, packaged?: string[]) => Promise<string[]>;
    detectYarn: (cwd: string) => Promise<boolean>;
  };
  const original = npm.getDependencies;
  if (typeof original !== 'function') {
    throw new Error('@vscode/vsce/out/npm has no getDependencies any more: update dev/package.ts for this vsce');
  }
  npm.getDependencies = async (cwd, dependencies, packaged) =>
    dependencies === 'npm' || (dependencies === undefined && !(await npm.detectYarn(cwd))) ? runtimeDirs(cwd) : original(cwd, dependencies, packaged);
  (require('@vscode/vsce/out/main') as (argv: string[]) => void)([process.argv[0], 'vsce', 'package', ...process.argv.slice(2)]);
}
