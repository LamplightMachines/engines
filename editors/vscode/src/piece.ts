// Pieces (ADR 0034): `pieces/<slug>/<slug>.json5` is a piece's doc; `.live/` beside it holds the
// player's status and history. Mirrors `engines_core::piece::Piece::of_doc`.

import * as fs from 'node:fs';
import * as path from 'node:path';

export interface Piece {
  slug: string;
  /** `pieces/<slug>` */
  dir: string;
  /** `pieces/<slug>/<slug>.json5` */
  doc: string;
  /** The repo root (the folder holding `pieces/`, where `target/release/` lives). */
  root: string;
}

const SLUG = /^[A-Za-z0-9_-]+$/;

/** The piece whose main doc is `file`, or `undefined` for any other file (history snapshots too). */
export function pieceOfDoc(file: string): Piece | undefined {
  const dir = path.dirname(file);
  const slug = path.basename(dir);
  const pieces = path.dirname(dir);
  if (path.basename(pieces) !== 'pieces' || !SLUG.test(slug)) {
    return undefined;
  }
  if (path.basename(file) !== `${slug}.json5`) {
    return undefined;
  }
  return { slug, dir, doc: file, root: path.dirname(pieces) };
}

/**
 * The pid of an engines-live already playing the piece in `dir` (a terminal `make live`): its
 * `.live/status.json` says playing or paused and that process exists. `undefined` otherwise.
 * `probe` is `process.kill(pid, 0)`: it throws `ESRCH` for a pid that's gone (stale status), but
 * `EPERM` for one that exists under another user — that's still a player.
 */
export function livePlayer(dir: string, probe: (pid: number) => void = (pid) => void process.kill(pid, 0)): number | undefined {
  let status: { pid?: unknown; state?: unknown } | null;
  try {
    status = JSON.parse(fs.readFileSync(path.join(dir, '.live', 'status.json'), 'utf8'));
  } catch {
    return undefined;
  }
  const pid = status?.pid;
  if (typeof pid !== 'number' || pid <= 0 || (status?.state !== 'playing' && status?.state !== 'paused')) {
    return undefined;
  }
  try {
    probe(pid);
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== 'EPERM') {
      return undefined; // stale status from a player that's gone
    }
  }
  return pid;
}

/** Every piece under `root/pieces/`, by slug. */
export function listPieces(root: string): Piece[] {
  const base = path.join(root, 'pieces');
  let names: string[];
  try {
    names = fs.readdirSync(base);
  } catch {
    return [];
  }
  return names
    .sort()
    .map((slug) => pieceOfDoc(path.join(base, slug, `${slug}.json5`)))
    .filter((p): p is Piece => p !== undefined && fs.existsSync(p.doc));
}
