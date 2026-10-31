// What the rename from SME to Engines carries over, editor-independent and unit-tested: the
// `sme.*` settings the user set, copied to their `engines.*` keys at the same scope (the old keys
// are left as they were), and `files.associations` entries that named the old `sme-loop` language.
// The VS Code side (reading each scope, writing, telling the user once) is in extension.ts.

/** The extension's ID before the rename. Installed beside this one, both would claim piece docs. */
export const OLD_EXTENSION_ID = 'sound-machine.sme-loop';
/** The language ID before the rename. */
export const OLD_LANGUAGE_ID = 'sme-loop';
export const LANGUAGE_ID = 'engines-loop';

export type Scope = 'global' | 'workspace' | 'folder';

/** One setting at one scope, as read: its old (`sme.*`) and new (`engines.*`) values. */
export interface Seen {
  /** The new key (`engines.live.path`). */
  key: string;
  scope: Scope;
  /** The workspace folder (its URI) for the folder scope. */
  folder?: string;
  old: unknown;
  current: unknown;
}

/** A write to make: `value` under `key` at `scope`. */
export interface Write {
  key: string;
  scope: Scope;
  folder?: string;
  value: unknown;
}

/** The old key of a new one: `engines.live.path` → `sme.live.path`. */
export function oldKey(key: string): string {
  return key.replace(/^engines\./, 'sme.');
}

/** Where a carry-over is remembered, so a setting the user later clears isn't copied again. */
export function doneId(w: { key: string; scope: Scope; folder?: string }): string {
  return [w.scope, w.folder ?? '', w.key].join('|');
}

/**
 * The settings to copy: each one set under its old key at a scope where the new key isn't set (an
 * already-migrated profile copies nothing) and that wasn't copied before (`done`).
 */
export function settingWrites(seen: Seen[], done: ReadonlySet<string>): Write[] {
  return seen
    .filter((s) => s.old !== undefined && s.current === undefined && !done.has(doneId(s)))
    .map((s) => ({ key: s.key, scope: s.scope, folder: s.folder, value: s.old }));
}

/** `files.associations` at one scope. */
export interface Associations {
  scope: Scope;
  folder?: string;
  value: unknown;
}

/** The `files.associations` maps to rewrite: every entry naming `sme-loop` now names `engines-loop`. */
export function associationWrites(seen: Associations[]): Write[] {
  const out: Write[] = [];
  for (const a of seen) {
    if (!a.value || typeof a.value !== 'object') {
      continue;
    }
    const map = a.value as Record<string, unknown>;
    if (!Object.values(map).includes(OLD_LANGUAGE_ID)) {
      continue;
    }
    const value = Object.fromEntries(Object.entries(map).map(([k, v]) => [k, v === OLD_LANGUAGE_ID ? LANGUAGE_ID : v]));
    out.push({ key: 'files.associations', scope: a.scope, folder: a.folder, value });
  }
  return out;
}

/** What to tell the user once the writes are made (nothing when there were none). */
export function carriedOverMessage(settings: number, associations: number): string | undefined {
  if (!settings && !associations) {
    return undefined;
  }
  const parts = [
    settings ? `${settings} setting${settings === 1 ? '' : 's'} (sme.* → engines.*)` : '',
    associations ? `${associations} file association${associations === 1 ? '' : 's'} (sme-loop → engines-loop)` : '',
  ].filter(Boolean);
  return `Engines carried over ${parts.join(' and ')} from SME Loop Docs. The old settings are left as they were.`;
}
