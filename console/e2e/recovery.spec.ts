import { expect, test } from '@playwright/test';
import { KnotrelServer, startConsole } from './harness';

test('reports a lost server connection and recovers after a restart', async ({ page }) => {
  const server = await KnotrelServer.start({});
  const ui = await startConsole(server.url);
  try {
    await page.goto(ui.url);
    const status = page.getByRole('status');
    await expect(status.getByText('Connected')).toBeVisible();
    await server.call('POST', '/v1/operations', { op: 'add_node', node: '1' });

    await server.stop();
    await page.getByRole('button', { name: 'Refresh now' }).click();
    await expect(status.getByText('Server unreachable')).toBeVisible();
    await expect(page.getByText(/request was not delivered/).first()).toBeVisible();
    await expect(page.getByText('Stale data').first()).toBeVisible();

    // A mutation against the unreachable server is reported as not delivered.
    await page.getByRole('tab', { name: 'Operations' }).click();
    await page.getByRole('region', { name: 'Node' }).getByLabel('Node ID').fill('2');
    await page.getByRole('button', { name: 'Add node' }).click();
    await expect(
      page.getByRole('region', { name: 'Last result' }).getByText('Rejected'),
    ).toBeVisible();

    await server.restart();
    await page.getByRole('tab', { name: 'Overview' }).click();
    await page.getByRole('button', { name: 'Refresh now' }).click();
    await expect(status.getByText('Connected')).toBeVisible();
    await expect(page.getByText('Server instance changed')).toBeVisible();
    const graph = page.getByRole('region', { name: 'Graph' });
    await expect(graph.getByText(/^0$/).first()).toBeVisible();
  } finally {
    await ui.close();
    await server.stop();
  }
});
