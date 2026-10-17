import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

import { admin, expect, test, unique } from './fixtures';

const DATA_URL = `http://127.0.0.1:${process.env.ANTIRION_E2E_DATA_PORT ?? '20480'}`;

/** An upstream that answers every chat completion. */
function upstream(): Promise<Server> {
  const server = createServer((req, res) => {
    req.resume();
    req.on('end', () => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          id: 'chatcmpl-marks',
          object: 'chat.completion',
          created: 1,
          model: 'claude-sonnet-4-5',
          choices: [{ index: 0, message: { role: 'assistant', content: 'Hi.' }, finish_reason: 'stop' }],
          usage: { prompt_tokens: 3, completion_tokens: 1, total_tokens: 4 },
        }),
      );
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

let server: Server;
let secret = '';
// A claude model served by a provider of the openai type (the demo's "bedrock"), and a generic one.
const claude = unique('claude-marks');
const openaiTyped = unique('bedrock-marks');
const generic = unique('vllm-marks');

test.beforeAll(async ({ request }) => {
  server = await upstream();
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1`;
  const ok = async (path: string, body: unknown) => {
    const r = await admin(request, 'POST', path, body);
    expect(r.status(), `${path}: ${await r.text()}`).toBeLessThan(300);
    return r;
  };
  await ok('/providers', {
    name: openaiTyped,
    spec: { type: 'openai', base_url: base, auth: { api_key: 'sk-e2e' } },
  });
  await ok('/providers', {
    name: generic,
    spec: { type: 'openai_compatible', base_url: base, auth: { api_key: 'sk-e2e' } },
  });
  await ok('/deployments', {
    name: `${claude}-dep`,
    spec: { provider: openaiTyped, model: 'claude-sonnet-4-5', price: { input: '3', output: '15' } },
  });
  await ok('/models', { name: claude, spec: { target: `${claude}-dep` } });
  const key = await ok('/keys', { name: unique('marks-key'), spec: { project: 'default' } });
  secret = ((await key.json()) as { secret: string }).secret;
  const sent = await request.fetch(`${DATA_URL}/v1/chat/completions`, {
    method: 'POST',
    headers: { authorization: `Bearer ${secret}`, 'content-type': 'application/json' },
    data: { model: claude, messages: [{ role: 'user', content: 'hello' }] },
  });
  expect(sent.status(), await sent.text()).toBe(200);
});

test.afterAll(async ({ request }) => {
  // Leaves the gateway as it found it: later specs expect no Models of their own making.
  await admin(request, 'DELETE', `/models/${claude}`);
  await admin(request, 'DELETE', `/deployments/${claude}-dep`);
  await admin(request, 'DELETE', `/providers/${openaiTyped}`);
  await admin(request, 'DELETE', `/providers/${generic}`);
  await new Promise((resolve) => server.close(resolve));
});

test("a claude model shows Anthropic's mark in the request log, whatever provider serves it", async ({
  signedIn: page,
}) => {
  await page.goto(`request-log/?range=15m&model=${claude}`);
  const row = page.getByRole('table', { name: 'Requests' }).getByRole('row').filter({ hasText: claude });
  await expect(row.first()).toBeVisible({ timeout: 15_000 });
  await expect(row.first().locator('[data-mark]').first()).toHaveAttribute('data-mark', 'anthropic');
  await expect(row.first().locator('[data-mark=openai]')).toHaveCount(0);
});

test('a generic provider shows the neutral monogram, an openai-typed one the OpenAI mark', async ({
  signedIn: page,
}) => {
  await page.goto('providers/');
  const genericRow = page.getByRole('row').filter({ hasText: generic });
  await expect(genericRow).toBeVisible();
  await expect(genericRow.locator('[data-mark]').first()).toHaveAttribute('data-mark', 'monogram');
  await expect(genericRow.locator('[data-mark=monogram]')).toHaveText('O');
  const typedRow = page.getByRole('row').filter({ hasText: openaiTyped });
  await expect(typedRow.locator('[data-mark]').first()).toHaveAttribute('data-mark', 'openai');
});
