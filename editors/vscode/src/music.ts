// The panel's keyboard: which keys are in the doc's scale, which pitch classes the Orrery's chord
// holds, and where its pad voicing and bass sit. Pitch follows the engine (ADR 0037): offset 0 is the
// root in octave 4 (note number 60 + root pitch class).

import type { KeyInfo, Voicing } from './live/protocol';

export interface KeyCell {
  midi: number;
  pc: number;
  black: boolean;
  /** Spelled in the doc's key, with the octave of its letter (`Bb3`, `F#4`). */
  name: string;
  inScale: boolean;
  root: boolean;
  /** The degree label for in-scale keys (`♭3`). */
  degree: string | null;
  chord: boolean;
  pad: boolean;
  bass: boolean;
}

export interface Keyboard {
  low: number;
  high: number;
  cells: KeyCell[];
}

const BLACK = new Set([1, 3, 6, 8, 10]);
const NATURAL: Record<string, number> = { C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11 };

const mod12 = (n: number) => ((n % 12) + 12) % 12;

export function midiOf(root: number, offset: number): number {
  return 60 + root + offset;
}

/** `name` (`Bb`, `F#`, `B#`) at `midi`, with the octave its letter belongs to (B#3 = C4, Cb4 = B3). */
export function noteName(name: string, midi: number): string {
  const nat = NATURAL[name[0]];
  if (nat === undefined) {
    return `${name}${Math.floor(midi / 12) - 1}`;
  }
  const acc = [...name.slice(1)].reduce((a, c) => a + (c === '#' ? 1 : c === 'b' ? -1 : 0), 0);
  return `${name}${Math.round((midi - acc - nat) / 12) - 1}`;
}

/** The keyboard for `key` (C3–A5, widened to whole octaves around any voiced note outside it). */
export function keyboard(key: KeyInfo | null, voicing: Voicing | null): Keyboard | null {
  if (!key) {
    return null;
  }
  const pad = new Set((voicing?.pad ?? []).map((o) => midiOf(key.root, o)));
  const bass = voicing ? midiOf(key.root, voicing.bass) : null;
  const chord = new Set((voicing?.tones ?? []).map((t) => mod12(key.root + t)));
  let low = 48;
  let high = 81;
  for (const n of [...pad, ...(bass === null ? [] : [bass])]) {
    if (n < low) {
      low = n - mod12(n);
    }
    if (n > high) {
      high = n - mod12(n) + 11;
    }
  }
  const degree = new Map(key.scale.map((s, i) => [mod12(key.root + s), key.degrees[i] ?? null]));
  const cells: KeyCell[] = [];
  for (let midi = low; midi <= high; midi++) {
    const pc = mod12(midi);
    cells.push({
      midi,
      pc,
      black: BLACK.has(pc),
      name: noteName(key.names[pc] ?? '?', midi),
      inScale: degree.has(pc),
      root: pc === mod12(key.root),
      degree: degree.get(pc) ?? null,
      chord: chord.has(pc),
      pad: pad.has(midi),
      bass: bass === midi,
    });
  }
  return { low, high, cells };
}

/** `F#4` → `F♯4` (display; the protocol spells with `#` / `b`). */
export function pretty(name: string): string {
  return name.replace(/^([A-G])([#b]+)/, (_, l: string, acc: string) => l + acc.replace(/#/g, '♯').replace(/b/g, '♭'));
}

/** `F#4` → `F sharp 4` (a screen reader's reading). */
export function spoken(name: string): string {
  return name.replace(/^([A-G])([#b]*)(-?\d+)?$/, (_, l: string, acc: string, oct: string | undefined) =>
    [l, ...[...acc].map((a) => (a === '#' ? 'sharp' : 'flat')), oct ?? ''].filter(Boolean).join(' '),
  );
}

/** The chord's tones spelled in the key, root first (`G · B · D · F♯`): the voicing's `tones`, in
 *  the order the Orrery gives them. */
export function chordTones(key: KeyInfo | null, voicing: Voicing | null): string[] {
  if (!key || !voicing) {
    return [];
  }
  return voicing.tones.map((t) => pretty(key.names[mod12(key.root + t)] ?? '?'));
}

/** The keyboard as words (its image's text alternative); chord tones in the chord's order. */
export function keyboardAria(kb: Keyboard | null, key: KeyInfo | null, voicing: Voicing | null = null): string {
  if (!kb || !key) {
    return 'Keyboard: no key';
  }
  const range = `Keyboard ${spoken(kb.cells[0].name)} to ${spoken(kb.cells[kb.cells.length - 1].name)}.`;
  const parts = [range, `${key.name.replace(/^(\S+)/, (r) => spoken(r))} scale highlighted.`];
  const chord = [...new Set((voicing?.tones ?? []).map((t) => mod12(key.root + t)))].map((pc) => spoken(key.names[pc] ?? '?'));
  const pad = kb.cells.filter((c) => c.pad).map((c) => spoken(c.name));
  const bass = kb.cells.filter((c) => c.bass).map((c) => spoken(c.name));
  if (chord.length) {
    parts.push(`Chord tones ${chord.join(', ')}.`);
  }
  if (pad.length) {
    parts.push(`Pad notes ${pad.join(', ')}.`);
  }
  if (bass.length) {
    parts.push(`Bass note ${bass.join(', ')}.`);
  }
  return parts.join(' ');
}
