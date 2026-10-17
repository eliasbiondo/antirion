import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, test, TOKEN, unique } from './fixtures';

const DATA_URL = `http://127.0.0.1:${process.env.ANTIRION_E2E_DATA_PORT ?? '20480'}`;

async function call(request: APIRequestContext, method: string, path: string, body?: unknown) {
  const response = await request.fetch(`${ADMIN_URL}/admin/v1${path}`, {
    method,
    headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { data: body }),
  });
  expect(response.status(), `${method} ${path}: ${await response.text()}`).toBeLessThan(300);
  return response;
}

/** An upstream that refuses the key `sk-refused` and reports its limits to every other one. */
function upstream(): Promise<Server> {
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c: Buffer) => (body += c.toString()));
    req.on('end', () => {
      if ((req.headers.authorization ?? '').includes('sk-refused')) {
        res.writeHead(401, { 'content-type': 'application/json' });
        res.end(
          '{"error":{"message":"invalid api key","type":"invalid_request_error","code":"invalid_api_key"}}',
        );
        return;
      }
      const parsed = JSON.parse(body || '{}') as { model?: string };
      res.writeHead(200, {
        'content-type': 'application/json',
        'x-ratelimit-limit-requests': '500',
        'x-ratelimit-remaining-requests': '120',
        'x-ratelimit-reset-requests': '20s',
      });
      res.end(
        JSON.stringify({
          id: 'chatcmpl-p',
          object: 'chat.completion',
          created: 1,
          model: parsed.model,
          choices: [{ index: 0, message: { role: 'assistant', content: 'Hi.' }, finish_reason: 'stop' }],
          usage: { prompt_tokens: 4, completion_tokens: 2, total_tokens: 6 },
        }),
      );
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

let server: Server;
let secret = '';
const provider = unique('prk');
const model = unique('prm');
const keyName = unique('prq');

test.beforeAll(async ({ request }) => {
  server = await upstream();
  const port = (server.address() as AddressInfo).port;
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: {
      type: 'openai',
      base_url: `http://127.0.0.1:${port}/v1`,
      account_tier: 'Tier 2',
      credential_rotation_max_age: '30d',
      auth: {
        api_keys: [
          { name: 'main', key: 'sk-accepted-0001', weight: 1 },
          { name: 'spare', key: 'sk-refused-0002', weight: 1 },
        ],
      },
    },
  });
  await call(request, 'POST', '/deployments', {
    name: model,
    spec: { provider, model: 'gpt-4o-mini', price: { input: '0.15', output: '0.60' } },
  });
  await call(request, 'POST', '/models', { name: model, spec: { target: { deployment: model } } });
  const key = await call(request, 'POST', '/keys', { name: keyName, spec: { project: 'default' } });
  secret = ((await key.json()) as { secret: string }).secret;
  for (let i = 0; i < 6; i++) {
    const r = await request.fetch(`${DATA_URL}/v1/chat/completions`, {
      method: 'POST',
      headers: { authorization: `Bearer ${secret}`, 'content-type': 'application/json' },
      data: { model, messages: [{ role: 'user', content: `Hello ${String(i)}` }] },
    });
    await r.text();
  }
});

test.afterAll(async ({ request }) => {
  for (const path of [
    `/keys/${keyName}`,
    `/models/${model}`,
    `/deployments/${model}`,
    `/providers/${provider}`,
  ])
    await call(request, 'DELETE', path);
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
});

test('a provider opens with its tier, key ages and limits, and a refused key is put back in use', async ({
  signedIn: page,
}) => {
  await page.goto('providers/');
  const table = page.getByRole('table', { name: 'Providers' });
  const row = table.getByRole('row', { name: `${provider}: keys and limits` });
  await expect(row).toContainText('Replaced today');
  await expect(row).toContainText('max 30 days');
  await row.click();
  const drawer = page.getByRole('dialog', { name: provider });
  await expect(drawer).toContainText('Tier 2');
  await expect(drawer).toContainText('Replace keys every 30 days');
  const main = drawer.getByRole('listitem').filter({ hasText: 'main' });
  await expect(main).toContainText('120 of 500 left');
  await expect(main).toContainText('Working');
  const spare = drawer.getByRole('listitem').filter({ hasText: 'spare' });
  await expect(spare).toContainText('Not working');
  await expect(spare).toContainText('the provider refused it');
  await spare.getByRole('button', { name: 'Use it again now' }).click();
  await expect(page.getByText('Key back in use')).toBeVisible();
  // Back in use is not evidence that it works: it reads as untested until a request succeeds with it.
  await expect(spare).toContainText('Not tested yet');
});
