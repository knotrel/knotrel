import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import { KnotrelServer, startConsole } from './harness';

test.describe('accessibility and keyboard use', () => {
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

  for (const page of ['overview', 'queries', 'sync', 'operations']) {
    test(`${page} has no serious or critical axe violations`, async ({ page: browser }) => {
      await browser.goto(`${ui.url}#${page}`);
      await expect(browser.getByRole('status').getByText('Connected')).toBeVisible();
      const results = await new AxeBuilder({ page: browser })
        .withTags(['wcag2a', 'wcag2aa'])
        .analyze();
      const blocking = results.violations.filter(
        (v) => v.impact === 'serious' || v.impact === 'critical',
      );
      expect(blocking.map((v) => `${v.id}: ${v.nodes.length}`)).toEqual([]);
    });
  }

  test('tabs, queries and confirmations work with the keyboard only', async ({ page }) => {
    await page.goto(ui.url);
    await expect(page.getByRole('status').getByText('Connected')).toBeVisible();
    await page.getByRole('tab', { name: 'Overview' }).focus();
    await page.keyboard.press('ArrowRight');
    await expect(page.getByRole('tab', { name: 'Graph/Queries' })).toBeFocused();
    await expect(page.getByRole('tab', { name: 'Graph/Queries' })).toHaveAttribute(
      'aria-selected',
      'true',
    );

    await server.call('POST', '/v1/operations', { op: 'link', source: '4', target: '5' });
    const card = page.getByRole('region', { name: 'Connectivity' });
    await card.getByLabel('Source').focus();
    await page.keyboard.type('4');
    await page.keyboard.press('Tab');
    await expect(card.getByLabel('Target')).toBeFocused();
    await page.keyboard.type('5');
    await page.keyboard.press('Enter');
    await expect(card.locator('.answer')).toHaveText(/Connected/);

    await page.getByRole('tab', { name: 'Graph/Queries' }).focus();
    await page.keyboard.press('End');
    await expect(page.getByRole('tab', { name: 'Operations' })).toBeFocused();
    await page.getByRole('region', { name: 'Node' }).getByLabel('Node ID').fill('5');
    await page.getByRole('button', { name: 'Remove permanently…' }).focus();
    await page.keyboard.press('Enter');
    const dialog = page.getByRole('alertdialog');
    await expect(dialog.getByRole('textbox')).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Remove permanently…' })).toBeFocused();
  });
});
