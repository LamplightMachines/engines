// The companion API: what an extension built on this one (Engines Studio) can add
// without its code shipping here — strip actions, output-picker steps, menu items, its own binaries —
// and what it can read (the piece, the player's state and raw protocol objects). Editor-independent
// and unit-tested; extension.ts hands it out from `activate()` and wires it to VS Code. The contract is
// documented in API.md.

import type { LiveState } from './live/state';

/** The API's version. A companion asks for a major version; a different one is refused. */
export const API_VERSION = '1.0.0';
export const API_MAJOR = 1;

export interface Disposable {
  dispose(): void;
}

/** The strip an action is asked about: an instrument of the playing piece, with the player's state. */
export interface StripContext {
  /** The instrument id. */
  id: string;
  /** The player's state (its `routing`, `capabilities`, …). */
  state: LiveState;
}

/** What a strip action shows: nothing (`null`), or a button that is on, off or blocked. */
export type StripActionState = null | { on: boolean; blocked?: boolean; title: string };

export interface StripAction {
  /** Unique among a companion's actions; the panel keys its button by it. */
  id: string;
  /** SVG path data in a 24×24 box, stroked in the button's colour (no markup: the panel draws it). */
  icon: string;
  state(strip: StripContext): StripActionState;
  run(strip: StripContext): unknown;
  /** Fires when `state` would answer differently (the panel redraws). */
  onDidChange?(listener: () => void): Disposable;
}

/** One strip action as the panel draws it. */
export interface StripActionView {
  id: string;
  icon: string;
  on: boolean;
  blocked: boolean;
  title: string;
}

/** What an output step knows: the piece (if one is open), the device picked and its channels. */
export interface OutputContext {
  piece: PieceInfo | undefined;
  device: string;
  channels: number;
}

/** An output-picker step, asked after a multichannel device is picked. */
export interface OutputStep {
  id: string;
  /** What the routing-pairs choice does with this step (its description), if it changes it. */
  pairsDescription?(ctx: OutputContext): string | undefined;
  /** Runs after the pick, before the player switches; `route` is whether each source gets its own pair. */
  beforeSwitch?(ctx: OutputContext & { route: boolean }): unknown;
}

/** A command a companion registered with VS Code, placed in one of this extension's menus. */
export interface MenuItem {
  command: string;
  title: string;
  where: 'panel' | 'statusBar';
}

/** The binaries a companion may swap in (the player restarts onto a new one). */
export type SwappableTool = 'engines-live' | 'engines-lsp';

export interface PieceInfo {
  slug: string;
  /** The piece's folder (`pieces/<slug>`). */
  dir: string;
  /** Its doc (`pieces/<slug>/<slug>.json5`). */
  doc: string;
  /** The repo root holding `pieces/`. */
  root: string;
}

/** What the player last said: its state, and the raw `ready` and `status` objects (fields this
 *  extension doesn't know included). */
export interface PlayerInfo {
  state: LiveState;
  ready: Record<string, unknown> | undefined;
  status: Record<string, unknown> | undefined;
}

/** The API a companion gets from `connect`. */
export interface EnginesApi {
  readonly version: string;
  setBinary(tool: SwappableTool, path: string | undefined): void;
  registerStripAction(action: StripAction): Disposable;
  registerOutputStep(step: OutputStep): Disposable;
  registerMenuItem(item: MenuItem): Disposable;
  piece(): PieceInfo | undefined;
  player(): PlayerInfo;
  /** Fires when the piece or the player's state changes. */
  onDidChange(listener: () => void): Disposable;
}

/** Why a companion built for `major` can't use this API, or `undefined` when it can. */
export function refusal(name: string, major: number): string | undefined {
  if (major === API_MAJOR) {
    return undefined;
  }
  const update = major < API_MAJOR ? name : 'Engines';
  return `${name} is built for Engines API v${major}, but this Engines has v${API_MAJOR} (${API_VERSION}) — update ${update}.`;
}

