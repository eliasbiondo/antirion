import type { Page, Request } from '@playwright/test';

import { admin, expect, signIn, test, unique, watchCsp } from './fixtures';

/** Records how each Admin API write of `method` ended: its status, or the failure the browser reports. */
function watchWrites(page: Page, method: string): Map<string, string> {
  const seen = new Map<string, string>();
  const key = (r: Request) => `${r.method()} ${new URL(r.url()).pathname}`;
  page.on('requestfinished', (r) => {
    if (r.method() !== method || !r.url().includes('/admin/v1/')) return;
    void r.response().then((resp) => seen.set(key(r), String(resp?.status() ?? 'no response')));
  });
  page.on('requestfailed', (r) => {
    if (r.method() === method && r.url().includes('/admin/v1/'))
      seen.set(key(r), r.failure()?.errorText ?? 'failed');
  });
  return seen;
}

test('a write with an empty answer finishes instead of reading as cancelled', async ({
  signedIn: page,
  request,
}) => {
  const name = unique('net-view');
  const created = await admin(request, 'POST', '/logs/views', { name, filters: { status: ['2xx'] } });
  expect(created.status()).toBeLessThan(300);
  const deletes = watchWrites(page, 'DELETE');
  await page.goto('request-log/?range=15m');
  const views = page.getByRole('navigation', { name: 'Saved views' });
  await views.getByRole('button', { name: `Actions for ${name}` }).click();
  await page.getByRole('menuitem', { name: 'Delete' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Delete view' }).click();
  await expect(views.getByRole('button', { name: new RegExp(`^${name}`) })).toHaveCount(0);
  await expect.poll(() => [...deletes.values()]).toEqual(['204']);
});

test('signing out waits for the revoke, which answers 204', async ({ page }) => {
  await signIn(page);
  const posts = watchWrites(page, 'POST');
  await page.getByRole('button', { name: /Account menu/ }).click();
  await page.getByRole('menuitem', { name: 'Sign out' }).click();
  await expect(page).toHaveURL(/\/ui\/login\//);
  await expect.poll(() => posts.get('POST /admin/v1/auth/logout')).toBe('204');
});

test('the push service worker is a same-origin script the console registers under its CSP', async ({
  signedIn: page,
  request,
}) => {
  const csp = watchCsp(page);
  const worker = await request.get('push-worker.js');
  expect(worker.status()).toBe(200);
  expect(worker.headers()['content-type']).toMatch(/javascript/);
  await page.goto('');
  await expect(page.getByRole('navigation', { name: 'Console' })).toBeVisible();
  const scope = await page.evaluate(async () => {
    const reg = await navigator.serviceWorker.register('/ui/push-worker.js', { scope: '/ui/' });
    return reg.scope;
  });
  expect(new URL(scope).pathname).toBe('/ui/');
  expect(csp).toEqual([]);
});
