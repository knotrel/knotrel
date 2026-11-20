import { describe, expect, it } from 'vitest';
import { applyTheme, loadTheme, storeTheme } from '../src/ui/theme';

function memoryStorage(initial: Record<string, string> = {}): Storage {
  const data = new Map(Object.entries(initial));
  return {
    get length() {
      return data.size;
    },
    clear: () => data.clear(),
    getItem: (key) => data.get(key) ?? null,
    key: (index) => [...data.keys()][index] ?? null,
    removeItem: (key) => void data.delete(key),
    setItem: (key, value) => void data.set(key, value),
  };
}

const broken = {
  getItem: () => {
    throw new Error('blocked');
  },
  setItem: () => {
    throw new Error('blocked');
  },
} as unknown as Storage;

describe('theme', () => {
  it('defaults to dark and reads a stored light choice', () => {
    expect(loadTheme(memoryStorage())).toBe('dark');
    expect(loadTheme(memoryStorage({ 'knotrel-console.theme': 'light' }))).toBe('light');
    expect(loadTheme(memoryStorage({ 'knotrel-console.theme': 'purple' }))).toBe('dark');
  });

  it('survives unavailable storage', () => {
    expect(loadTheme(broken)).toBe('dark');
    expect(storeTheme('light', broken)).toBe(false);
    const storage = memoryStorage();
    expect(storeTheme('light', storage)).toBe(true);
    expect(storage.getItem('knotrel-console.theme')).toBe('light');
  });

  it('applies the theme to the root element', () => {
    const root = document.createElement('html');
    applyTheme('light', root);
    expect(root.dataset.theme).toBe('light');
  });
});
