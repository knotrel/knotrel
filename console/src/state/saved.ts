/**
 * Saved workbench requests in browser storage (this browser only).
 *
 * Requests contain operations and node IDs only; the console holds no
 * credentials. Storage can be unavailable (private windows, blocked site
 * data), so every access is guarded and failures degrade to an empty list.
 */

/** One saved request. */
export interface SavedRequest {
  name: string;
  /** Editor text (a batch or single operation in JSON). */
  text: string;
  savedAt: string;
}

const KEY = 'knotrel-console.saved-requests.v1';
/** Most requests kept. */
export const SAVED_LIMIT = 50;

/** Minimal storage interface, satisfied by `localStorage`. */
export interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function defaultStorage(): KeyValueStorage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

/** Loads saved requests; returns `[]` when storage is unavailable or corrupt. */
export function loadSaved(storage: KeyValueStorage | undefined = defaultStorage()): SavedRequest[] {
  try {
    const raw = storage?.getItem(KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (item): item is SavedRequest =>
          typeof item === 'object' &&
          item !== null &&
          typeof (item as SavedRequest).name === 'string' &&
          typeof (item as SavedRequest).text === 'string' &&
          typeof (item as SavedRequest).savedAt === 'string',
      )
      .slice(0, SAVED_LIMIT);
  } catch {
    return [];
  }
}

/** Persists saved requests. Returns false when storage is unavailable. */
export function storeSaved(
  requests: readonly SavedRequest[],
  storage: KeyValueStorage | undefined = defaultStorage(),
): boolean {
  try {
    if (!storage) return false;
    storage.setItem(KEY, JSON.stringify(requests.slice(0, SAVED_LIMIT)));
    return true;
  } catch {
    return false;
  }
}

/** Adds or replaces a request by name, newest first. */
export function upsertSaved(
  requests: readonly SavedRequest[],
  request: SavedRequest,
): SavedRequest[] {
  return [request, ...requests.filter((r) => r.name !== request.name)].slice(0, SAVED_LIMIT);
}
