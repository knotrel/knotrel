/**
 * Color theme: dark by default, light on request. The choice is a per-browser
 * convenience kept in local storage; the console works without storage.
 */

/** Available themes. */
export type Theme = 'dark' | 'light';

const KEY = 'knotrel-console.theme';

/** Reads the stored theme, falling back to dark. */
export function loadTheme(storage: Storage | undefined = safeStorage()): Theme {
  try {
    return storage?.getItem(KEY) === 'light' ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

/** Stores the theme; returns false when storage is unavailable. */
export function storeTheme(theme: Theme, storage: Storage | undefined = safeStorage()): boolean {
  try {
    if (!storage) return false;
    storage.setItem(KEY, theme);
    return true;
  } catch {
    return false;
  }
}

/** Applies the theme to the document root (`data-theme`). */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement): void {
  root.dataset.theme = theme;
}

function safeStorage(): Storage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}