/** Where the extension reads what it hands out. */
export interface Host {
  piece(): PieceInfo | undefined;
  player(): PlayerInfo;
  /** A swapped binary took effect: restart the player (or the language server) onto it. */
  binaryChanged(tool: SwappableTool): void;
}

/** The registry behind every connected companion. */
export class Companions {
  private readonly actions: StripAction[] = [];
  private readonly steps: OutputStep[] = [];
  private readonly items: MenuItem[] = [];
  private readonly binaries = new Map<SwappableTool, string>();
  private readonly listeners = new Set<() => void>();
  private readonly subs = new Map<StripAction, Disposable>();

  constructor(private readonly host: Host) {}

  /** The API for companion `name`, built for API v`major` — or an error saying which to update. */
  connect(name: string, major: number): EnginesApi {
    const why = refusal(name, major);
    if (why) {
      throw new Error(why);
    }
    const reg = <T>(list: T[], item: T, after?: () => void): Disposable => {
      list.push(item);
      this.changed();
      return {
        dispose: () => {
          const i = list.indexOf(item);
          if (i >= 0) {
            list.splice(i, 1);
            after?.();
            this.changed();
          }
        },
      };
    };
    return {
      version: API_VERSION,
      setBinary: (tool, path) => this.setBinary(tool, path),
      registerStripAction: (action) => {
        const sub = action.onDidChange?.(() => this.changed());
        if (sub) {
          this.subs.set(action, sub);
        }
        return reg(this.actions, action, () => {
          this.subs.get(action)?.dispose();
          this.subs.delete(action);
        });
      },
      registerOutputStep: (step) => reg(this.steps, step),
      registerMenuItem: (item) => reg(this.items, item),
      piece: () => this.host.piece(),
      player: () => this.host.player(),
      onDidChange: (listener) => this.subscribe(listener),
    };
  }

  /** Listen for changes (the panel redraws its strips). */
  subscribe(listener: () => void): Disposable {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }

  /** Tell every listener (companions, the panel) that something changed. */
  changed(): void {
    for (const l of [...this.listeners]) {
      l();
    }
  }

  private setBinary(tool: SwappableTool, path: string | undefined): void {
    const before = this.binaries.get(tool);
    if (path) {
      this.binaries.set(tool, path);
    } else {
      this.binaries.delete(tool);
    }
    if (before !== this.binaries.get(tool)) {
      this.host.binaryChanged(tool);
    }
  }

  /** The binary a companion swapped in for `tool`, if any. */
  binary(tool: SwappableTool): string | undefined {
    return this.binaries.get(tool);
  }

  /** The buttons strip `id` shows, in registration order. An action that throws shows nothing. */
  stripActions(id: string, state: LiveState): StripActionView[] {
    const out: StripActionView[] = [];
    for (const a of this.actions) {
      let s: StripActionState = null;
      try {
        s = a.state({ id, state });
      } catch {
        s = null;
      }
      if (s) {
        out.push({ id: a.id, icon: a.icon, on: s.on, blocked: !!s.blocked, title: s.title });
      }
    }
    return out;
  }

  /** Run strip action `action` on strip `id`. `false` when there's no such action. */
  runStripAction(action: string, id: string, state: LiveState): boolean {
    const a = this.actions.find((x) => x.id === action);
    if (!a) {
      return false;
    }
    void Promise.resolve(a.run({ id, state }));
    return true;
  }

  /** The routing-pairs choice's description: the last step's that has one, else `fallback`. */
  pairsDescription(ctx: OutputContext, fallback: string): string {
    let d = fallback;
    for (const s of this.steps) {
      d = s.pairsDescription?.(ctx) ?? d;
    }
    return d;
  }

  /** Run every step's `beforeSwitch`, in order. */
  async beforeSwitch(ctx: OutputContext & { route: boolean }): Promise<void> {
    for (const s of this.steps) {
      await s.beforeSwitch?.(ctx);
    }
  }

  /** The menu items placed in `where`. */
  menu(where: MenuItem['where']): MenuItem[] {
    return this.items.filter((i) => i.where === where);
  }
}
