// The jump-to-source's arithmetic (ADR 0040): where the key lands and how the flash fades. Pure.

/** The flash: `n` frames from alpha `from` down towards 0 over `ms` (pure). */
export function flashFrames(n = 8, ms = 1600, from = 0.38): { at: number; alpha: number }[] {
  return Array.from({ length: n }, (_, i) => ({ at: Math.round((i * ms) / n), alpha: Number((from * (1 - i / n)).toFixed(3)) }));
}

/** The line to scroll to the top so `line` sits `context` lines below it (pure). */
export function topLine(line: number, context = 4): number {
  return Math.max(0, line - context);
}

/** `["song", "sequence", 1]` → `song.sequence[1]`. */
export function pathText(path: (string | number)[]): string {
  return path.map((p, i) => (typeof p === 'number' ? `[${p}]` : i ? `.${p}` : p)).join('');
}
