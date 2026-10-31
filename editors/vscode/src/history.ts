// A piece's revision history (ADR 0034): `.live/history/index.jsonl` lists every revision the doc
// went through (content-addressed, shared by engines-live and engines-song); each snapshot sits beside it.

import * as fs from 'node:fs';
import * as path from 'node:path';

export interface Revision {
  rev: number;
  hash: string;
  /** The snapshot's file name inside `history/`. */
  file: string;
  /** Unix seconds. */
  at: number;
  /** Which tool took it (`engines-live`, `engines-song`, …). */
  source: string;
}

export function historyDir(pieceDir: string): string {
  return path.join(pieceDir, '.live', 'history');
}

/** Every revision, oldest first (an unreadable line is skipped, a missing index is empty). */
export function readHistory(pieceDir: string): Revision[] {
  let text: string;
  try {
    text = fs.readFileSync(path.join(historyDir(pieceDir), 'index.jsonl'), 'utf8');
  } catch {
    return [];
  }
  const out: Revision[] = [];
  for (const line of text.split('\n')) {
    if (!line.trim()) {
      continue;
    }
    try {
      const r = JSON.parse(line);
      if (typeof r.rev === 'number' && typeof r.file === 'string') {
        out.push({ rev: r.rev, hash: String(r.hash ?? ''), file: r.file, at: Number(r.at ?? 0), source: String(r.source ?? '') });
      }
    } catch {
      // a torn last line while a tool is writing — the next read has it
    }
  }
  return out;
}

/** The loop-doc schema this build reads (ADR 0054). */
export const SCHEMA_VERSION = '0.9';

/** The `schema_version` a doc's text declares (`"0.8"`), read without parsing it; `undefined` when it
 *  names none. */
export function schemaVersion(text: string): string | undefined {
  return /["']?schema_version["']?\s*:\s*["']([^"']*)["']/.exec(text)?.[1];
}

/** The path of revision `rev`'s snapshot, if it's in the history. */
export function revisionPath(pieceDir: string, rev: number): string | undefined {
  const r = readHistory(pieceDir).find((x) => x.rev === rev);
  return r && path.join(historyDir(pieceDir), r.file);
}

/** `just now`, `5 min ago`, `3 h ago`, `2 d ago`. */
export function ago(at: number, now: number = Date.now() / 1000): string {
  const s = Math.max(0, now - at);
  if (s < 45) {
    return 'just now';
  }
  if (s < 3600) {
    return `${Math.round(s / 60)} min ago`;
  }
  if (s < 86400) {
    return `${Math.round(s / 3600)} h ago`;
  }
  return `${Math.round(s / 86400)} d ago`;
}
