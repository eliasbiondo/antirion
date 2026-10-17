import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, pickOption, test, TOKEN, unique } from './fixtures';

async function call(request: APIRequestContext, method: string, path: string, body?: unknown) {
  const response = await request.fetch(`${ADMIN_URL}/admin/v1${path}`, {
    method,
    headers: {
      authorization: `Bearer ${TOKEN}`,
      'content-type': method === 'PATCH' ? 'application/merge-patch+json' : 'application/json',
    },
    ...(body === undefined ? {} : { data: body }),
  });
  expect(response.status(), `${method} ${path}: ${await response.text()}`).toBeLessThan(300);
  return response;
}

test('a blocked budget: approve an overage, see and revoke it; a rolling budget with cache hits; re-cost spend', async ({
  signedIn: page,
  request,
}) => {
  const team = unique('bud');
  await call(request, 'POST', '/teams', {
    name: team,
    spec: {
      organization: 'default',
      policies: { budgets: { monthly: { unit: 'usd', amount: '1', period: 'monthly', mode: 'hard' } } },
    },
  });
  const person = unique('ppl');
  await call(request, 'POST', '/users', {
    name: person,
    spec: {
      display_name: 'E2E Spender',
      title: 'Tester',
      primary_team: team,
      email: `${person}@example.com`,
    },
  });
  await call(request, 'POST', `/runtime/budgets/team/${team}/monthly:adjust`, {
    amount: '1.5',
    reason: 'e2e: spend imported',
  });

  await page.goto('budgets/');
  const row = page.getByRole('row').filter({ hasText: team }).first();
  await expect(row).toContainText('Blocked');
  await expect(row).toContainText('Blocks at the limit');

  // Approve an overage from the row.
  await row.getByRole('button', { name: `More for ${team}` }).click();
  await page.getByRole('menuitem', { name: 'Approve overage' }).click();
  const approve = page.getByRole('alertdialog', { name: 'Approve overage?' });
  await approve.getByLabel(/^Extra amount/).fill('2');
  await approve.getByLabel(/^Reason/).fill('e2e launch week');
  await approve.getByRole('button', { name: 'Approve overage' }).click();
  await expect(approve).toBeHidden();
  const counter = (await (await call(request, 'GET', `/runtime/budgets/team/${team}/monthly`)).json()) as {
    effective_amount: string;
    grants: { id: string; reason: string }[];
  };
  expect(Number(counter.effective_amount)).toBe(3);
  expect(counter.grants[0]?.reason).toBe('e2e launch week');

  // It shows as an approved overage in Activity, and is revoked there.
  await page.getByRole('tab', { name: /Activity/ }).click();
  const waiver = page
    .getByRole('listitem')
    .filter({ hasText: `${team} · monthly` })
    .filter({ hasText: 'e2e launch week' });
  await expect(waiver).toBeVisible();
  await waiver.getByRole('button', { name: 'Revoke' }).click();
  await page
    .getByRole('alertdialog', { name: 'Revoke this overage?' })
    .getByRole('button', { name: 'Revoke overage' })
    .click();
  await expect(waiver).toHaveCount(0);
  const after = (await (await call(request, 'GET', `/runtime/budgets/team/${team}/monthly`)).json()) as {
    grants: unknown[];
  };
  expect(after.grants).toHaveLength(0);

  // Make it a rolling 24-hour budget that lets cache hits through.
  await page.getByRole('tab', { name: /Budgets/ }).click();
  await page.getByRole('button', { name: `Edit ${team} monthly` }).click();
  const drawer = page.getByRole('dialog', { name: `${team} · monthly` });
  await expect(drawer.getByText('Burn, last 7 days')).toBeVisible();
  await pickOption(page, drawer.getByLabel('Resets'), 'Never: a rolling window');
  await pickOption(page, drawer.getByLabel('Counts spend over the'), 'Last 24 hours');
  await drawer.getByRole('checkbox', { name: /Answer from the cache/ }).click();
  await drawer.getByRole('button', { name: 'Save changes' }).click();
  await expect(drawer).toBeHidden();
  const declared = (await (await call(request, 'GET', `/teams/${team}`)).json()) as {
    spec: { policies: { budgets: { monthly: Record<string, unknown> } } };
  };
  expect(declared.spec.policies.budgets.monthly).toMatchObject({
    period: '24h',
    algorithm: 'sliding_window',
    cache_hits_bypass_budgets: true,
  });
  await expect(page.getByRole('row').filter({ hasText: team }).first()).toContainText('rolling 24 h');

  // The People tab lists people with this month's figures.
  await page.getByRole('tab', { name: /People/ }).click();
  const people = page.getByRole('table', { name: 'People and their spend this month' });
  await expect(people.getByRole('row').filter({ hasText: 'E2E Spender' })).toContainText('Tester');

  // Re-cost the last week's spend (a gateway-wide report).
  await page.getByRole('tab', { name: /Budgets/ }).click();
  await page.getByRole('button', { name: 'Cost tools' }).click();
  await page.getByRole('menuitem', { name: 'Re-cost spend' }).click();
  await page
    .getByRole('alertdialog', { name: 'Re-cost spend?' })
    .getByRole('button', { name: 'Re-cost spend' })
    .click();
  await expect(page.getByText('Spend re-costed')).toBeVisible();

  await call(request, 'DELETE', `/users/${person}`);
  await call(request, 'DELETE', `/teams/${team}`);
});
