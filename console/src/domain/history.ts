/**
 * Console history: requests sent from this browser tab, kept in memory only.
 *
 * It is not the server's activity log and not an audit trail: other clients,
 * other tabs and the feeder are invisible here. Appending is O(limit) because
 * the array is copied to keep React state immutable.
 */
import type { Severity } from './sync';

/** A request that can be loaded back into the Workbench. */
export type ReplayableRequest =
  { path: 'v1/operations'; body: unknown } | { path: 'v1/batch'; body: unknown };

/** One recorded request. */
export interface HistoryEntry {
  /** Monotonic local identifier. */
  id: number;
  /** Local time the response (or failure) was observed. */
  at: Date;
  /** Human description with exact IDs. */
  description: string;
  /** Short outcome, e.g. "Effective", "Connected", "3 ok, 1 error". */
  summary: string;
  /** Severity of the outcome, shown with an icon and text. */
  severity: Severity;
  /** Whether the request could change the graph. */
  mutating: boolean;
  /** The request, for replay in the Workbench (no credentials). */
  request: ReplayableRequest | undefined;
  /** Server instance that answered, when known. */
  instanceId: string | undefined;
  /** Browser-measured duration in milliseconds. */
  durationMs: number;
}

/** Default number of entries retained. */
export const HISTORY_LIMIT = 100;

/** Returns a new history with `entry` first, keeping at most `limit` entries. */
export function appendHistory(
  history: readonly HistoryEntry[],
  entry: HistoryEntry,
  limit: number = HISTORY_LIMIT,
): HistoryEntry[] {
  return [entry, ...history].slice(0, Math.max(0, limit));
}
