import { createHmac } from 'node:crypto';

import { expect, test as base, type APIRequestContext, type Page } from '@playwright/test';

export const ADMIN_URL = process.env.ANTIRION_E2E_ADMIN_URL ?? 'http://127.0.0.1:20481';
export const TOKEN = process.env.ANTIRION_ADMIN_TOKEN ?? 'dev-bootstrap-token-0123456789abcdef0123';
export const STORAGE = 'test-results/.auth/session.json';

/** A name unique to this run, so a reused gateway's leftovers never collide. */
export function unique(prefix: string): string {
  return `${prefix}-${Date.now().toString(36)}${Math.floor(Math.random() * 1296).toString(36)}`;
}

/** Calls the Admin API directly with the bootstrap token, for test setup only. */
export async function admin(
  request: APIRequestContext,
  method: 'GET' | 'POST' | 'DELETE',
  path: string,
  body?: unknown,
) {
  const response = await request.fetch(`${ADMIN_URL}/admin/v1${path}`, {
    method,
    headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { data: body }),
  });
  return response;
}

/** Opens the admin-token form; with local accounts on, it sits behind a link under the password form. */
export async function openTokenForm(page: Page): Promise<void> {
  const link = page.getByRole('button', { name: 'Use an admin token' });
  const field = page.getByLabel('Admin token');
  await expect(link.or(field)).toBeVisible();
  if (await link.isVisible()) await link.click();
}

export async function signIn(page: Page): Promise<void> {
  await page.goto('login/');
  await openTokenForm(page);
  await page.getByLabel('Admin token').fill(TOKEN);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByRole('navigation', { name: 'Console' })).toBeVisible();
}

/** Records Content-Security-Policy violations the page reports. */
export function watchCsp(page: Page): string[] {
  const violations: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && /Content Security Policy|Refused to (execute|apply|load)/i.test(m.text()))
      violations.push(m.text());
  });
  return violations;
}

/** Everything the page keeps in web storage and its URL, to check that no secret ends up there. */
export async function storedText(page: Page): Promise<string> {
  return page.evaluate(() => {
    const dump = (storage: Storage) => Object.keys(storage).map((k) => `${k}=${storage.getItem(k) ?? ''}`);
    return [...dump(localStorage), ...dump(sessionStorage), location.href].join('\n');
  });
}

export const test = base.extend<{ signedIn: Page }>({
  signedIn: async ({ browser }, provide) => {
    const context = await browser.newContext({ storageState: STORAGE });
    const page = await context.newPage();
    await provide(page);
    await context.close();
  },
});

export { expect };

/**
 * Picks an option of the console's Select or Combobox (they replace the native select): opens the
 * list from its trigger, narrows a Combobox by typing, then clicks the option.
 */
export async function pickOption(
  page: Page,
  trigger: ReturnType<Page['getByRole']>,
  option: string | RegExp,
  search?: string,
): Promise<void> {
  await trigger.click();
  const list = page.getByRole('listbox');
  await expect(list).toBeVisible();
  if (search) await page.keyboard.type(search);
  await list.getByRole('option', { name: option }).first().click();
  await expect(list).toBeHidden();
}

function base32(text: string): Buffer {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const ch of text.replace(/=+$/, '').toUpperCase())
    bits += alphabet.indexOf(ch).toString(2).padStart(5, '0');
  const bytes: number[] = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) bytes.push(parseInt(bits.slice(i, i + 8), 2));
  return Buffer.from(bytes);
}

/** The authenticator app's code for a time step, as the gateway computes it. */
export function totp(secret: string, step: number): string {
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(step));
  const digest = createHmac('sha1', base32(secret)).update(counter).digest();
  const offset = (digest[digest.length - 1] ?? 0) & 0x0f;
  const word = digest.readUInt32BE(offset) & 0x7fffffff;
  return String(word % 1_000_000).padStart(6, '0');
}

/** The time step of the test-mode gateway's clock. */
export async function gatewayStep(request: APIRequestContext): Promise<number> {
  const clock = (await (await admin(request, 'GET', '/test/clock')).json()) as { now: string };
  return Math.floor(Date.parse(clock.now) / 30_000);
}
