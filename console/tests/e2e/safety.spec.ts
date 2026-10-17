import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, openTokenForm, test, TOKEN, unique, watchCsp } from './fixtures';

const DATA_URL = `http://127.0.0.1:${process.env.ANTIRION_E2E_DATA_PORT ?? '20480'}`;

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

function upstream(): Promise<Server> {
  const server = createServer((req, res) => {
    req.resume();
    req.on('end', () => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          id: 'chatcmpl-e2e',
          object: 'chat.completion',
          created: 1,
          model: 'gpt-4o-mini',
          choices: [{ index: 0, message: { role: 'assistant', content: 'Done.' }, finish_reason: 'stop' }],
          usage: { prompt_tokens: 12, completion_tokens: 2, total_tokens: 14 },
        }),
      );
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

let server: Server;
let secret = '';
const model = unique('safem');
const pii = `pii_${Date.now().toString(36)}`;

test.beforeAll(async ({ request }) => {
  server = await upstream();
  const port = (server.address() as AddressInfo).port;
  const provider = unique('safep');
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: `http://127.0.0.1:${port}/v1`, auth: { api_key: 'sk-test-e2e' } },
  });
  await call(request, 'POST', '/deployments', {
    name: `${model}-dep`,
    spec: { provider, model: 'gpt-4o-mini', price: { input: '0.15', output: '0.60' } },
  });
  await call(request, 'POST', '/models', { name: model, spec: { target: `${model}-dep` } });
  const key = await call(request, 'POST', '/keys', { name: unique('safek'), spec: { project: 'default' } });
  secret = ((await key.json()) as { secret: string }).secret;
  await call(request, 'POST', '/guardrails', {
    name: pii,
    spec: { type: 'pii', action: 'redact', pii: { entities: ['email', 'phone', 'ip_address'] } },
  });
  await call(request, 'PATCH', '/settings/policies', { spec: { guardrails: { input: [pii] } } });
  const sent = await request.fetch(`${DATA_URL}/v1/chat/completions`, {
    method: 'POST',
    headers: { authorization: `Bearer ${secret}`, 'content-type': 'application/json' },
    data: { model, messages: [{ role: 'user', content: 'Mail me at ada@example.com about the refund' }] },
  });
  expect(sent.status(), await sent.text()).toBe(200);
});

test.afterAll(async ({ request }) => {
  await call(request, 'PUT', '/settings/policies', { spec: {} });
  await request.fetch(`${ADMIN_URL}/admin/v1/guardrails/${pii}`, {
    method: 'DELETE',
    headers: { authorization: `Bearer ${TOKEN}` },
  });
  await new Promise((resolve) => server.close(resolve));
});

test('a rule is created, tested on sample text and deleted from the Safety screen', async ({
  signedIn: page,
}) => {
  const csp = watchCsp(page);
  const name = `words_${Date.now().toString(36)}`;
  await page.goto('safety/?view=rules');
  await page.getByRole('button', { name: 'New rule' }).click();
  const drawer = page.getByRole('dialog', { name: 'New rule' });
  await drawer.getByLabel('Method').click();
  await page.getByRole('option', { name: 'Keywords', exact: true }).click();
  await drawer.getByLabel('Name').fill(name);
  await drawer.getByLabel('Words and phrases').fill('project falcon');
  await drawer.getByRole('button', { name: 'Create rule' }).click();
  await expect(drawer).toBeHidden();
  await page.getByRole('button', { name }).click();
  const edit = page.getByRole('dialog', { name });
  const panel = edit.getByRole('region', { name: 'Test this rule' });
  await panel.getByLabel('Sample text').fill('This is about project falcon.');
  await panel.getByRole('button', { name: 'Run test' }).click();
  await expect(panel.getByRole('status')).toContainText('1 match');
  await expect(panel.locator('mark')).toHaveText(/project falcon/);
  await edit.getByRole('button', { name: 'Delete rule' }).click();
  await page.getByRole('alertdialog').getByRole('textbox').fill(name);
  await page.getByRole('alertdialog').getByRole('button', { name: 'Delete rule' }).click();
  await expect(page.getByRole('button', { name })).toHaveCount(0);
  expect(csp).toEqual([]);
});

