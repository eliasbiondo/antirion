import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, test, TOKEN, unique } from './fixtures';

async function call(request: APIRequestContext, method: string, path: string, body?: unknown) {
  const response = await request.fetch(`${ADMIN_URL}/admin/v1${path}`, {
    method,
    headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { data: body }),
  });
  expect(response.status(), `${method} ${path}: ${await response.text()}`).toBeLessThan(300);
  return response;
}

const WORDS = ['Ahoy ', 'from ', 'the ', 'streaming ', 'mock.'];

/** An upstream that answers chat completions whole, or word by word as server-sent events. */
function upstream(): Promise<Server> {
  const server = createServer((req, res) => {
    let raw = '';
    req.on('data', (c: Buffer) => (raw += c.toString()));
    req.on('end', () => {
      const body = JSON.parse(raw || '{}') as { stream?: boolean };
      const base = { id: 'chatcmpl-pg', created: 1, model: 'gpt-4o-mini' };
      if (!body.stream) {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(
          JSON.stringify({
            ...base,
            object: 'chat.completion',
            choices: [
              { index: 0, message: { role: 'assistant', content: WORDS.join('') }, finish_reason: 'stop' },
            ],
            usage: { prompt_tokens: 9, completion_tokens: 5, total_tokens: 14 },
          }),
        );
        return;
      }
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const frames = [
        ...WORDS.map((w) => ({
          ...base,
          object: 'chat.completion.chunk',
          choices: [{ index: 0, delta: { content: w } }],
        })),
        {
          ...base,
          object: 'chat.completion.chunk',
          choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
          usage: { prompt_tokens: 9, completion_tokens: 5, total_tokens: 14 },
        },
      ];
      let i = 0;
      const tick = setInterval(() => {
        const frame = frames[i];
        if (frame) res.write(`data: ${JSON.stringify(frame)}\n\n`);
        else {
          res.end('data: [DONE]\n\n');
          clearInterval(tick);
        }
        i += 1;
      }, 120);
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

let server: Server;
const model = unique('pgm');

test.beforeAll(async ({ request }) => {
  server = await upstream();
  const port = (server.address() as AddressInfo).port;
  const provider = unique('pgp');
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: `http://127.0.0.1:${port}/v1`, auth: { api_key: 'sk-test-e2e' } },
  });
  await call(request, 'POST', '/deployments', {
    name: `${model}-dep`,
    spec: { provider, model: 'gpt-4o-mini', price: { input: '0.15', output: '0.60' } },
  });
  await call(request, 'POST', '/models', { name: model, spec: { target: `${model}-dep` } });
  await call(request, 'PUT', '/settings/policies', {
    spec: { observability: { content: { enabled: true } } },
  });
});

test.afterAll(async ({ request }) => {
  await call(request, 'PUT', '/settings/policies', { spec: {} });
  const keys = (await (await call(request, 'GET', '/keys?limit=500')).json()) as {
    items: { name: string; metadata: { created_by?: string } }[];
  };
  for (const k of keys.items)
    if (k.metadata.created_by === 'playground') await call(request, 'DELETE', `/keys/${k.name}`);
  await new Promise((resolve) => server.close(resolve));
});

test('the playground streams an answer, shows its record and opens it in the request log', async ({
  signedIn: page,
}) => {
  await page.goto(`playground/?model=${model}&project=default`);
  await expect(page.getByRole('heading', { name: 'Playground' })).toBeVisible();
  await expect(page.getByRole('combobox', { name: /Model/ })).toContainText(model);
  const prompt = `Where is the treasure ${unique('q')}?`;
  await page.getByRole('textbox', { name: 'User message 1' }).fill(prompt);
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await expect(page.getByText('Streaming')).toBeVisible();
  await expect(page.getByText(WORDS.join(''))).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText('Finished: stop')).toBeVisible();
  const figures = page.getByRole('definition').filter({ hasText: '9 · 5' });
  await expect(figures).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(/short-lived key playground-/)).toBeVisible();
  const html = await page.content();
  expect(html).not.toContain('sk-antirion');

  await page.getByRole('tab', { name: /Decision headers/ }).click();
  await expect(page.getByRole('cell', { name: 'x-antirion-deployment' })).toBeVisible();
  await page.getByRole('link', { name: 'View in request log' }).click();
  const drawer = page.getByRole('dialog').filter({ has: page.getByRole('tab', { name: /Messages/ }) });
  await drawer.getByRole('tab', { name: /Messages/ }).click();
  await expect(drawer.getByText(prompt)).toBeVisible();
});

test('a logged request opens in the playground with its body, and Models offers it', async ({
  signedIn: page,
}) => {
  const prompt = `Replay me ${unique('r')}`;
  await page.goto(`playground/?model=${model}&project=default`);
  await page.getByRole('textbox', { name: 'User message 1' }).fill(prompt);
  await page.getByRole('button', { name: /Parameters/ }).click();
  await page.getByRole('switch', { name: 'Stream the answer' }).click();
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await expect(page.getByText(WORDS.join(''))).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('link', { name: 'View in request log' })).toBeVisible();
  await page.getByRole('link', { name: 'View in request log' }).click();

  await page.getByRole('button', { name: 'Replay' }).click();
  await page.getByRole('link', { name: 'Open in playground' }).click();
  await expect(page.getByText('Opened from a logged request.')).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'User message 1' })).toHaveValue(prompt);
  await page.reload();
  await expect(page.getByRole('textbox', { name: 'User message 1' })).toHaveValue(prompt);

  await page.goto(`models/?model=${model}`);
  await expect(page.getByRole('link', { name: 'Try in playground' }).first()).toBeVisible();
});
