import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, test, TOKEN, unique } from './fixtures';

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

test('a key bound to a client certificate keeps the binding until it is removed', async ({
  signedIn: page,
  request,
}) => {
  const name = unique('crt');
  await page.goto('keys/');
  await page.getByRole('button', { name: 'Create key' }).first().click();
  const drawer = page.getByRole('dialog', { name: 'Create API key' });
  await drawer.getByLabel('Name').fill(name);
  await drawer.getByRole('button', { name: /Advanced/ }).click();
  // The key list is a listbox too, so the option is picked by name.
  await drawer.getByRole('combobox', { name: 'Client certificate' }).click();
  await page.getByRole('option', { name: 'SHA-256 fingerprint' }).click();
  await drawer.getByLabel('Certificate fingerprint').fill('not-a-fingerprint');
  await drawer.getByRole('button', { name: 'Create key' }).click();
  await expect(drawer.getByText(/64 hexadecimal digits, with or without colons/)).toBeVisible();
  await drawer.getByRole('combobox', { name: 'Client certificate' }).click();
  await page.getByRole('option', { name: 'Subject alternative name' }).click();
  await drawer.getByLabel('Certificate name').fill('spiffe://acme.test/billing');
  await drawer.getByRole('button', { name: 'Create key' }).click();
  const reveal = page.getByRole('alertdialog', { name: 'Your new API key' });
  await reveal.getByRole('checkbox').check();
  await reveal.getByRole('button', { name: 'Done' }).click();

  const read = async () =>
    (
      (await (await call(request, 'GET', `/keys/${name}`)).json()) as {
        spec: { require_client_cert?: unknown };
      }
    ).spec.require_client_cert;
  expect(await read()).toEqual({ san: 'spiffe://acme.test/billing' });
  await expect(page.getByText('spiffe://acme.test/billing')).toBeVisible();

  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const edit = page.getByRole('dialog', { name: `Edit ${name}` });
  await expect(edit.getByLabel('Certificate name')).toHaveValue('spiffe://acme.test/billing');
  await edit.getByRole('combobox', { name: 'Client certificate' }).click();
  await page.getByRole('option', { name: 'Not required' }).click();
  await edit.getByRole('button', { name: 'Save changes' }).click();
  await expect(edit).toBeHidden();
  expect(await read()).toBeUndefined();
  await call(request, 'DELETE', `/keys/${name}`);
});
