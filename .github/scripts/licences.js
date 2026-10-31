// The licence rules cargo-deny doesn't see (LICENSING.md; `make audit` runs it after
// `cargo deny check licenses`):
//   1. the extensions' runtime npm packages (the VSIX ships them) take a licence deny.toml allows, so
//      the npm side follows the same rule as the crates;
//   2. every manifest has a licence field, and none says UNLICENSED;
//   3. each extension's licence file is a copy of the root one (vsce packages only the extension's folder).
// `node .github/scripts/licences.js [root]` prints what's wrong and exits 1. `root` defaults to this repo; a repo
// that embeds this one (a companion) passes its own, with its own deny.toml and extensions.

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const ROOT = path.join(__dirname, '..', '..');
// npm-only additions to deny.toml's list: BlueOak-1.0.0 is a permissive licence no crate uses yet.
const NPM_EXTRA = ['BlueOak-1.0.0'];
const EXTENSIONS = ['editors/vscode', 'editors/studio'];

/** deny.toml's `[licenses] allow` list: one list for the crates and the npm packages. */
function allowedLicences(denyToml) {
  const section = /^\[licenses\]\s*$([\s\S]*?)(?=^\[)/m.exec(denyToml);
  const allow = section && /^allow\s*=\s*\[([\s\S]*?)\]/m.exec(section[1]);
  if (!allow) throw new Error('deny.toml has no [licenses] allow list');
  return [...allow[1].replace(/#.*$/gm, '').matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

/**
 * Whether an SPDX expression passes: OR needs one allowed side, AND needs both, and `X WITH e`
 * counts as X. A missing or unparsable licence fails.
 */
function licencePasses(expr, allowed) {
  if (typeof expr !== 'string' || !expr.trim()) return false;
  const tokens = expr.match(/\(|\)|[^\s()]+/g);
  let i = 0;
  const peek = () => tokens[i];
  const next = () => tokens[i++];
  const primary = () => {
    const t = next();
    if (t === '(') {
      const v = or();
      if (next() !== ')') throw new Error(`unbalanced: ${expr}`);
      return v;
    }
    if (t === undefined || t === ')' || /^(AND|OR|WITH)$/.test(t)) throw new Error(`unexpected ${t}: ${expr}`);
    const id = t.replace(/\+$/, '');
    if (peek() === 'WITH') {
      next();
      next();
    }
    return allowed.includes(id);
  };
  const and = () => {
    let v = primary();
    while (peek() === 'AND') {
      next();
      v = primary() && v;
    }
    return v;
  };
  const or = () => {
    let v = and();
    while (peek() === 'OR') {
      next();
      v = and() || v;
    }
    return v;
  };
  try {
    const v = or();
    return i === tokens.length && v;
  } catch {
    return false;
  }
}

/** The packages a lockfile installs at runtime (not dev): `[name, licence]`. */
function runtimePackages(lock) {
  return Object.entries(lock.packages ?? {})
    .filter(([key, p]) => key && !p.dev && !p.devOptional)
    .map(([key, p]) => [key.replace(/^.*node_modules\//, ''), p.license]);
}

/** The licence a manifest declares, or undefined. */
function manifestLicence(file, text) {
  if (file.endsWith('package.json')) return JSON.parse(text).license;
  // Cargo.toml / pyproject.toml: `license = "…"` or a workspace's `license.workspace = true`.
  const m = /^license\s*=\s*"([^"]*)"/m.exec(text) ?? /^license\.workspace\s*=\s*(true)/m.exec(text);
  return m?.[1] === 'true' ? '(the workspace)' : m?.[1];
}

/** Every problem in the repo at `root`, as strings. */
function problems(root = ROOT) {
  const out = [];
  const allowed = [...allowedLicences(fs.readFileSync(path.join(root, 'deny.toml'), 'utf8')), ...NPM_EXTRA];

  for (const ext of EXTENSIONS) {
    const lockFile = path.join(root, ext, 'package-lock.json');
    if (!fs.existsSync(lockFile)) continue; // Studio is absent in the open set.
    for (const [name, licence] of runtimePackages(JSON.parse(fs.readFileSync(lockFile, 'utf8')))) {
      if (!licencePasses(licence, allowed)) out.push(`${ext}: the runtime package ${name} is ${licence ?? 'unlicensed'}, which deny.toml doesn't allow`);
    }
  }

  const manifests = execFileSync('git', ['ls-files', '--', '*Cargo.toml', '*package.json', '*pyproject.toml'], { cwd: root })
    .toString()
    .split('\n')
    .filter((f) => f && fs.existsSync(path.join(root, f)));
  for (const f of manifests) {
    const text = fs.readFileSync(path.join(root, f), 'utf8');
    // A virtual workspace manifest has no package of its own: its licence is `[workspace.package]`'s.
    const licence = manifestLicence(f, text);
    if (!licence) out.push(`${f}: no licence field`);
    else if (/UNLICENSED/i.test(licence)) out.push(`${f}: says ${licence}; the closed side is LicenseRef-Proprietary (LICENSING.md)`);
  }

  // vsce packages only an extension's own folder, so each carries a copy of its licence.
  for (const [copy, original, needed] of [
    ['editors/vscode/LICENSE', 'LICENSE', fs.existsSync(path.join(root, 'editors/vscode'))],
    ['editors/studio/LICENSE-PROPRIETARY', 'LICENSE-PROPRIETARY', fs.existsSync(path.join(root, 'editors/studio'))],
  ]) {
    const file = path.join(root, copy);
    if (needed && (!fs.existsSync(file) || !fs.readFileSync(file).equals(fs.readFileSync(path.join(root, original))))) {
      out.push(`${copy}: not a copy of the root ${original} (cp ${original} ${copy})`);
    }
  }
  return out;
}

if (require.main === module) {
  const found = problems(process.argv[2] ? path.resolve(process.argv[2]) : ROOT);
  for (const p of found) console.error(`licences: ${p}`);
  if (found.length) process.exit(1);
  console.log('licences ok: npm runtime packages, manifest fields, the extensions\' licence files');
}

module.exports = { allowedLicences, licencePasses, runtimePackages, manifestLicence, problems };
