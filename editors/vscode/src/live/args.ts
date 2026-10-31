// `engines-live`'s command line from the extension's settings (editor-independent, unit-tested).

/** The `engines.live.*` settings that shape the command line. */
export interface LiveArgOptions {
  /** `engines.live.headless`: no audio device. */
  headless: boolean;
  /** `engines.live.outputDevice`: the device name (empty: the system output). */
  outputDevice: string;
  /** `engines.live.routeSources`: one stereo pair per audio source on that device (ADR 0044). */
  routeSources: boolean;
  /** `engines.live.args`: anything else, passed last. */
  extra: string[];
}

/** `engines-live <doc> --stdio …` for these settings. */
export function liveArgs(doc: string, o: LiveArgOptions): string[] {
  const args = [doc, '--stdio'];
  if (o.headless) {
    args.push('--headless');
  } else if (o.outputDevice.trim()) {
    args.push('--output-device', o.outputDevice.trim());
  }
  // engines-live refuses routing to the system output — only with a named device (or headless).
  if (o.routeSources && (o.headless || o.outputDevice.trim())) {
    args.push('--route-sources');
  }
  return [...args, ...o.extra];
}
