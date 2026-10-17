import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, admin, expect, test, TOKEN, unique } from './fixtures';

async function governance(request: APIRequestContext, spec: Record<string, unknown>) {
  const response = await request.fetch(`${ADMIN_URL}/admin/v1/settings/governance`, {
    method: 'PUT',
    headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
    data: { spec },
  });
  expect(response.ok(), await response.text()).toBeTruthy();
}

// Change control is gateway-wide: whatever a test sets is undone before the next one runs.
test.afterEach(async ({ request }) => {
  await governance(request, {});
});

test('a held action asks for approval and the request waits on the Change requests screen', async ({
  signedIn: page,
  request,
}) => {
  await governance(request, { dual_control: { actions: ['cache_flush_all'] } });
  try {
    await page.goto('cache/');
    await page.getByRole('button', { name: 'Clear cache…' }).click();
    const confirm = page.getByRole('alertdialog', { name: 'Clear the whole cache?' });
    await confirm.getByLabel(/Type gateway to confirm/).fill('gateway');
    await confirm.getByRole('button', { name: 'Clear cache' }).click();
    await expect(confirm).toBeHidden();
    await expect(page.getByText('Approval requested')).toBeVisible();
    await expect(page.getByText('Cache cleared')).toHaveCount(0);
    // The Cache screen keeps the waiting request in view after the notice goes.
    await expect(page.getByText('Clearing the whole cache is waiting for approval')).toBeVisible();
    await page
      .getByRole('status')
      .filter({ hasText: 'Approval requested' })
      .getByRole('button', { name: 'View request' })
      .click();
    await expect(page.getByRole('heading', { level: 1, name: 'Change requests' })).toBeVisible();
    await expect(page.getByRole('heading', { level: 2, name: /cache_flush_all/ })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Approve' })).toBeDisabled();
    await page.getByRole('button', { name: 'Cancel request' }).click();
    await page
      .getByRole('alertdialog', { name: /^Cancel / })
      .getByRole('button', { name: 'Cancel request' })
      .click();
    await expect(page.getByText('Cancelled').first()).toBeVisible();
  } finally {
    await governance(request, {});
  }
});

test('a write under change control offers a change request with the same change', async ({
  signedIn: page,
  request,
}) => {
  const team = unique('cc');
  await governance(request, { change_control: { resources: ['teams'] } });
  try {
    await page.goto('teams/?tab=teams');
    // On the Teams tab, Create team is the header's primary action.
    await page.getByRole('button', { name: 'Create team' }).first().click();
    const drawer = page.getByRole('dialog', { name: 'Create team' });
    await drawer.getByLabel(/^Name/).fill(team);
    await drawer.getByRole('button', { name: 'Create team' }).click();
    const offer = page.getByRole('dialog', { name: 'This change needs a reviewer' });
    await expect(offer.getByLabel('Title')).toHaveValue(`Create team ${team}`);
    await expect(offer.getByText('POST /teams')).toBeVisible();
    await offer.getByRole('button', { name: 'Open change request' }).click();
    await expect(offer).toBeHidden();
    await expect(page.getByText('Change request opened')).toBeVisible();
    const listed = await admin(request, 'GET', '/change_requests?state=open');
    const items = ((await listed.json()) as { items: { title: string }[] }).items;
    expect(items.map((i) => i.title)).toContain(`Create team ${team}`);
    expect((await admin(request, 'GET', `/teams/${team}`)).status()).toBe(404);
  } finally {
    await governance(request, {});
  }
});

test('a starred key shows in the sidebar, in search and as a filter', async ({ signedIn: page, request }) => {
  const name = unique('fav');
  expect((await admin(request, 'POST', '/keys', { name, spec: { project: 'default' } })).ok()).toBeTruthy();
  try {
    await page.goto(`keys/?key=${name}`);
    await page.getByRole('button', { name: `Add key ${name} to favourites` }).click();
    const nav = page.getByRole('navigation', { name: 'Console' });
    await expect(nav.getByRole('link', { name: new RegExp(name) })).toBeVisible();
    await page.getByRole('button', { name: /^Starred/ }).click();
    await expect(page).toHaveURL(/starred=1/);
    await expect(page.getByRole('listbox', { name: 'API keys' }).getByRole('option')).toHaveCount(1);
    await page.keyboard.press('ControlOrMeta+k');
    const palette = page.getByRole('dialog', { name: 'Search' });
    await expect(palette.getByText('Favourites')).toBeVisible();
    await expect(palette.getByRole('option', { name: new RegExp(name) }).first()).toBeVisible();
    await page.keyboard.press('Escape');
    await page.getByRole('button', { name: `Remove key ${name} from favourites` }).click();
    await expect(nav.getByRole('link', { name: new RegExp(name) })).toHaveCount(0);
  } finally {
    await admin(request, 'DELETE', `/keys/${name}`);
  }
});

test('compare with lives in the URL and names the comparison', async ({ signedIn: page }) => {
  await page.goto('?range=24h');
  await page.getByRole('button', { name: /Compare with/ }).click();
  await page.getByRole('button', { name: 'Same time last week' }).click();
  await expect(page).toHaveURL(/cmp=week/);
  await expect(page.getByRole('button', { name: 'Compare with: Same time last week' })).toBeVisible();
  await page.goto('analytics/?range=24h&cmp=month');
  await expect(page.getByText('Compared with the same time last month')).toBeVisible();
});

test('the provider select starts at the first provider, with no placeholder option', async ({
  signedIn: page,
}) => {
  await page.goto('providers/');
  await page
    .getByRole('button', { name: /Add provider/ })
    .first()
    .click();
  const drawer = page.getByRole('dialog', { name: /Add provider/ });
  const trigger = drawer.getByRole('combobox', { name: 'Provider' });
  await expect(trigger).toHaveText(/Select a provider/);
  await trigger.click();
  const options = page.getByRole('listbox', { name: 'Provider' }).getByRole('option');
  await expect(options.first()).not.toHaveText(/Select a provider/);
  for (const option of await options.all()) await expect(option).toHaveAttribute('aria-selected', 'false');
});
