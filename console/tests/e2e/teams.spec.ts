import { admin, expect, test, unique } from './fixtures';

test('create, edit and delete a team', async ({ signedIn: page, request }) => {
  const name = unique('team');
  await page.goto('teams/?tab=teams');
  // On the Teams tab, Create team is the header's primary action.
  await page.getByRole('button', { name: 'Create team' }).first().click();
  const drawer = page.getByRole('dialog', { name: 'Create team' });
  await drawer.getByLabel(/^Name/).fill(name);
  await drawer.getByLabel('Display name').fill('E2E Team');
  await drawer.getByLabel('Monthly budget (USD)').fill('250');
  await drawer.getByRole('button', { name: 'Create team' }).click();
  await expect(drawer).toBeHidden();

  const card = page.getByRole('listitem', { name: 'E2E Team' });
  await expect(card).toBeVisible();
  await expect(card).toContainText('$250.00');

  await card.getByRole('button', { name: `Edit ${name}` }).click();
  const edit = page.getByRole('dialog', { name: `Edit ${name}` });
  await edit.getByLabel('Display name').fill('E2E Team renamed');
  await edit.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByRole('listitem', { name: 'E2E Team renamed' })).toBeVisible();

  const team = (await (await admin(request, 'GET', `/teams/${name}`)).json()) as {
    spec: { policies: { budgets: { monthly: { amount: string } } } };
  };
  expect(team.spec.policies.budgets.monthly.amount).toBe('250');

  await page
    .getByRole('listitem', { name: 'E2E Team renamed' })
    .getByRole('button', { name: `Delete ${name}` })
    .click();
  const confirm = page.getByRole('alertdialog', { name: `Delete team ${name}?` });
  await confirm.getByRole('textbox').first().fill(name);
  await confirm.getByRole('button', { name: 'Delete team' }).click();
  await expect(page.getByRole('listitem', { name: 'E2E Team renamed' })).toHaveCount(0);
  expect((await admin(request, 'GET', `/teams/${name}`)).status()).toBe(404);
});

test('add a user with a role and remove it', async ({ signedIn: page, request }) => {
  const name = unique('user');
  await page.goto('teams/');
  await page.getByRole('button', { name: 'More' }).first().click();
  await page.getByRole('menuitem', { name: 'Add user without inviting' }).click();
  const drawer = page.getByRole('dialog', { name: 'Add user without inviting' });
  await drawer.getByLabel('Username').fill(name);
  await drawer.getByLabel(/^Name/).fill('E2E Person');
  await drawer.getByLabel('Email').fill(`${name}@example.com`);
  await drawer.getByRole('button', { name: 'Add user' }).click();
  await expect(drawer).toBeHidden();
  await expect(
    page.getByRole('listbox', { name: 'People' }).getByRole('option', { name: /E2E Person/ }),
  ).toBeVisible();
  const user = (await (await admin(request, 'GET', `/users/${name}`)).json()) as {
    spec: { role_bindings: { role: string }[] };
  };
  expect(user.spec.role_bindings[0]?.role).toBe('developer');
  await admin(request, 'DELETE', `/users/${name}`);
});
