import { defineConfig, devices } from '@playwright/test';

/**
 * Browser integration tests. Each spec starts its own knotrel-server processes
 * (from KNOTREL_SERVER_BIN or ../target/debug/knotrel-server) and serves the
 * production build through `vite preview` with the same /api contract as the
 * container gateway. Run `npm run build` and build the server first.
 */
export default defineConfig({
  testDir: 'e2e',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: !!process.env.CI,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    trace: 'retain-on-failure',
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
    { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
  ],
});
