import { admin, expect, pickOption, test, unique } from './fixtures';

test('add a provider, then create, edit and delete a Model', async ({ signedIn: page, request }) => {
  const provider = unique('prov');
  const model = unique('model');
  const display = `E2E ${model}`;

  await page.goto('providers/');
  await page.getByRole('button', { name: 'Add provider' }).first().click();
  const add = page.getByRole('dialog', { name: 'Add provider' });
  await pickOption(page, add.getByRole('combobox', { name: 'Provider', exact: true }), /^OpenAI$/, 'OpenAI');
  await add.getByRole('textbox', { name: 'Name', exact: true }).fill(provider);
  await add.getByLabel('API key').fill('sk-test-0000000000');
  await add.getByRole('button', { name: 'Add provider' }).click();
  await expect(add).toBeHidden();
  await expect(
    page.getByRole('table', { name: 'Providers' }).getByText(provider, { exact: true }),
  ).toBeVisible();

  await page.goto('models/');
  await page.getByRole('button', { name: 'Add model' }).first().click();
  const drawer = page.getByRole('dialog', { name: 'Add model' });
  await pickOption(
    page,
    drawer.getByRole('combobox', { name: 'Provider', exact: true }),
    new RegExp(`^${provider}`),
    provider,
  );
  await drawer.getByLabel('Provider’s model ID').fill('gpt-4o-mini');
  await expect(drawer.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue('gpt-4o-mini');
  await drawer.getByRole('textbox', { name: 'Name', exact: true }).fill(model);
  await drawer.getByLabel('Display name').fill(display);
  await drawer.getByLabel('Input price (USD per 1M tokens)').fill('0.15');
  await drawer.getByLabel('Output price (USD per 1M tokens)').fill('0.60');
  await drawer.getByRole('button', { name: 'Add model' }).click();
  await expect(drawer).toBeHidden();

  const list = page.getByRole('listbox', { name: 'Models' });
  await expect(list.getByRole('option', { name: new RegExp(display) })).toBeVisible();
  await expect(page.getByRole('heading', { level: 2, name: new RegExp(display) })).toBeVisible();
  await expect(page.getByRole('cell', { name: '$0.15 / $0.60' })).toBeVisible();

  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const edit = page.getByRole('dialog', { name: `Edit ${model}` });
  await edit.getByLabel('Display name').fill(`${display} renamed`);
  await edit.getByRole('button', { name: 'Save changes' }).click();
  await expect(edit).toBeHidden();
  await expect(list.getByRole('option', { name: new RegExp(`${display} renamed`) })).toBeVisible();

  await page.getByRole('button', { name: 'More', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Delete' }).click();
  const confirm = page.getByRole('alertdialog', { name: `Delete ${model}` });
  await expect(confirm.getByRole('button', { name: 'Delete permanently' })).toBeDisabled();
  await confirm.getByRole('textbox').first().fill(model);
  await confirm.getByRole('button', { name: 'Add a reason for the audit log' }).click();
  await confirm.getByLabel('Reason').fill('e2e cleanup');
  await confirm.getByRole('button', { name: 'Delete permanently' }).click();
  await expect(list.getByRole('option', { name: new RegExp(`${display} renamed`) })).toHaveCount(0);

  const audit = (await (await admin(request, 'GET', `/audit?resource=models/${model}`)).json()) as unknown;
  expect(JSON.stringify(audit)).toContain('models.delete');
});

test('a Model form refuses invalid input before calling the API', async ({ signedIn: page }) => {
  await page.goto('models/');
  await page.getByRole('button', { name: 'Add model' }).first().click();
  const drawer = page.getByRole('dialog', { name: 'Add model' });
  await drawer.getByRole('textbox', { name: 'Name', exact: true }).fill('bad name!');
  await drawer.getByRole('button', { name: 'Add model' }).click();
  await expect(drawer.getByText('Use letters, digits and . _ : / - only', { exact: false })).toBeVisible();
  await expect(drawer.getByRole('alert').filter({ hasText: 'Select a provider' })).toBeVisible();
});
