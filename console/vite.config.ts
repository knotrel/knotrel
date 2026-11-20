/// <reference types="vitest/config" />
import { readFileSync } from 'node:fs';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as {
  version: string;
};

// The console is a static bundle. Relative asset URLs (`base: './'`) let the same
// build be served from `/` by the launcher or from a sub-path behind a gateway.
export default defineConfig({
  base: './',
  plugins: [react()],
  define: {
    __CONSOLE_VERSION__: JSON.stringify(pkg.version),
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: false,
    target: 'es2022',
    assetsInlineLimit: 0,
  },
  server: {
    // Development only: forward the same allowlisted API prefix the launcher serves.
    proxy: {
      '/api': {
        target: process.env.KNOTREL_SERVER ?? 'http://127.0.0.1:8080',
        rewrite: (path) => path.replace(/^\/api/, ''),
      },
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./test/setup.ts'],
    include: ['test/**/*.test.{ts,tsx}'],
    restoreMocks: true,
    coverage: {
      provider: 'v8',
      // LCOV paths are written relative to the repository root (console/src/...)
      // so SonarCloud and Codecov resolve them without rewriting.
      reporter: ['text', ['lcov', { projectRoot: '..' }], 'json-summary'],
      reportsDirectory: 'coverage',
      // Every handwritten source file is measured, imported by a test or not.
      include: ['src/**/*.{ts,tsx}'],
      exclude: [
        // Generated from docs/openapi.yaml; types only, no runtime code.
        'src/api/generated/**',
        // Ambient declarations only.
        'src/**/*.d.ts',
      ],
      thresholds: {
        statements: 80,
        branches: 80,
        functions: 80,
        lines: 80,
      },
    },
  },
});
