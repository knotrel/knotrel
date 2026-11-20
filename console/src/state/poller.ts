/**
 * Bounded polling scheduler.
 *
 * Invariants:
 * - At most one run is current; the next run is scheduled only after the current
 *   one settles, so requests never overlap.
 * - A manual refresh aborts the current run (its result is ignored) and starts a
 *   new one immediately.
 * - After `n` consecutive failures the delay is `min(interval * 2^n, maxBackoff)`.
 * - While the page is hidden, polling pauses; it resumes with an immediate run when
 *   the page becomes visible again.
 * - `stop()` aborts the current run and cancels any scheduled run.
 */

/** Page visibility source, injectable for tests. */
export interface VisibilitySource {
  isHidden(): boolean;
  subscribe(listener: () => void): () => void;
}

/** Visibility of the current document. */
export const documentVisibility: VisibilitySource = {
  isHidden: () => typeof document !== 'undefined' && document.visibilityState === 'hidden',
  subscribe: (listener) => {
    document.addEventListener('visibilitychange', listener);
    return () => document.removeEventListener('visibilitychange', listener);
  },
};

/** Poller configuration. */
export interface PollerOptions {
  /** One polling cycle; resolves `true` on success. Must honor `signal`. */
  task: (signal: AbortSignal) => Promise<boolean>;
  /** Delay between successful runs, in milliseconds. */
  intervalMs: number;
  /** Upper bound of the error backoff, in milliseconds. */
  maxBackoffMs: number;
  /** Visibility source (defaults to the document). */
  visibility?: VisibilitySource;
}

/** Schedules non-overlapping, cancellable polling runs. */
export class Poller {
  readonly #options: PollerOptions;
  readonly #visibility: VisibilitySource;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #controller: AbortController | undefined;
  #generation = 0;
  #failures = 0;
  #running = false;
  #unsubscribe: (() => void) | undefined;

  constructor(options: PollerOptions) {
    this.#options = options;
    this.#visibility = options.visibility ?? documentVisibility;
  }

  /** Consecutive failed runs. */
  get failures(): number {
    return this.#failures;
  }

  /** Delay before the next run after the current failure count. */
  nextDelay(): number {
    const { intervalMs, maxBackoffMs } = this.#options;
    return this.#failures === 0
      ? intervalMs
      : Math.min(intervalMs * 2 ** this.#failures, maxBackoffMs);
  }

  /** Starts polling with an immediate run. Idempotent. */
  start(): void {
    if (this.#running) {
      return;
    }
    this.#running = true;
    this.#unsubscribe = this.#visibility.subscribe(() => {
      if (this.#running && !this.#visibility.isHidden()) {
        this.refreshNow();
      }
    });
    this.refreshNow();
  }

  /** Stops polling and aborts the current run. */
  stop(): void {
    this.#running = false;
    this.#generation += 1;
    clearTimeout(this.#timer);
    this.#timer = undefined;
    this.#controller?.abort();
    this.#controller = undefined;
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;
  }

  /** Aborts any current run and starts a new one now. */
  refreshNow(): void {
    if (!this.#running) {
      return;
    }
    clearTimeout(this.#timer);
    this.#timer = undefined;
    this.#controller?.abort();
    void this.#run();
  }

  async #run(): Promise<void> {
    const generation = ++this.#generation;
    const controller = new AbortController();
    this.#controller = controller;
    let ok: boolean;
    try {
      ok = await this.#options.task(controller.signal);
    } catch {
      ok = false;
    }
    if (generation !== this.#generation || !this.#running) {
      return; // Superseded or stopped: ignore this result.
    }
    this.#controller = undefined;
    this.#failures = ok ? 0 : this.#failures + 1;
    if (this.#visibility.isHidden()) {
      return; // Paused until the page is visible again.
    }
    this.#timer = setTimeout(() => void this.#run(), this.nextDelay());
  }
}
