// Run a function once things have been quiet for `ms` — how the editor's buffer reaches the player
// (the plan's 300 ms: typing doesn't flood it, a pause is heard at once).

export class Debouncer {
  private timer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private readonly fn: () => void,
    public ms: number,
  ) {}

  /** (Re)start the quiet period. */
  trigger(): void {
    this.cancel();
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.fn();
    }, this.ms);
  }

  /** Drop a pending run (the player quit, or it hears saves again). */
  cancel(): void {
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
  }
}

/** Call at most once per `ms`, always delivering the latest value (trailing edge included). */
export class Throttle<T> {
  private last = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private value: T | undefined;

  constructor(
    private readonly fn: (v: T) => void,
    private readonly ms: number,
    private readonly now: () => number = Date.now,
  ) {}

  push(v: T): void {
    this.value = v;
    const wait = this.last + this.ms - this.now();
    if (wait <= 0) {
      this.fire();
    } else if (this.timer === undefined) {
      this.timer = setTimeout(() => this.fire(), wait);
    }
  }

  private fire(): void {
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    this.last = this.now();
    const v = this.value as T;
    this.fn(v);
  }

  dispose(): void {
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
  }
}
