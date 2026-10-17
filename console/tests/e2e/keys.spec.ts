import { admin, ADMIN_URL, expect, storedText, test, TOKEN, unique } from './fixtures';

test('create a key, see its secret once, edit it and revoke it', async ({ signedIn: page, request }) => {
  const name = unique('key');
  await page.goto('keys/');
  await page.getByRole('button', { name: 'Create key' }).first().click();
  const drawer = page.getByRole('dialog', { name: 'Create API key' });
  await drawer.getByLabel('Name').fill(name);
  await drawer.getByLabel('Description').fill('created by the e2e suite');
  await drawer.getByRole('radio', { name: 'Only these…' }).click();
  await drawer.getByRole('checkbox', { name: 'Chat completions' }).check();
  await drawer.getByRole('button', { name: /Advanced/ }).click();
  await drawer.getByLabel('Rate limit (requests per minute)').fill('120');
  await drawer.getByRole('button', { name: 'Create key' }).click();

  const reveal = page.getByRole('alertdialog', { name: 'Your new API key' });
  const secret =
    (await reveal.getByRole('status').or(reveal.locator('[data-secret]')).first().textContent()) ?? '';
  expect(secret).toMatch(/^sk-antirion-/);
  await reveal.getByRole('checkbox').check();
  await reveal.getByRole('button', { name: 'Done' }).click();
  await expect(reveal).toBeHidden();
  expect(await page.content()).not.toContain(secret);
  const stored = await storedText(page);
  expect(stored).not.toContain(secret);

  const list = page.getByRole('listbox', { name: 'API keys' });
  await expect(list.getByRole('option', { name: new RegExp(name) })).toBeVisible();
  await expect(page.getByText('120 requests per minute')).toBeVisible();

  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const edit = page.getByRole('dialog', { name: `Edit ${name}` });
  await edit.getByLabel('Description').fill('edited by the e2e suite');
  // Leaving with unsaved edits asks first, by Escape and by Cancel alike.
  await page.keyboard.press('Escape');
  const discard = page.getByRole('alertdialog', { name: 'Discard changes?' });
  await expect(discard).toBeVisible();
  await discard.getByRole('button', { name: 'Keep editing' }).click();
  await edit.getByRole('button', { name: 'Cancel' }).click();
  await expect(discard).toBeVisible();
  await discard.getByRole('button', { name: 'Keep editing' }).click();
  await expect(edit.getByLabel('Description')).toHaveValue('edited by the e2e suite');
  await edit.getByRole('button', { name: 'Save changes' }).click();
  await expect(edit).toBeHidden();
  await expect(page.getByText('edited by the e2e suite')).toBeVisible();

  await page.getByRole('button', { name: 'More' }).click();
  await page.getByRole('menuitem', { name: 'Revoke' }).click();
  const confirm = page.getByRole('alertdialog', { name: `Revoke ${name}?` });
  await page.getByRole('alertdialog').getByRole('textbox').fill(name);
  await confirm.getByRole('button', { name: 'Revoke key' }).click();
  await expect(list.getByRole('option', { name: new RegExp(name) })).toHaveCount(0);
  expect((await admin(request, 'GET', `/keys/${name}`)).status()).toBe(404);
});

test('rotating a key shows the new secret once', async ({ signedIn: page, request }) => {
  const name = unique('rot');
  expect((await admin(request, 'POST', '/keys', { name, spec: { project: 'default' } })).ok()).toBeTruthy();
  await page.goto(`keys/?key=${name}`);
  await expect(page.getByRole('button', { name: 'Edit', exact: true })).toHaveClass(/bg-accent/);
  await page.getByRole('button', { name: 'Rotate', exact: true }).click();
  const confirm = page.getByRole('alertdialog', { name: `Rotate ${name}?` });
  await confirm.getByRole('button', { name: 'Rotate key' }).click();
  const reveal = page.getByRole('alertdialog', { name: `New secret for ${name}` });
  await expect(reveal.locator('[data-secret]')).toContainText('sk-antirion-');
  await expect(reveal.getByRole('button', { name: 'Done' })).toBeDisabled();
  await reveal.getByRole('checkbox').check();
  await reveal.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByText('Rotating', { exact: true }).first()).toBeVisible();
  await admin(request, 'DELETE', `/keys/${name}`);
});

test('an ownerless key is a service key; owner filter, priority class, rotation policy, by team and activity', async ({
  signedIn: page,
  request,
}) => {
  const name = unique('svc');
  await page.goto('keys/');
  await page.getByRole('button', { name: 'Create key' }).first().click();
  const drawer = page.getByRole('dialog', { name: 'Create API key' });
  await drawer.getByLabel('Name').fill(name);
  await drawer.getByRole('button', { name: /Advanced/ }).click();
  await drawer.getByLabel(/^Priority class/).fill('batch');
  await drawer.getByRole('button', { name: 'Create key' }).click();
  const reveal = page.getByRole('alertdialog', { name: 'Your new API key' });
  await reveal.getByRole('checkbox').check();
  await reveal.getByRole('button', { name: 'Done' }).click();

  const stored = (await (await admin(request, 'GET', `/keys/${name}`)).json()) as {
    spec: { priority_class?: string };
    metadata: { type?: string };
  };
  expect(stored.spec.priority_class).toBe('batch');
  expect(stored.metadata.type).toBe('service');

  const list = page.getByRole('listbox', { name: 'API keys' });
  const option = list.getByRole('option', { name: new RegExp(name) });
  await expect(option).toContainText('Service');
  await expect(page.getByText('Priority class')).toBeVisible();
  // The key list is a listbox too, so the filters' options are picked by name.
  await page.getByRole('combobox', { name: 'Owner' }).click();
  await page.getByRole('option', { name: 'No owner' }).click();
  await expect(option).toBeVisible();
  await page.getByRole('combobox', { name: 'Type' }).click();
  await page.getByRole('option', { name: /^User \(/ }).click();
  await expect(option).toHaveCount(0);
  await page.getByRole('button', { name: 'Clear filters' }).first().click();

  // Rotation: the organization's key policy, written and put back.
  const before = (await (await admin(request, 'GET', '/organizations/default')).json()) as {
    spec: { key_policy?: Record<string, unknown> };
  };
  await page.getByRole('tab', { name: 'Rotation' }).click();
  await page.getByLabel(/^Reminders, days before/).fill('10, 3');
  await page.getByRole('button', { name: 'Save policy' }).click();
  await expect(page.getByText('Key policy saved')).toBeVisible();
  const org = (await (await admin(request, 'GET', '/organizations/default')).json()) as {
    spec: { key_policy?: { reminder_days?: number[] } };
  };
  expect(org.spec.key_policy?.reminder_days).toEqual([10, 3]);
  await request.fetch(`${ADMIN_URL}/admin/v1/organizations/default`, {
    method: 'PATCH',
    headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/merge-patch+json' },
    data: { spec: { key_policy: before.spec.key_policy ?? null } },
  });

  await page.getByRole('tab', { name: 'By team' }).click();
  await expect(page.getByRole('button', { name, exact: true })).toBeVisible();
  await page.getByRole('tab', { name: 'Activity' }).click();
  await expect(page.getByRole('list', { name: 'Key activity' }).getByText(name).first()).toBeVisible();

  await admin(request, 'DELETE', `/keys/${name}`);
});
