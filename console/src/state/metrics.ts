/**
 * Session window of `/metrics` scrapes for the Performance and Synchronization
 * views.
 *
 * There is no history beyond what this browser tab observed: at most
 * {@link MAX_SCRAPES} scrapes (15 minutes at the default interval). When the
 * server answers 404, metrics are disabled; polling stops until the user
 * checks again. Polling never overlaps and pauses in hidden tabs.
 */
import type { ConsoleApi } from '../api/endpoints';
import { parseExposition, type Scrape } from '../domain/prometheus';
import { Poller, type VisibilitySource } from './poller';

/** Scrape interval, in milliseconds. */
export const METRICS_INTERVAL_MS = 5_000;
/** Most scrapes kept in memory. */
export const MAX_SCRAPES = 180;

/** Availability of `/metrics`. */
export type MetricsAvailability = 'unknown' | 'available' | 'disabled' | 'error';

/** Snapshot of the metrics store. */
export interface MetricsState {
  availability: MetricsAvailability;
  scrapes: Scrape[];
  /** Description of the latest failure, if any. */
  error: string | undefined;
  /** Lines that could not be parsed in the latest scrape. */
  skippedLines: number;
}

const INITIAL: MetricsState = {
  availability: 'unknown',
  scrapes: [],
  error: undefined,
  skippedLines: 0,
};

/** Polls `/metrics` and keeps the bounded session window. */
export class MetricsStore {
  #state: MetricsState = INITIAL;
  readonly #listeners = new Set<() => void>();
  readonly #api: ConsoleApi;
  readonly #poller: Poller;
  readonly #now: () => number;

  constructor(
    api: ConsoleApi,
    options: { intervalMs?: number; visibility?: VisibilitySource; now?: () => number } = {},
  ) {
    this.#api = api;
    this.#now = options.now ?? Date.now;
    const pollerOptions = {
      task: (signal: AbortSignal) => this.#scrape(signal),
      intervalMs: options.intervalMs ?? METRICS_INTERVAL_MS,
      maxBackoffMs: 60_000,
    };
    this.#poller = new Poller(
      options.visibility ? { ...pollerOptions, visibility: options.visibility } : pollerOptions,
    );
  }

  getState = (): MetricsState => this.#state;

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  /** Starts polling. */
  start(): void {
    this.#poller.start();
  }

  /** Stops polling. */
  stop(): void {
    this.#poller.stop();
  }

  /** Retries now (also after metrics were found disabled). */
  retry(): void {
    this.#poller.stop();
    this.#poller.start();
  }

  #set(patch: Partial<MetricsState>): void {
    this.#state = { ...this.#state, ...patch };
    for (const listener of this.#listeners) {
      listener();
    }
  }

  async #scrape(signal: AbortSignal): Promise<boolean> {
    const result = await this.#api.metrics(signal);
    if (signal.aborted) {
      return false;
    }
    if (result.ok) {
      const { samples, skipped } = parseExposition(result.value);
      const scrapes = [...this.#state.scrapes, { at: this.#now(), samples }].slice(-MAX_SCRAPES);
      this.#set({ availability: 'available', scrapes, error: undefined, skippedLines: skipped });
      return true;
    }
    const failure = result.failure;
    if (failure.kind === 'http' && failure.status === 404) {
      this.#set({ availability: 'disabled', error: undefined });
      this.#poller.stop(); // Nothing to poll; the user can check again.
      return true;
    }
    if (failure.kind === 'aborted') {
      return false;
    }
    this.#set({ availability: 'error', error: failure.kind });
    return false;
  }
}
