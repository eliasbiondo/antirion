import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, openTokenForm, test, TOKEN, unique } from './fixtures';

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

test.describe('sign-in from a terminal', () => {
  const user = unique('cli');
  let token = '';

  test.beforeAll(async ({ request }) => {
    await call(request, 'PATCH', '/settings/auth', {
      spec: { admin: { device_authorization: { enabled: true } } },
    });
    await call(request, 'POST', '/users', {
      name: user,
      spec: {
        kind: 'human',
        email: `${user}@example.test`,
        role_bindings: [{ role: 'viewer', scope: 'gateway' }],
      },
    });
    const created = await call(request, 'POST', `/users/${user}/tokens`, {});
    token = ((await created.json()) as { token: string }).token;
  });

  test.afterAll(async ({ request }) => {
    await call(request, 'DELETE', `/users/${user}`);
    await call(request, 'PATCH', '/settings/auth', { spec: { admin: { device_authorization: null } } });
  });

  test('the sign-in page shows the command, and a signed-in person approves its code', async ({
    page,
    request,
  }) => {
    await page.goto('login/');
    await page.getByRole('button', { name: 'Sign in from a terminal' }).click();
    await expect(page.getByRole('heading', { name: 'Sign in from a terminal' })).toBeVisible();
    await expect(page.getByText(/antirion auth login --admin-url http/)).toBeVisible();

    const started = await request.fetch(`${ADMIN_URL}/admin/v1/auth/device`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      data: { client_id: 'e2e' },
    });
    expect(started.status()).toBe(200);
    const grant = (await started.json()) as { user_code: string; device_code: string };

    await page.goto(`device/?code=${grant.user_code}`);
    await page.getByRole('link', { name: 'Sign in to continue' }).click();
    await openTokenForm(page);
    await page.getByLabel('Admin token').fill(token);
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await expect(page.getByLabel('Code')).toHaveValue(grant.user_code);
    await page.getByRole('button', { name: 'Continue' }).click();
    await expect(page.getByRole('heading', { name: 'Approve this sign-in?' })).toBeVisible();
    await expect(page.getByText('e2e', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Approve' }).click();
    await expect(page.getByRole('heading', { name: 'Sign-in approved' })).toBeVisible();

    const polled = await request.fetch(`${ADMIN_URL}/admin/v1/auth/device/token`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      data: { device_code: grant.device_code },
    });
    expect(polled.status()).toBe(200);
    expect(((await polled.json()) as { user: string }).user).toBe(user);
  });
});
