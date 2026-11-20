import { expect, test } from '@playwright/test';
import { KnotrelServer, startConsole } from './harness';

test.describe('browser on an empty, then growing, plain graph', () => {
  let server: KnotrelServer;
  let ui: { url: string; close: () => Promise<void> };

  test.beforeAll(async () => {
    server = await KnotrelServer.start({ KNOTREL_INSPECTION: 'true' });
    ui = await startConsole(server.url);
  });
  test.afterAll(async () => {
    await ui?.close();
    await server?.stop();
  });

  test('empty graph, paging, changes during navigation and restart detection', async ({ page }) => {
    await page.goto(`${ui.url}#/browser`);
    const list = page.locator('.browser-list');
    await expect(list.getByText('The graph has no nodes.')).toBeVisible();
    await expect(list.getByText(/No state filters/)).toBeVisible();

    // 250 nodes: three pages of 100.
    const ops = Array.from({ length: 250 }, (_, i) => ({ op: 'add_node', node: String(1000 + i) }));
    await server.call('POST', '/v1/batch', { operations: ops });
    await list.getByRole('button', { name: 'Restart listing' }).click();
    await expect(list.getByText(/100 nodes loaded \(1 page\)/)).toBeVisible();

    // Change the graph between pages: a node added beyond the cursor appears.
    await server.call('POST', '/v1/operations', { op: 'add_node', node: '99999' });
    await list.getByRole('button', { name: 'Load more' }).click();
    await list.getByRole('button', { name: 'Load more' }).click();
    await expect(list.getByText('End of list.')).toBeVisible();
    await expect(list.getByRole('button', { name: '99999' })).toBeVisible();
    await expect(list.getByText(/251 nodes loaded \(3 pages\)/)).toBeVisible();

    // Restart the server between pages: the listing reports mixed graphs.
    await list.getByRole('button', { name: 'Restart listing' }).click();
    await expect(list.getByText(/100 nodes loaded/)).toBeVisible();
    await server.stop();
    await server.restart();
    await server.call('POST', '/v1/batch', { operations: ops });
    await list.getByRole('button', { name: 'Load more' }).click();
    await expect(page.getByText('Graph changed while browsing')).toBeVisible();
  });
});

test('experimental engine without lifecycle: nodes only, explained', async ({ page }) => {
  const server = await KnotrelServer.start({ KNOTREL_INSPECTION: 'true', KNOTREL_ENGINE: 'ett' });
  const ui = await startConsole(server.url);
  try {
    await server.call('POST', '/v1/operations', { op: 'link', source: '1', target: '2' });
    await page.goto(`${ui.url}#/browser?node=1`);
    const pane = page.getByRole('region', { name: 'Node 1' });
    await expect(pane.getByText(/not reported by this engine/)).toBeVisible();
    await expect(pane.getByText(/keeps no per-node adjacency/)).toBeVisible();
    await page.getByRole('button', { name: 'Edges' }).click();
    await expect(page.getByText('Edge list unavailable')).toBeVisible();
  } finally {
    await ui.close();
    await server.stop();
  }
});

test('server without inspection: the browser explains how to enable it', async ({ page }) => {
  const server = await KnotrelServer.start({});
  const ui = await startConsole(server.url);
  try {
    await page.goto(`${ui.url}#/browser`);
    await expect(page.getByText('Node list unavailable')).toBeVisible();
    await expect(page.locator('.browser-list').getByText(/KNOTREL_INSPECTION=true/)).toBeVisible();
    await page.goto(`${ui.url}#/performance`);
    await expect(page.getByText('Metrics disabled')).toBeVisible();
  } finally {
    await ui.close();
    await server.stop();
  }
});
