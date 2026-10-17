import { admin, expect, gatewayStep, test, totp, unique } from './fixtures';

const PASSWORD = 'Scoped-developer-2026';

test('a developer on one team sees only the pages it can read, and the console asks for nothing it would refuse', async ({
  page,
  request,
}) => {
  const team = unique('devteam');
  expect(
    (await admin(request, 'POST', '/teams', { name: team, spec: { organization: 'default' } })).ok(),
  ).toBeTruthy();
  const email = `${unique('developer')}@example.com`;
  const created = await admin(request, 'POST', '/invitations', {
    email,
    display_name: 'Scoped Developer',
    role_bindings: [{ role: 'developer', scope: `team/${team}` }],
  });
  expect(created.status()).toBe(201);
  const { link } = (await created.json()) as { link: string };
  // The mailed link names the gateway's own address; open the same page on the console under test.
  await page.goto(link.replace(/^https?:\/\/[^/]+\/ui\//, ''));
  await page.getByLabel('New password', { exact: true }).fill(PASSWORD);
  await page.getByLabel('Confirm password', { exact: true }).fill(PASSWORD);
  await page.getByRole('button', { name: 'Set password and continue' }).click();
  const secret = (await page.locator('[data-secret]').textContent()) ?? '';
  await page.getByLabel('Verification code').fill(totp(secret, await gatewayStep(request)));
  await page.getByLabel("I've saved my recovery codes").check();
  await page.getByRole('button', { name: 'Continue to Antirion' }).click();
  const nav = page.getByRole('navigation', { name: 'Console' });
  await expect(nav).toBeVisible();

  const refused: string[] = [];
  page.on('response', (r) => {
    if (r.status() === 403 && r.url().includes('/admin/v1/'))
      refused.push(`${r.request().method()} ${r.url()}`);
  });

  for (const hidden of ['Models', 'Providers', 'Alerts', 'Teams & users', 'Cache', 'Routing policy'])
    await expect(nav.getByRole('link', { name: new RegExp(`^${hidden}`) })).toHaveCount(0);
  for (const shown of ['Overview', 'Request log', 'Analytics', 'API keys'])
    await expect(nav.getByRole('link', { name: new RegExp(`^${shown}`) })).toBeVisible();

  for (const path of ['', 'request-log/', 'analytics/', 'keys/']) {
    await page.goto(path);
    await expect(nav).toBeVisible();
    await page.waitForLoadState('networkidle');
  }
  // A page it can't read, opened by its address, says so instead of calling the gateway.
  await page.goto('models/');
  await expect(page.getByText("You don't have access to models")).toBeVisible();
  await page.waitForLoadState('networkidle');

  await page.keyboard.press('ControlOrMeta+k');
  await expect(page.getByRole('dialog', { name: 'Search' })).toBeVisible();
  await page.waitForLoadState('networkidle');
  await page.keyboard.press('Escape');

  expect(refused).toEqual([]);
});
