/**
 * Shareable hash routes: `#/<page>?<params>`. Parameters hold filters and the
 * selected element, never credentials or server data beyond public IDs.
 */

/** Navigable pages. */
export const PAGE_IDS = ['overview', 'browser', 'workbench', 'performance', 'sync'] as const;

/** Page identifier. */
export type PageId = (typeof PAGE_IDS)[number];

/** A parsed route. */
export interface Route {
  page: PageId;
  params: URLSearchParams;
}

/** Parses `location.hash`. Unknown pages fall back to the overview. */
export function parseHash(hash: string): Route {
  const text = hash.replace(/^#\/?/, '');
  const [path = '', search = ''] = text.split('?', 2);
  const page = (PAGE_IDS as readonly string[]).includes(path) ? (path as PageId) : 'overview';
  return { page, params: new URLSearchParams(search) };
}

/** Serializes a route; empty parameters are dropped. */
export function formatHash(page: PageId, params: Record<string, string | undefined> = {}): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== '') {
      search.set(key, value);
    }
  }
  const text = search.toString();
  return `#/${page}${text ? `?${text}` : ''}`;
}
