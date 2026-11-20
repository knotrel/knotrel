/**
 * Bounded, in-memory history of operations performed during this UI session.
 *
 * The history is never persisted. Appending is O(limit) because the array is
 * copied to keep React state immutable; with the default limit of 50 this is
 * negligible.
 */
import type { MutationVerdict } from './outcomes';

/** One recorded operation. */
export interface HistoryEntry {
  /** Monotonic local identifier. */
  id: number;
  /** Local time the response (or failure) was observed. */
  at: Date;
  /** Human description with exact IDs. */
  description: string;
  /** Classified verdict. */
  verdict: MutationVerdict;
  /** Server instance that answered, when known. */
  instanceId: string | undefined;
  /** Browser-measured duration in milliseconds. */
  durationMs: number;
}

/** Default number of entries retained. */
export const HISTORY_LIMIT = 50;

/** Returns a new history with `entry` first, keeping at most `limit` entries. */
export function appendHistory(
  history: readonly HistoryEntry[],
  entry: HistoryEntry,
  limit: number = HISTORY_LIMIT,
): HistoryEntry[] {
  return [entry, ...history].slice(0, Math.max(0, limit));
}
