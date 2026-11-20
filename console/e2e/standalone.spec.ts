import { expect, test, type Page } from '@playwright/test';
import { KnotrelServer, startConsole } from './harness';

const MAX = '18446744073709551615';

async function fillNode(page: Page, section: string, label: string, value: string) {
  const field = page.getByRole('region', { name: section }).getByLabel(label, { exact: true });
  await field.fill(value);
}

async function connected(page: Page, source: string, target: string) {
  await page.getByRole('tab', { name: 'Graph/Queries' }).click();
  const card = page.getByRole('region', { name: 'Connectivity' });
  await card.getByLabel('Source').fill(source);
  await card.getByLabel('Target').fill(target);
  await card.getByRole('button', { name: 'Run query' }).click();
  return card.locator('.answer');
}

test.describe('standalone, lifecycle enabled', () => {
  let server: KnotrelServer;
  let ui: { url: string; close: () => Promise<void> };

  test.beforeAll(async () => {
    server = await KnotrelServer.start({ KNOTREL_TOPOLOGY_LIFECYCLE: 'true' });
    ui = await startConsole(server.url);
  });
  test.afterAll(async () => {
    await ui?.close();
    await server?.stop();
  });

  test('overview reports the real server configuration', async ({ page }) => {
    await page.goto(ui.url);
    await expect(page.getByRole('status').getByText('Connected')).toBeVisible();
    const serverCard = page.getByRole('region', { name: 'Server' });
    await expect(serverCard.getByText('standalone')).toBeVisible();
    await expect(serverCard.getByText('compact-bfs')).toBeVisible();
    await expect(serverCard.getByText('enabled', { exact: true })).toBeVisible();
    await expect(serverCard.getByText(/not exposed by the Knotrel HTTP API/)).toBeVisible();
    await expect(page.getByText('Process alive')).toBeVisible();
    await expect(
      page
        .getByRole('region', { name: 'Liveness and readiness' })
        .getByText('Ready', { exact: true }),
    ).toBeVisible();
    const instance = (await server.call('GET', '/v1/stats')).json;
    expect(instance.state_version).toBeDefined();
  });

  test('lifecycle changes are reflected by queries and statistics', async ({ page }) => {
    await page.goto(`${ui.url}#operations`);
    const last = page.getByRole('region', { name: 'Last result' });

    await fillNode(page, 'Edge', 'Source', '1');
    await fillNode(page, 'Edge', 'Target', MAX);
    await page.getByRole('button', { name: 'Link' }).click();
    await expect(last.getByText('Effective')).toBeVisible();
    await page.getByRole('button', { name: 'Link' }).click();
    await expect(last.getByText('No-op')).toBeVisible();

    await expect(await connected(page, MAX, '1')).toHaveText(/Connected/);

    await page.getByRole('tab', { name: 'Operations' }).click();
    await fillNode(page, 'Node', 'Node ID', MAX);
    await page.getByRole('button', { name: 'Deactivate' }).click();
    await expect(last.getByText('Effective')).toBeVisible();
    await expect(await connected(page, MAX, '1')).toHaveText(/Not connected/);

    const edgeCard = page.getByRole('region', { name: 'Edge state' });
    await edgeCard.getByLabel('Source').fill('1');
    await edgeCard.getByLabel('Target').fill(MAX);
    await edgeCard.getByRole('button', { name: 'Run query' }).click();
    await expect(edgeCard.getByText('Registered, enabled, not effective')).toBeVisible();

    await page.getByRole('tab', { name: 'Operations' }).click();
    await page.getByRole('button', { name: 'Activate', exact: true }).click();
    await expect(last.getByText('Effective')).toBeVisible();
    await fillNode(page, 'Edge', 'Source', '1');
    await fillNode(page, 'Edge', 'Target', MAX);
    await page.getByRole('button', { name: 'Disable', exact: true }).click();
    await expect(last.getByText('Effective')).toBeVisible();
    await expect(await connected(page, MAX, '1')).toHaveText(/Not connected/);

    await page.getByRole('tab', { name: 'Operations' }).click();
    await page.getByRole('button', { name: 'Cut' }).click();
    await expect(last.getByText('Effective')).toBeVisible();
    await fillNode(page, 'Edge', 'Target', '1');
    await page.getByRole('button', { name: 'Link' }).click();
    await expect(last.getByText('Rejected')).toBeVisible();
    await expect(last.getByText('self_loop')).toBeVisible();

    const stats = (await server.call('GET', '/v1/stats')).json;
    await page.getByRole('tab', { name: 'Overview' }).click();
    await page.getByRole('button', { name: 'Refresh now' }).click();
    const graph = page.getByRole('region', { name: 'Graph' });
    await expect(graph.getByText(String(stats.state_version), { exact: true })).toBeVisible();
    await expect(graph.getByText(MAX)).toHaveCount(0);
    expect(stats.node_count).toBe(2);
    expect(stats.edge_count).toBe(0);
  });

  test('permanent removal requires typing the node ID', async ({ page }) => {
    await server.call('POST', '/v1/operations', { op: 'link', source: '7', target: '8' });
    await page.goto(`${ui.url}#operations`);
    await fillNode(page, 'Node', 'Node ID', '7');
    await page.getByRole('button', { name: 'Remove permanently…' }).click();
    const dialog = page.getByRole('alertdialog');
    await expect(dialog.getByRole('button', { name: 'Remove node' })).toBeDisabled();
    await dialog.getByRole('textbox').fill('7');
    await dialog.getByRole('button', { name: 'Remove node' }).click();
    await expect(
      page.getByRole('region', { name: 'Last result' }).getByText('Effective'),
    ).toBeVisible();
    const state = await server.call('POST', '/v1/operations', { op: 'node_state', node: '7' });
    expect(state.json).toMatchObject({ registered: false });
    await expect(page.getByRole('table').getByText('remove node 7')).toBeVisible();
  });
});

test.describe('standalone, lifecycle disabled', () => {
  let server: KnotrelServer;
  let ui: { url: string; close: () => Promise<void> };

  test.beforeAll(async () => {
    server = await KnotrelServer.start({
      KNOTREL_TOPOLOGY_LIFECYCLE: 'false',
      KNOTREL_ENGINE: 'ett',
    });
    ui = await startConsole(server.url);
  });
  test.afterAll(async () => {
    await ui?.close();
    await server?.stop();
  });

  test('unsupported actions are disabled with an explanation', async ({ page }) => {
    await page.goto(ui.url);
    await expect(
      page.getByRole('region', { name: 'Server' }).getByText('(experimental)'),
    ).toBeVisible();
    await expect(page.getByText(/requires lifecycle support/).first()).toBeVisible();
    await page.getByRole('tab', { name: 'Operations' }).click();
    await page.getByRole('region', { name: 'Node' }).getByLabel('Node ID').fill('1');
    await expect(page.getByRole('button', { name: 'Add node' })).toBeEnabled();
    for (const name of ['Activate', 'Deactivate', 'Remove permanently…', 'Enable', 'Disable']) {
      await expect(page.getByRole('button', { name, exact: true })).toBeDisabled();
    }
    await page.getByRole('button', { name: 'Add node' }).click();
    await expect(
      page.getByRole('region', { name: 'Last result' }).getByText('Effective'),
    ).toBeVisible();
    await page.getByRole('tab', { name: 'Graph/Queries' }).click();
    await expect(
      page.getByRole('region', { name: 'Node state' }).getByLabel('Node'),
    ).toBeDisabled();
    await expect(page.getByText(/Unsupported: node and edge state queries/).first()).toBeVisible();
  });
});
