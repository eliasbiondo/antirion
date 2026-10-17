import { admin, expect, test, unique } from './fixtures';

test('list filters reopen from a copied link', async ({ signedIn: page, request }) => {
  const key = unique('lnk');
  expect(
    (await admin(request, 'POST', '/keys', { name: key, spec: { project: 'default' } })).ok(),
  ).toBeTruthy();
  try {
    await page.goto('keys/');
    await page.getByLabel('Search keys').fill(key);
    await expect(page).toHaveURL(new RegExp(`[?&]q=${key}`));
    await page.reload();
    await expect(page.getByLabel('Search keys')).toHaveValue(key);
    await expect(page.getByRole('listbox', { name: 'API keys' }).getByRole('option')).toHaveCount(1);
    await page.getByRole('button', { name: 'Clear filters' }).click();
    await expect(page).not.toHaveURL(/[?&]q=/);
  } finally {
    await admin(request, 'DELETE', `/keys/${key}`);
  }
});

test('a team, a budget and an organization each have a link that reopens them', async ({
  signedIn: page,
  request,
}) => {
  const team = unique('lnt');
  expect(
    (
      await admin(request, 'POST', '/teams', {
        name: team,
        spec: {
          organization: 'default',
          display_name: 'Link Team',
          policies: { budgets: { monthly: { unit: 'usd', amount: '5', period: 'monthly', mode: 'soft' } } },
        },
      })
    ).ok(),
  ).toBeTruthy();
  try {
    await page.goto(`teams/?tab=teams&team=${team}`);
    const grid = page.getByRole('list', { name: 'Teams' });
    await expect(grid.getByRole('listitem')).toHaveCount(1);
    await expect(grid.getByRole('button', { name: 'Copy link to team Link Team' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Export CSV' })).toBeVisible();

    await page.goto('budgets/');
    await page
      .getByRole('button', { name: new RegExp(`^Edit .*${team}`) })
      .first()
      .click();
    await expect(page).toHaveURL(new RegExp(`[?&]budget=team(%2F|/)${team}`));
    const url = page.url();
    await page.goto(url);
    const drawer = page.getByRole('dialog');
    await expect(drawer.getByRole('button', { name: 'Copy link' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page).not.toHaveURL(/[?&]budget=/);

    await page.goto('teams/?tab=organizations&detail=default');
    await expect(page.getByRole('dialog').getByRole('button', { name: 'Copy link' })).toBeVisible();
  } finally {
    await admin(request, 'DELETE', `/teams/${team}`);
  }
});
