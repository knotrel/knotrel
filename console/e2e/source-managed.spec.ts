import { expect, test } from '@playwright/test';
import { KnotrelServer, startConsole } from './harness';

test.describe('source-managed diagnostics', () => {
  let server: KnotrelServer;
  let ui: { url: string; close: () => Promise<void> };

  test.beforeAll(async () => {
    server = await KnotrelServer.start({ KNOTREL_MODE: 'source_managed' });
    ui = await startConsole(server.url);
  });
  test.afterAll(async () => {
    await ui?.close();
    await server?.stop();
  });

  test('follows synchronization driven by an external feeder, without mutation controls', async ({
    page,
  }) => {
    await page.goto(`${ui.url}#sync`);
    const sync = page.getByRole('region', { name: 'Synchronization' });
    await expect(sync.getByText('Uninitialized', { exact: true }).first()).toBeVisible();
    await expect(sync.getByText(/Start the feeder/)).toBeVisible();
    await expect(sync.getByText(/not inferred from readiness/)).toBeVisible();

    // The test acts as the feeder, directly against the server.
    const instance = String((await server.call('GET', '/v1/info')).json.instance_id);
    const header = { 'x-knotrel-instance-id': instance };
    const fence = { instance_id: instance, feeder_session_id: 'e2e', feeder_epoch: 1 };
    expect(
      (
        await server.call(
          'POST',
          '/v1/feeder/session',
          { ...fence, lease_duration_ms: 60000 },
          header,
        )
      ).status,
    ).toBe(200);
    const imported = await server.call(
      'POST',
      '/v1/feeder/import',
      {
        ...fence,
        mode: 'create_only',
        graph_id: 'default',
        source_id: 'e2e-source',
        source_epoch: '1',
        source_position: 'lsn/0001',
        nodes: ['1', '2', '3'],
        edges: [{ source: '1', target: '2' }],
      },
      header,
    );
    expect(imported.status).toBe(201);

    await page.getByRole('button', { name: 'Refresh now' }).click();
    await expect(sync.getByText('Catching up', { exact: true }).first()).toBeVisible();
    await expect(sync.getByText('lsn/0001')).toBeVisible();

    const barrier = await server.call(
      'POST',
      '/v1/feeder/barrier',
      {
        ...fence,
        graph_id: 'default',
        generation: 1,
        barrier_delivery_sequence: 0,
        barrier_source_position: 'lsn/0001',
      },
      header,
    );
    expect(barrier.status).toBe(200);
    await page.getByRole('button', { name: 'Refresh now' }).click();
    await expect(sync.getByText('Synchronized', { exact: true }).first()).toBeVisible();
    await expect(sync.getByText(instance)).toBeVisible();
    await expect(sync.getByRole('button')).toHaveCount(0);

    await page.getByRole('tab', { name: 'Graph/Queries' }).click();
    const card = page.getByRole('region', { name: 'Connectivity' });
    await card.getByLabel('Source').fill('1');
    await card.getByLabel('Target').fill('2');
    await card.getByRole('button', { name: 'Run query' }).click();
    await expect(card.locator('.answer')).toHaveText(/Connected/);
    await expect(card.getByText('lsn/0001')).toBeVisible();

    await page.getByRole('tab', { name: 'Operations' }).click();
    await expect(
      page.getByRole('region', { name: 'Operations' }).getByText(/authoritative source/),
    ).toBeVisible();
    await expect(page.getByRole('button', { name: /Add node|Link|Cut|Remove/ })).toHaveCount(0);

    // The server itself rejects direct mutations in this mode.
    const rejected = await server.call('POST', '/v1/operations', { op: 'add_node', node: '9' });
    expect(rejected.json.code).toBe('direct_mutation_disabled');
  });
});
