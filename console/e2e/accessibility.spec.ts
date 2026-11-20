import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import { KnotrelServer, startConsole } from './harness';

test.describe('accessibility and keyboard use', () => {
  let server: KnotrelServer;
  let ui: { url: string; close: () => Promise<void> };

  test.beforeAll(async () => {
    server = await KnotrelServer.start({
      KNOTREL_TOPOLOGY_LIFECYCLE: 'true',
      KNOTREL_INSPECTION: 'true',
      KNOTREL_METRICS_ENABLED: 'true',
    });
    await server.call('POST', '/v1/operations', { op: 'link', source: '4', target: '5' });
    ui = await startConsole(server.url);
  });
  test.afterAll(async () => {
    await ui?.close();
    await server?.stop();
  });

  for (const page of ['overview', 'browser?node=4', 'workbench', 'performance']) {
    test(`${page} has no serious or critical axe violations`, async ({ page: browser }) => {
      await browser.goto(`${ui.url}#/${page}`);
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

  test('tabs, lists, queries and confirmations work with the keyboard only', async ({ page }) => {
    await page.goto(ui.url);
    await expect(page.getByRole('status').getByText('Connected')).toBeVisible();
    await page.getByRole('tab', { name: 'Overview' }).focus();
    await page.keyboard.press('ArrowRight');
    await expect(page.getByRole('tab', { name: 'Browser' })).toBeFocused();
    await expect(page.getByRole('tab', { name: 'Browser' })).toHaveAttribute(
      'aria-selected',
      'true',
    );

    // List navigation with arrow keys, open with Enter.
    const first = page.locator('.browser-list').getByRole('button', { name: '4', exact: true });
    await first.focus();
    await page.keyboard.press('ArrowDown');
    await expect(
      page.locator('.browser-list').getByRole('button', { name: '5', exact: true }),
    ).toBeFocused();
    await page.keyboard.press('Enter');
    const pane = page.getByRole('region', { name: 'Node 5' });
    await expect(pane).toBeVisible();

    // Confirmation dialog: focus moves in, Escape cancels and restores focus.
    // Destructive actions sit in a menu: arrows move, Enter selects.
    const more = pane.getByRole('button', { name: 'More node actions' });
    await more.focus();
    await page.keyboard.press('Enter');
    await expect(
      page.getByRole('menuitem', { name: 'Query node state in Workbench' }),
    ).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(page.getByRole('menuitem', { name: 'Remove permanently…' })).toBeFocused();
    await page.keyboard.press('Enter');
    const dialog = page.getByRole('alertdialog');
    await expect(dialog.getByRole('textbox')).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
    await expect(more).toBeFocused();

    // Connectivity from the pane by keyboard.
    await pane.getByLabel('Target node').focus();
    await page.keyboard.type('4');
    await page.keyboard.press('Enter');
    await expect(pane.getByText(/^Connected to/)).toBeVisible();

    // The splitter resizes the detail pane from the keyboard.
    const splitter = page.getByRole('separator', { name: 'Resize detail pane' });
    const before = Number(await splitter.getAttribute('aria-valuenow'));
    await splitter.focus();
    await page.keyboard.press('ArrowLeft');
    await expect(splitter).toHaveAttribute('aria-valuenow', String(before + 16));

    // Workbench query by keyboard.
    await page.getByRole('tab', { name: 'Browser' }).focus();
    await page.keyboard.press('ArrowRight');
    const query = page.getByRole('region', { name: 'Query' });
    await query.getByLabel('Source').focus();
    await page.keyboard.type('4');
    await page.keyboard.press('Tab');
    await expect(query.getByLabel('Target')).toBeFocused();
    await page.keyboard.type('5');
    await page.keyboard.press('Enter');
    await expect(query.locator('.answer')).toHaveText(/Connected/);
  });
});
