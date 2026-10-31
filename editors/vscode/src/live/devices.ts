// The output picker (ADR 0044 D7): which device `engines-live` plays on, chosen in the panel instead of
// the `engines.live.outputDevice` / `engines.live.routeSources` settings (which it writes). Editor-independent
// and unit-tested; the controller runs the QuickPicks.

import type { LiveCommand, RoutingInfo } from './protocol';

/** One line of `engines-live --list-devices --json`. */
export interface OutputDevice {
  name: string;
  channels: number;
  sample_rate: number;
  default: boolean;
}

/** Where the player plays: a device name ('' = the system output) and whether each source gets its
 *  own pair (only on a named device). */
export interface OutputChoice {
  device: string;
  route: boolean;
}

/** `engines-live --list-devices --json`'s output (anything else: no devices). */
export function parseDevices(json: string): OutputDevice[] {
  try {
    const v: unknown = JSON.parse(json);
    if (!Array.isArray(v)) {
      return [];
    }
    return v.filter((d): d is OutputDevice => !!d && typeof d.name === 'string' && typeof d.channels === 'number' && typeof d.sample_rate === 'number');
  } catch {
    return [];
  }
}

function rate(sr: number): string {
  return `${Number((sr / 1000).toFixed(1))} kHz`;
}

export interface DeviceItem {
  label: string;
  description: string;
  detail?: string;
  /** '' = the system output. */
  device: string;
  channels: number;
}

/** The first pick: the system output (it follows macOS's choice each time it's opened), then every
 *  device. The current one is marked. */
export function deviceItems(devices: OutputDevice[], current: OutputChoice): DeviceItem[] {
  const def = devices.find((d) => d.default);
  const mark = (on: boolean) => (on ? '$(check) ' : '');
  const system: DeviceItem = {
    label: `${mark(current.device === '')}System output`,
    description: def ? `now ${def.name}` : '',
    detail: 'Whatever macOS plays to — pick it again after switching there (e.g. headphones plugged in).',
    device: '',
    channels: 2,
  };
  return [
    system,
    ...devices.map((d) => ({
      label: `${mark(current.device === d.name)}${d.name}`,
      description: `${d.channels} ch · ${rate(d.sample_rate)}${d.default ? ' · default' : ''}`,
      detail: d.channels > 2 && /blackhole|loopback|soundflower/i.test(d.name) ? 'A loopback — Live (or another app) has to listen to it for you to hear anything.' : undefined,
      device: d.name,
      channels: d.channels,
    })),
  ];
}

export interface RouteItem {
  label: string;
  description: string;
  route: boolean;
}

/** The second pick, for a multichannel device: each source on its own pair, or the mix. `pairs` says
 *  what the pairs choice does (a companion's output step may say more). */
export function routeItems(device: string, current: OutputChoice, pairs: string): RouteItem[] {
  const same = current.device === device;
  const mark = (on: boolean) => (on ? '$(check) ' : '');
  return [
    {
      label: `${mark(same && current.route)}Each source on its own pair`,
      description: pairs,
      route: true,
    },
    { label: `${mark(same && !current.route)}The mix`, description: 'on 1/2', route: false },
  ];
}

/** The `output` command for a choice. */
export function outputCommand(c: OutputChoice): Extract<LiveCommand, { type: 'output' }> {
  return { type: 'output', device: c.device || null, route_sources: !!c.device && c.route };
}

export function sameChoice(a: OutputChoice, b: OutputChoice): boolean {
  return a.device === b.device && (!!a.device && a.route) === (!!b.device && b.route);
}

/** The panel's device line: where it plays, and how. */
export function outputSummary(device: string | null, routing: RoutingInfo | null | undefined, sampleRate: number): string {
  if (!device) {
    return 'headless (no audio device)';
  }
  if (!routing) {
    return device;
  }
  const n = routing.pairs.length;
  const sr = sampleRate && sampleRate !== 48_000 ? ` · ${rate(sampleRate)} — set 48 kHz` : '';
  return `${device} · ${n} source${n === 1 ? '' : 's'} on their own pairs${sr}`;
}
