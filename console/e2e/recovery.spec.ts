import { expect, test } from '@playwright/test';
import { KnotrelServer, startConsole, refreshNow } from './harness';

test('reports a lost server connection and recovers after a restart', async ({ page }) => {
  const server = await KnotrelServer.start({ KNOTREL_INSPECTION: 'true' });
  const ui = await startConsole(server.url);
  try {
    await page.goto(ui.url);
    const status = page.getByRole('status');
    await expect(status.getByText('Connected')).toBeVisible();
    await server.call('POST', '/v1/operations', { op: 'add_node', node: '1' });

    await server.stop();
    await refreshNow(page);
    await expect(status.getByText('Server unreachable')).toBeVisible();
    await expect(page.getByText(/request was not delivered/).first()).toBeVisible();
    await expect(page.getByText(/Displayed data is stale/)).toBeVisible();

    // A mutation against the unreachable server is reported as not delivered.
    await page.getByRole('tab', { name: 'Browser' }).click();
    await page.getByRole('button', { name: 'New node' }).click();
    const dialog = page.getByRole('dialog', { name: 'New node' });
    await dialog.getByLabel('Node ID').fill('2');
    await dialog.getByRole('button', { name: 'Create node' }).click();
    await expect(dialog.getByText('Rejected')).toBeVisible();
    await expect(dialog.getByText(/not delivered/)).toBeVisible();
    await dialog.getByRole('button', { name: 'Cancel' }).click();

    await server.restart();
    await page.getByRole('tab', { name: 'Overview' }).click();
    await refreshNow(page);
    await expect(status.getByText('Connected')).toBeVisible();
    await expect(page.getByText('Server instance changed')).toBeVisible();
    await expect(
      page.getByRole('region', { name: 'Graph' }).getByText(/^0$/).first(),
    ).toBeVisible();
  } finally {
    await ui.close();
    await server.stop();
  }
});
