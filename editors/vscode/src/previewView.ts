// The preview dialog's choices (ADR 0040): a length, then the mix alone or the mix + stems (one
// −18 LUFS file per audio instrument and bus), each with what it costs on disk. Pure.

export interface LengthPick {
  label: string;
  /** `undefined`: ask for a custom length. */
  seconds: number | undefined;
}

export interface KindPick {
  label: string;
  description: string;
  detail: string;
  stems: boolean;
}

/** The longest preview: the mix alone, and with stems (every source is a file of its own). */
export const MAX_MIX_S = 600;
export const MAX_STEMS_S = 300;

export function previewLengths(): LengthPick[] {
  return [
    { label: '30 seconds', seconds: 30 },
    { label: '1 minute', seconds: 60 },
    { label: '2 minutes', seconds: 120 },
    { label: '5 minutes', seconds: 300 },
    { label: 'Custom…', seconds: undefined },
  ];
}

/** `"45"` → 45; anything else → an error message (the input box's validation). */
export function parseSeconds(text: string): number | string {
  const n = Number(text);
  return Number.isFinite(n) && n > 0 && n <= MAX_MIX_S ? n : `a number of seconds, up to ${MAX_MIX_S}`;
}

function mb(bytes: number): string {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

/** The two kinds for `seconds` of a doc with `instruments` audio instruments (buses add a stem each).
 *  The player caps a stems preview — the mix with it — at `MAX_STEMS_S`. */
export function previewKinds(seconds: number, instruments: number, sampleRate = 48_000): KindPick[] {
  const mix = (s: number) => s * sampleRate * 2 * 4; // float WAV
  const stemS = Math.min(seconds, MAX_STEMS_S);
  const perStem = stemS * sampleRate * 2 * 3; // 24-bit WAV
  const capped = seconds > MAX_STEMS_S ? ` — mix and stems are capped at ${MAX_STEMS_S / 60} min` : '';
  return [
    { label: 'Mix only', description: `≈ ${mb(mix(seconds))}`, detail: '.live/preview.wav — the doc as written, rendered offline (deterministic)', stems: false },
    {
      label: 'Mix + stems',
      description: `≈ ${mb(mix(stemS) + Math.max(1, instruments) * perStem)}+`,
      detail: `.live/preview_stems/ — one −18 LUFS 24-bit WAV per instrument (${instruments}) and per bus${capped}`,
      stems: true,
    },
  ];
}

/** The notification once a preview is rendered. */
export function previewMessage(ev: { seconds: number; peak_db: number; lufs?: number | null; stems?: { files: unknown[] } | null }): string {
  const lufs = ev.lufs != null ? `, ${ev.lufs.toFixed(1)} LUFS` : '';
  const stems = ev.stems ? ` + ${ev.stems.files.length} stem${ev.stems.files.length === 1 ? '' : 's'}` : '';
  return `Preview rendered: ${ev.seconds.toFixed(0)} s, peak ${ev.peak_db.toFixed(1)} dBFS${lufs}${stems}.`;
}
