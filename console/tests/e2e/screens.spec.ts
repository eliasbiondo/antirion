import { expect, test, watchCsp } from './fixtures';

const SCREENS: [string, string][] = [
  ['', 'Overview'],
  ['request-log/', 'Request log'],
  ['analytics/', 'Analytics'],
  ['alerts/', 'Alerts'],
  ['budgets/', 'Budgets'],
  ['safety/', 'Safety & PII'],
  ['keys/', 'API keys'],
  ['teams/', 'Teams & users'],
  ['models/', 'Models'],
  ['cache/', 'Cache'],
  ['routing/', 'Routing policy'],
  ['providers/', 'Providers'],
  ['change-requests/', 'Change requests'],
];

test('every screen renders under the console CSP without broken states', async ({ signedIn: page }) => {
  const csp = watchCsp(page);
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(e.message));
  for (const [path, title] of SCREENS) {
    await page.goto(path);
    await expect(page.getByRole('heading', { level: 1, name: title })).toBeVisible();
  }
  // Settings is a dialog over the page; its old address opens it at the section asked for.
  await page.goto('settings/?section=audit');
  const settings = page.getByRole('dialog', { name: 'Gateway settings' });
  await expect(settings.getByRole('heading', { name: 'Audit log' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(settings).toBeHidden();
  await expect(page.getByRole('heading', { level: 1, name: 'Overview' })).toBeVisible();
  expect(csp).toEqual([]);
  expect(errors).toEqual([]);
});

test('Gateway settings opens over the current page from the account menu and keeps the page', async ({
  signedIn: page,
}) => {
  await page.goto('keys/');
  await page.getByRole('button', { name: /^Account menu for/ }).click();
  await page.getByRole('menuitem', { name: 'Gateway settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Gateway settings' });
  await expect(settings).toBeVisible();
  await expect(page).toHaveURL(/\/keys\/\?.*settings=general/);
  await settings.getByRole('button', { name: 'Configuration' }).click();
  await expect(settings.getByRole('heading', { name: 'Configuration' })).toBeVisible();
  await expect(page).toHaveURL(/settings=configuration/);
  await settings.getByRole('button', { name: 'Close settings' }).click();
  await expect(settings).toBeHidden();
  await expect(page.getByRole('heading', { level: 1, name: 'API keys' })).toBeVisible();
  await page.goto('settings/?section=providers');
  await expect(page.getByRole('heading', { level: 1, name: 'Providers' })).toBeVisible();
});

test('the time range and scope live in the URL', async ({ signedIn: page }) => {
  await page.goto('budgets/?range=7d');
  await expect(page.getByRole('button', { name: /Last 7 days/ })).toBeVisible();
  await expect(page).toHaveURL(/org=gateway/);
  await page.getByRole('button', { name: /Last 7 days/ }).click();
  await page.getByRole('button', { name: /Last 1 hour/ }).click();
  await expect(page).toHaveURL(/range=1h/);
});

test('keyboard: ? lists shortcuts and Escape closes it', async ({ signedIn: page }) => {
  await page.goto('keys/');
  await page.keyboard.press('Shift+?');
  await expect(page.getByRole('dialog', { name: 'Keyboard shortcuts' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Keyboard shortcuts' })).toBeHidden();
  await page.keyboard.press('ControlOrMeta+k');
  await expect(page.getByRole('dialog', { name: 'Search' })).toBeVisible();
});

test('a page header never squeezes its title to make room for the actions', async ({ signedIn: page }) => {
  for (const width of [390, 700, 900]) {
    await page.setViewportSize({ width, height: 800 });
    for (const [path, title] of [
      ['models/', 'Models'],
      ['teams/', 'Teams & users'],
      ['budgets/', 'Budgets'],
    ]) {
      await page.goto(path ?? '');
      const heading = page.getByRole('heading', { level: 1, name: title });
      await expect(heading).toBeVisible();
      const header = page.locator('header').filter({ has: heading });
      const block = heading.locator('xpath=ancestor::div[2]');
      const box = await block.boundingBox();
      const headerBox = await header.boundingBox();
      expect(box && headerBox, `${path} at ${width}`).toBeTruthy();
      if (!box || !headerBox) continue;
      expect(box.width, `${path} at ${width}: the title block keeps its room`).toBeGreaterThanOrEqual(
        Math.min(384, headerBox.width - 64),
      );
    }
  }
});
