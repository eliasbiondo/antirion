import type { Page } from '@playwright/test';

import { admin, ADMIN_URL, expect, gatewayStep, storedText, test, totp, unique, watchCsp } from './fixtures';

const PASSWORD = 'Invited-operator-2026';

async function acknowledge(page: Page, title: string): Promise<string> {
  const reveal = page.getByRole('alertdialog', { name: title });
  const secret = (await reveal.locator('[data-secret]').textContent()) ?? '';
  await reveal.getByRole('checkbox').check();
  await reveal.getByRole('button', { name: 'Done' }).click();
  await expect(reveal).toBeHidden();
  return secret;
}

test('an invited operator sets a password, enrols a second factor and manages their own tokens', async ({
  page,
  request,
}) => {
  const csp = watchCsp(page);
  const email = `${unique('invited')}@example.com`;
  const created = await admin(request, 'POST', '/invitations', {
    email,
    display_name: 'Invited Operator',
    role_bindings: [{ role: 'gateway_viewer', scope: 'gateway' }],
  });
  expect(created.status()).toBe(201);
  const { link } = (await created.json()) as { link: string };
  expect(link).toContain('/ui/accept-invitation');

  await page.goto(link);
  await expect(page.getByRole('heading', { name: 'Accept your invitation' })).toBeVisible();
  await expect(page.getByText('Step 1 of 3')).toBeVisible();
  await page.getByLabel('New password', { exact: true }).fill(PASSWORD);
  await page.getByLabel('Confirm password', { exact: true }).fill(PASSWORD);
  await page.getByRole('button', { name: 'Set password and continue' }).click();

  await expect(page.getByRole('heading', { name: 'Set up two-step verification' })).toBeVisible();
  await expect(page.getByRole('img', { name: /QR code/ })).toBeVisible();
  const secret = (await page.locator('[data-secret]').textContent()) ?? '';
  expect(secret).toMatch(/^[A-Z2-7]+=*$/);
  const step = await gatewayStep(request);
  // A full code submits on its own.
  await page.getByLabel('Verification code').fill(totp(secret, step));
  await expect(page.getByRole('heading', { name: 'Save your recovery codes' })).toBeVisible();
  const codes = page.getByRole('list', { name: 'Recovery codes' }).getByRole('listitem');
  expect(await codes.count()).toBeGreaterThan(1);
  const proceed = page.getByRole('button', { name: 'Continue to Antirion' });
  await expect(proceed).toBeDisabled();
  await page.getByLabel("I've saved my recovery codes").check();
  await proceed.click();
  await expect(page.getByRole('navigation', { name: 'Console' })).toBeVisible();
  expect(await storedText(page)).not.toContain(secret);

  await page.getByRole('button', { name: /Account menu/ }).click();
  await page.getByRole('menuitem', { name: 'Personal tokens' }).click();
  const account = page.getByRole('dialog', { name: 'Account' });
  await account.getByLabel('Name', { exact: true }).fill('e2e script');
  await account.getByRole('button', { name: 'Create token' }).click();
  const token = await acknowledge(page, 'Your new personal token');
  const me = () =>
    request.get(`${ADMIN_URL}/admin/v1/auth/me`, { headers: { authorization: `Bearer ${token}` } });
  expect((await me()).status()).toBe(200);
  const tokens = account.getByRole('table', { name: 'Personal tokens' });
  await expect(tokens).toContainText('e2e script');
  await tokens.getByRole('button', { name: 'Revoke' }).click();
  const revoke = page.getByRole('alertdialog', { name: 'Revoke this token?' });
  await revoke.getByLabel(/to confirm/).fill('e2e script');
  await revoke.getByRole('button', { name: 'Revoke token' }).click();
  await expect(account.getByText('No personal tokens yet')).toBeVisible();
  expect((await me()).status()).toBe(401);

  await account.getByRole('button', { name: 'Security', exact: true }).click();
  await expect(account.getByRole('table', { name: 'Your sessions' }).getByRole('row')).not.toHaveCount(1);
  await page.keyboard.press('Escape');

  await page.getByRole('button', { name: /Account menu/ }).click();
  await page.getByRole('menuitem', { name: 'Sign out' }).click();
  await expect(page).toHaveURL(/\/ui\/login\//);
  await page.getByLabel('Email', { exact: true }).fill(email);
  await page.getByLabel('Password', { exact: true }).fill(PASSWORD);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Enter your verification code' })).toBeVisible();
  await expect(page.getByText(email)).toBeVisible();
  await page.getByLabel('Verification code').fill(totp(secret, (await gatewayStep(request)) + 1));
  await expect(page.getByRole('navigation', { name: 'Console' })).toBeVisible();
  expect(csp).toEqual([]);
});

test('a wrong password gives one generic error', async ({ page }) => {
  await page.goto('login/');
  await page.getByLabel('Email', { exact: true }).fill('nobody@example.com');
  await page.getByLabel('Password', { exact: true }).fill('Not-the-password-1');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(
    page.getByRole('alert').filter({ hasText: /That email and password don't match/ }),
  ).toBeVisible();
  await expect(page.getByLabel('Email', { exact: true })).toHaveValue('nobody@example.com');
  await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');
});

test('an invitation link that was used or expired says so before asking for a password', async ({ page }) => {
  await page.goto('accept-invitation/?token=not-a-real-invitation-token');
  await expect(page.getByRole('heading', { name: 'This invitation link no longer works' })).toBeVisible();
  await expect(page.getByLabel('New password', { exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Go to sign in' }).click();
  await expect(page.getByRole('heading', { name: 'Sign in', level: 1 })).toBeVisible();
});
