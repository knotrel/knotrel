import { expect, test } from '@playwright/test';
import { KnotrelServer, startConsole } from './harness';

const MAX = '18446744073709551615';
const BEYOND_JS = '9007199254740993';

test.describe('operator journey on a standalone server with lifecycle and inspection', () => {
  let server: KnotrelServer;
  let ui: { url: string; close: () => Promise<void> };

  test.beforeAll(async () => {
    server = await KnotrelServer.start({
      KNOTREL_TOPOLOGY_LIFECYCLE: 'true',
      KNOTREL_INSPECTION: 'true',
      KNOTREL_METRICS_ENABLED: 'true',
    });
    ui = await startConsole(server.url);
    const ops = [
      { op: 'link', source: '1', target: '2' },
      { op: 'link', source: '2', target: '3' },
      { op: 'link', source: '3', target: MAX },
      { op: 'link', source: BEYOND_JS, target: MAX },
      { op: 'set_edge_enabled', source: '2', target: '3', enabled: false },
    ];
    expect((await server.call('POST', '/v1/batch', { operations: ops })).status).toBe(200);
  });
  test.afterAll(async () => {
    await ui?.close();
    await server?.stop();
  });

  test('find a node, inspect its links, query, apply an operation and verify', async ({ page }) => {
    await page.goto(`${ui.url}#/browser`);
    const list = page.locator('.browser-list');
    // 1. Discover nodes without knowing their IDs; exact u64 values.
    await expect(list.getByRole('button', { name: MAX })).toBeVisible();
    await expect(list.getByRole('button', { name: BEYOND_JS })).toBeVisible();
    await expect(list.getByText(/5 nodes loaded/)).toBeVisible();

    // 2. Find by exact ID and inspect incident edges.
    await page.getByLabel('Find node by exact ID').fill(MAX);
    await page.getByRole('button', { name: 'Open', exact: true }).click();
    const maxPane = page.getByRole('region', { name: `Node ${MAX}` });
    await expect(maxPane.getByText('Registered degree')).toBeVisible();
    await expect(maxPane.getByRole('button', { name: BEYOND_JS })).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`node=${MAX}`));

    // Navigate node -> neighbor -> edge -> endpoint.
    await maxPane.getByRole('button', { name: '3', exact: true }).click();
    const pane3 = page.getByRole('region', { name: 'Node 3' });
    await expect(pane3.getByText('not effective: disabled')).toBeVisible();
    await pane3
      .getByRole('row', { name: /^2 / })
      .getByRole('button', { name: 'Open edge' })
      .click();
    const edge = page.getByRole('region', { name: 'Edge 3 – 2' });
    await expect(edge.getByText('Disabled')).toBeVisible();
    await expect(edge.getByText('Not effective')).toBeVisible();
    await edge.getByRole('button', { name: '2', exact: true }).click();
    const pane2 = page.getByRole('region', { name: 'Node 2' });
    await expect(pane2).toBeVisible();

    // 3. Query from the selection in the Workbench.
    await pane2.getByRole('button', { name: 'Check connectivity from here…' }).click();
    await expect(page.getByRole('tab', { name: 'Workbench' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    const query = page.getByRole('region', { name: 'Query' });
    await expect(query.getByLabel('Source')).toHaveValue('2');
    await query.getByLabel('Target').fill(MAX);
    await query.getByRole('button', { name: 'Run query' }).click();
    await expect(query.locator('.answer')).toHaveText(/Not connected/);
    await expect(query.getByText(/Browser round trip/)).toBeVisible();

    // Back to the browser: the selection survived navigation.
    await page.getByRole('tab', { name: 'Browser' }).click();
    await expect(page.getByRole('region', { name: 'Node 2' })).toBeVisible();

    // 4. Apply an allowed operation on the selection: enable edge 2–3.
    await pane2.getByRole('row', { name: /^3 / }).getByRole('button', { name: 'Enable' }).click();
    await expect(pane2.getByText('Enable edge 2 – 3')).toBeVisible();
    await expect(pane2.getByText('Effective', { exact: true }).first()).toBeVisible();

    // 5. Verify: the detail was re-read, the server agrees, and the query flips.
    await expect(pane2.getByRole('row', { name: /^3 / }).getByText('effective')).toBeVisible();
    const state = await server.call('POST', '/v1/operations', {
      op: 'edge_state',
      source: '2',
      target: '3',
    });
    expect(state.json).toMatchObject({ enabled: true, effective: true });
    await page.getByRole('tab', { name: 'Workbench' }).click();
    await query.getByRole('button', { name: 'Run query' }).click();
    await expect(query.locator('.answer')).toHaveText(/^.?\s*Connected/);
    const history = page.getByRole('region', { name: 'Console history' });
    await expect(history.getByText('Enable edge 2 – 3')).toBeVisible();
    await expect(history.getByText(/not the server's activity/)).toBeVisible();
  });

  test('deactivating a node is reflected in its edges and the node list filter', async ({
    page,
  }) => {
    await page.goto(`${ui.url}#/browser?node=${BEYOND_JS}`);
    const pane = page.getByRole('region', { name: `Node ${BEYOND_JS}` });
    await pane.getByRole('button', { name: 'Deactivate' }).click();
    await expect(pane.getByText('Inactive', { exact: true })).toBeVisible();
    await expect(pane.getByText('not effective: this node inactive')).toBeVisible();
    await page
      .locator('.browser-list')
      .getByLabel(/Filter/)
      .selectOption('inactive');
    await expect(
      page.locator('.browser-list').getByRole('button', { name: BEYOND_JS }),
    ).toBeVisible();
    await expect(page).toHaveURL(/state=inactive/);
    await page.getByRole('button', { name: 'Edges' }).click();
    await page
      .locator('.browser-list')
      .getByLabel(/Filter/)
      .selectOption('ineffective');
    await expect(
      page.locator('.browser-list').getByRole('button', { name: `${BEYOND_JS} – ${MAX}` }),
    ).toBeVisible();
  });

  test('a batch with an invalid element reports per-operation results', async ({ page }) => {
    await page.goto(`${ui.url}#/workbench`);
    const batch = page.getByRole('region', { name: 'Batch' });
    await batch.getByLabel('Batch request JSON').fill(
      JSON.stringify({
        operations: [
          { op: 'add_node', node: '500' },
          { op: 'link', source: '500', target: '999999' },
          { op: 'set_edge_enabled', source: '500', target: '1', enabled: false },
          { op: 'connected', source: '500', target: '999999' },
        ],
      }),
    );
    await expect(batch.getByText('Valid', { exact: true })).toBeVisible();
    await batch.getByRole('button', { name: 'Send batch (changes the graph)' }).click();
    await expect(batch.getByText('3 ok, 1 error')).toBeVisible();
    await expect(batch.getByText('unknown_edge').first()).toBeVisible();
    await expect(batch.getByText(/There is no rollback/)).toBeVisible();
    await batch.getByText('Raw response').click();
    await expect(batch.locator('pre.code')).toContainText('"results"');
    await batch.getByLabel('Batch request JSON').fill('[{"op":"link","source":1,"target":"2"}]');
    await expect(batch.getByText(/numbers lose precision/)).toBeVisible();
    await expect(batch.getByRole('button', { name: 'Send batch' })).toBeDisabled();
  });

  test('performance metrics come from the real server', async ({ page }) => {
    await page.goto(`${ui.url}#/performance`);
    const perf = page.getByRole('region', { name: 'Performance' });
    await expect(perf.getByText('/v1/stats')).toBeVisible({ timeout: 20_000 });
    await expect(perf.getByText('Graph jobs outstanding / capacity')).toBeVisible();
    await expect(perf.getByRole('cell', { name: 'inspect' })).toBeVisible();
  });
});
