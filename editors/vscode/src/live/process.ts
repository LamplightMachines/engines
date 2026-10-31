// One `engines-live <doc> --stdio` child process: spawn, line-split its stdout into protocol events,
// send commands, quit cleanly, and restart it (with backoff) when it crashes after starting.
// A process that dies before `ready` (a doc with errors, no audio device, a missing binary) is NOT
// restarted blindly — the host decides when to try again (on the next save).

import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { parseEvent, type LiveCommand, type LiveEvent } from './protocol';

export interface SpawnSpec {
  command: string;
  args: string[];
  cwd: string;
  env?: NodeJS.ProcessEnv;
}

export interface ExitInfo {
  code: number | null;
  signal: NodeJS.Signals | null;
  /** `stop()` asked for it. */
  expected: boolean;
  /** It had sent `ready`. */
  ready: boolean;
  /** The last `error` event before `ready`, else the last stderr line. */
  message?: string;
}

export interface LiveProcessOptions {
  /** Restart delays after a crash (ms); the last one repeats. */
  backoff?: number[];
  /** Give up after this many crashes in a row. */
  maxRestarts?: number;
  /** A run at least this long resets the crash count. */
  stableMs?: number;
  /** How long `stop()` waits for a clean quit before killing. */
  quitTimeoutMs?: number;
}

export interface LiveProcessEvents {
  event: [LiveEvent];
  log: [string];
  exit: [ExitInfo];
  restarting: [{ attempt: number; delay: number }];
  gaveUp: [ExitInfo];
}

export class LiveProcess extends EventEmitter<LiveProcessEvents> {
  private child: ChildProcessWithoutNullStreams | undefined;
  private stopping = false;
  private ready = false;
  private crashes = 0;
  private startedAt = 0;
  private restartTimer: ReturnType<typeof setTimeout> | undefined;
  private lastError: string | undefined;
  private stderrTail: string[] = [];
  private readonly backoff: number[];
  private readonly maxRestarts: number;
  private readonly stableMs: number;
  private readonly quitTimeoutMs: number;

  constructor(
    readonly spec: SpawnSpec,
    opts: LiveProcessOptions = {},
  ) {
    super();
    this.backoff = opts.backoff ?? [500, 1000, 2000, 5000, 10000];
    this.maxRestarts = opts.maxRestarts ?? 5;
    this.stableMs = opts.stableMs ?? 30_000;
    this.quitTimeoutMs = opts.quitTimeoutMs ?? 2000;
  }

  /** A child is alive (or starting). */
  get running(): boolean {
    return this.child !== undefined;
  }

  /** It has sent `ready`, is still alive and isn't on its way out (a draining quit takes no commands:
   *  its stdin is already closed). */
  get isReady(): boolean {
    return this.ready && !this.stopping;
  }

  /** `stop()` asked it to quit and it hasn't exited yet (that drains for up to the quit timeout,
   *  then it's killed). */
  get isStopping(): boolean {
    return this.stopping && this.child !== undefined;
  }

  /** A crash restart is scheduled. */
  get restarting(): boolean {
    return this.restartTimer !== undefined;
  }

  start(): void {
    if (this.child) {
      return;
    }
    this.stopping = false;
    this.ready = false;
    this.lastError = undefined;
    this.stderrTail = [];
    this.startedAt = Date.now();
    const child = spawn(this.spec.command, this.spec.args, { cwd: this.spec.cwd, env: this.spec.env ?? process.env, stdio: 'pipe' });
    this.child = child;
    let closed = false;
    const done = (code: number | null, signal: NodeJS.Signals | null) => {
      if (!closed) {
        closed = true;
        this.onExit(child, code, signal);
      }
    };
    lines(child.stdout, (l) => this.onLine(l));
    lines(child.stderr, (l) => {
      this.stderrTail.push(l);
      if (this.stderrTail.length > 20) {
        this.stderrTail.shift();
      }
      this.emit('log', l);
    });
    child.stdin.on('error', () => {}); // EPIPE once it has died — the exit handler reports that
    child.on('error', (err) => {
      this.lastError = err.message; // e.g. spawn ENOENT
      if (child.pid === undefined) {
        done(null, null);
      }
    });
    child.on('close', (code, signal) => done(code, signal));
  }

  /** Send one command; `false` when there's no live process to take it. */
  send(cmd: LiveCommand): boolean {
    const c = this.child;
    if (!c || !c.stdin.writable) {
      return false;
    }
    c.stdin.write(JSON.stringify(cmd) + '\n');
    return true;
  }

  /** Quit (EOF + `quit`), killing it if it doesn't exit in time; cancels a pending restart. */
  async stop(): Promise<void> {
    if (this.restartTimer !== undefined) {
      clearTimeout(this.restartTimer);
      this.restartTimer = undefined;
    }
    const child = this.child;
    if (!child) {
      return;
    }
    this.stopping = true;
    await new Promise<void>((resolve) => {
      const kill = setTimeout(() => {
        child.kill('SIGTERM');
        setTimeout(() => child.kill('SIGKILL'), 1000).unref();
      }, this.quitTimeoutMs);
      child.once('close', () => {
        clearTimeout(kill);
        resolve();
      });
      this.send({ type: 'quit' });
      child.stdin.end();
    });
  }

  private onLine(line: string): void {
    const ev = parseEvent(line);
    if (!ev) {
      this.emit('log', line);
      return;
    }
    if (ev.type === 'ready') {
      this.ready = true;
    } else if (ev.type === 'error' && !this.ready) {
      this.lastError = ev.message;
    }
    this.emit('event', ev);
  }

  private onExit(child: ChildProcessWithoutNullStreams, code: number | null, signal: NodeJS.Signals | null): void {
    if (this.child !== child) {
      return;
    }
    const info: ExitInfo = { code, signal, expected: this.stopping, ready: this.ready, message: this.lastError ?? this.stderrTail.at(-1) };
    this.child = undefined;
    this.ready = false;
    const crashed = !info.expected && info.ready;
    let restart: { attempt: number; delay: number } | undefined;
    if (crashed) {
      if (Date.now() - this.startedAt >= this.stableMs) {
        this.crashes = 0;
      }
      this.crashes += 1;
      if (this.crashes <= this.maxRestarts) {
        restart = { attempt: this.crashes, delay: this.backoff[Math.min(this.crashes - 1, this.backoff.length - 1)] };
        this.restartTimer = setTimeout(() => {
          this.restartTimer = undefined;
          this.start();
        }, restart.delay);
      }
    }
    // `restarting` is already true when listeners hear `exit`.
    this.emit('exit', info);
    if (restart) {
      this.emit('restarting', restart);
    } else if (crashed) {
      this.emit('gaveUp', info);
    }
  }
}

/** Call `fn` with every complete line of `stream` (and a trailing partial one at its end). */
function lines(stream: NodeJS.ReadableStream, fn: (line: string) => void): void {
  let buf = '';
  stream.setEncoding('utf8');
  stream.on('data', (chunk: string) => {
    buf += chunk;
    let i: number;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).replace(/\r$/, '');
      buf = buf.slice(i + 1);
      if (line.trim()) {
        fn(line);
      }
    }
  });
  stream.on('end', () => {
    if (buf.trim()) {
      fn(buf);
    }
    buf = '';
  });
}