test('an event explains its verdict and prepares "allow similar" as an edit of its rule', async ({
  signedIn: page,
  request,
}) => {
  await page.goto('safety/');
  await expect(page.getByText(`${pii} found 1 email`).first()).toBeVisible();
  await expect(page.getByText('Gateway (prompts)')).toBeVisible();
  await expect(page.getByRole('list', { name: 'Safety checks' })).toBeVisible();
  await page.getByRole('button', { name: 'More' }).click();
  await page.getByRole('menuitem', { name: 'Allow similar' }).click();
  const drawer = page.getByRole('dialog', { name: pii });
  await expect(drawer).toContainText('Stop checking for email address.');
  await drawer.getByRole('button', { name: 'Save rule' }).click();
  await expect(drawer).toBeHidden();
  const rule = (await (await call(request, 'GET', `/guardrails/${pii}`)).json()) as {
    spec: { pii?: { entities?: string[] } };
  };
  expect(rule.spec.pii?.entities).toEqual(['phone', 'ip_address']);
  await expect(page.getByRole('button', { name: 'Export CSV' })).toBeVisible();
});

test('strictness presets say what they change before replacing the rule', async ({
  signedIn: page,
  request,
}) => {
  await page.goto('safety/?view=pii');
  await page.getByRole('radio', { name: 'Strict' }).click();
  const confirm = page.getByRole('alertdialog', { name: 'Make redaction strict?' });
  await expect(confirm).toContainText('Starts checking for');
  await confirm.getByRole('button', { name: 'Make it strict' }).click();
  await expect(confirm).toBeHidden();
  const rule = (await (await call(request, 'GET', `/guardrails/${pii}`)).json()) as {
    spec: { pii?: { entities?: string[] } };
  };
  expect(rule.spec.pii?.entities).toHaveLength(7);
  await page.getByRole('tab', { name: /Coverage/ }).click();
  await expect(page.getByRole('list', { name: 'Detection coverage' })).toContainText(
    'Secrets and exposed keys',
  );
});

test('the account takes a photo and a language, and the email landing page explains a dead link', async ({
  browser,
  request,
}) => {
  const user = unique('acct');
  await call(request, 'POST', '/users', {
    name: user,
    spec: { kind: 'human', email: `${user}@example.com`, display_name: 'Account Tester', role_bindings: [] },
  });
  const created = (await (await call(request, 'POST', `/users/${user}/tokens`, {})).json()) as {
    token: string;
  };
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto('login/');
  await openTokenForm(page);
  await page.getByLabel('Admin token').fill(created.token);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByRole('navigation', { name: 'Console' })).toBeVisible();

  await page.goto('?account=profile');
  const account = page.getByRole('dialog', { name: 'Account' });
  const png = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGP4z8DwnwEIGP4zMDAwAAAhCQP9dU7aaAAAAABJRU5ErkJggg==',
    'base64',
  );
  await account
    .locator('input[type="file"]')
    .setInputFiles({ name: 'me.png', mimeType: 'image/png', buffer: png });
  await expect(account.getByRole('button', { name: 'Change photo' })).toBeVisible();
  await expect(page.getByRole('button', { name: /Account menu/ }).locator('img')).toBeVisible();

  await account.getByRole('button', { name: 'Change email' }).click();
  await account.getByLabel('New email').fill(`${user}@new.example.com`);
  await account.getByRole('button', { name: 'Change email' }).click();
  await expect(account.getByLabel('Email', { exact: true })).toHaveValue(`${user}@new.example.com`);

  await account.getByRole('button', { name: 'Preferences', exact: true }).click();
  await account.getByRole('combobox', { name: 'Language' }).click();
  await page.getByRole('option', { name: 'English' }).click();
  await expect.poll(async () => page.evaluate(() => document.documentElement.lang)).toBe('en');

  await page.goto('confirm-email/?token=not-a-real-token');
  await expect(page.getByRole('heading', { name: 'This link no longer works' })).toBeVisible();
  await context.close();
  await call(request, 'DELETE', `/users/${user}`);
});

test('the sign-in page states the second step and offers no trust without trusted devices', async ({
  page,
}) => {
  await page.goto('login/');
  // A gateway with no account yet opens on the bootstrap token; the password form is one step away.
  const other = page.getByRole('button', { name: 'Use another sign-in method' });
  const statement = page.getByText(/Password sign-in always asks for a second step/);
  await expect(other.or(statement)).toBeVisible();
  if (await other.isVisible()) await other.click();
  await expect(statement).toBeVisible();
  await expect(page.getByRole('checkbox', { name: /Trust this browser/ })).toHaveCount(0);
  await page.goto('reset-password/?token=not-a-real-reset-token');
  await expect(page.getByRole('heading', { name: 'This reset link no longer works' })).toBeVisible();
});
