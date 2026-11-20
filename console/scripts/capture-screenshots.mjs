#!/usr/bin/env node
// Captures the documentation screenshots from a running console loaded with the
// demo network (start-local.sh, then seed-demo.sh). Read-only: it runs queries
// but never changes the graph.
//
//   node console/scripts/capture-screenshots.mjs [console URL] [output dir]
//
// Defaults: http://127.0.0.1:7380/ and docs/images/console/ (repository root).
import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import { chromium } from '@playwright/test';

const base = (process.argv[2] ?? 'http://127.0.0.1:7380/').replace(/\/?$/, '/');
const out = path.resolve(
  process.argv[3] ?? path.join(import.meta.dirname, '..', '..', 'docs', 'images', 'console'),
);
const viewport = { width: 1280, height: 720 };

await mkdir(out, { recursive: true });
const browser = await chromium.launch();

/**
 * Opens a page with the given theme and returns it once the console is connected.
 * @param {string} hash Route after `#/`.
 * @param {'dark' | 'light'} [theme]
 */
async function open(hash, theme = 'dark') {
  const context = await browser.newContext({ viewport, deviceScaleFactor: 2 });
  await context.addInitScript((value) => {
    globalThis.localStorage.setItem('knotrel-console.theme', value);
  }, theme);
  const page = await context.newPage();
  await page.goto(`${base}#/${hash}`);
  await page.locator('.connection-button.tone-ok').waitFor();
  return page;
}

/**
 * Saves a JPEG screenshot and closes the page's context.
 * @param {import('@playwright/test').Page} page
 * @param {string} name File name without extension.
 */
async function shot(page, name) {
  const file = path.join(out, `${name}.jpg`);
  await page.screenshot({ path: file, type: 'jpeg', quality: 82 });
  console.log(`wrote ${path.relative(process.cwd(), file)}`);
  await page.context().close();
}

try {
  // Browser: node 2 with a disabled edge and a connectivity check.
  let page = await open('browser?node=2');
  const pane = page.getByRole('region', { name: 'Node 2' });
  await pane.getByLabel('Target node').fill('3');
  await pane.getByRole('button', { name: 'Check' }).click();
  await pane.getByText(/connected to/i).waitFor();
  await shot(page, 'browser');

  // Browser: a high-degree node, neighbors paged in a bounded box.
  page = await open('browser?node=100');
  await page.getByText(/^1–25 of 150$/).waitFor();
  await shot(page, 'browser-hub');

  // Browser in the light theme: edges that do not carry connectivity.
  page = await open('browser?view=edges&state=ineffective&edge=2-3', 'light');
  await page.getByRole('region', { name: 'Edge 2 – 3' }).getByText('Disabled').first().waitFor();
  await shot(page, 'browser-light');

  // Connection details: server (backend) versus console host (frontend).
  page = await open('browser?node=18446744073709551615');
  await page.locator('.connection-button').click();
  await page.getByText('Server (backend)').waitFor();
  await shot(page, 'connection');

  // Workbench: a connectivity query.
  page = await open('workbench');
  const query = page.getByRole('region', { name: 'Query' });
  await query.getByLabel('Source').fill('1');
  await query.getByLabel('Target').fill('18446744073709551615');
  await query.getByRole('button', { name: 'Run query' }).click();
  await query.locator('.answer').waitFor();
  await shot(page, 'workbench');

  // Performance: needs a few scrapes of /metrics and some query traffic.
  page = await open('performance');
  for (let round = 0; round < 6; round += 1) {
    await page.evaluate(async () => {
      for (let i = 0; i < 40; i += 1) {
        await fetch('api/v1/operations', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ op: 'connected', source: String(10 + (i % 10)), target: '1' }),
        });
      }
    });
    await page.waitForTimeout(5_000);
  }
  await shot(page, 'performance');

  page = await open('overview');
  await page.waitForTimeout(1_000);
  await shot(page, 'overview');
} finally {
  await browser.close();
}
