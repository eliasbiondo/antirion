import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

import type { APIRequestContext, Page } from '@playwright/test';

import { ADMIN_URL, expect, pickOption, test, TOKEN, unique } from './fixtures';

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

/** A vector of letter counts: prompts that differ by a letter or two come out very close. */
function vector(text: string): number[] {
  const v = new Array<number>(32).fill(0);
  for (const ch of text.toLowerCase()) v[ch.charCodeAt(0) % 32] = (v[ch.charCodeAt(0) % 32] ?? 0) + 1;
  const n = Math.hypot(...v) || 1;
  return v.map((x) => x / n);
}

/** An upstream that answers chat completions and embeddings. */
function upstream(): Promise<Server> {
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c: Buffer) => (body += c.toString()));
    req.on('end', () => {
      const parsed = JSON.parse(body || '{}') as { model?: string; input?: string | string[] };
      res.writeHead(200, { 'content-type': 'application/json' });
      if (req.url?.includes('/embeddings')) {
        const inputs = Array.isArray(parsed.input) ? parsed.input : [parsed.input ?? ''];
        res.end(
          JSON.stringify({
            object: 'list',
            model: parsed.model,
            data: inputs.map((t, index) => ({ object: 'embedding', index, embedding: vector(t) })),
            usage: { prompt_tokens: 4 * inputs.length, total_tokens: 4 * inputs.length },
          }),
        );
        return;
      }
      res.end(
        JSON.stringify({
          id: 'chatcmpl-cache',
          object: 'chat.completion',
          created: 1,
          model: parsed.model,
          choices: [
            { index: 0, message: { role: 'assistant', content: 'Cached answer.' }, finish_reason: 'stop' },
          ],
          usage: { prompt_tokens: 40, completion_tokens: 8, total_tokens: 48 },
        }),
      );
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

let server: Server;
let secret = '';
const provider = unique('cap');
const chat = unique('cam');
const similar = unique('cas');
const embed = unique('cae');
const team = unique('cat');
const project = unique('caj');
const rule = unique('car');
const keyName = unique('cak');

async function send(request: APIRequestContext, model: string, content: string) {
  const response = await request.fetch(`${DATA_URL}/v1/chat/completions`, {
    method: 'POST',
    headers: { authorization: `Bearer ${secret}`, 'content-type': 'application/json' },
    data: { model, messages: [{ role: 'user', content }] },
  });
  expect(response.status(), await response.text()).toBe(200);
  return response.headers()['x-antirion-cache'];
}

async function open(page: Page, query = '') {
  await page.goto(`cache/${query}`);
  await expect(page.getByRole('heading', { level: 1, name: 'Cache' })).toBeVisible();
}

test.beforeAll(async ({ request }) => {
  server = await upstream();
  const port = (server.address() as AddressInfo).port;
  await call(request, 'PATCH', '/settings/cache', { spec: { semantic_store: { type: 'memory' } } });
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: `http://127.0.0.1:${port}/v1`, auth: { api_key: 'sk-test-e2e' } },
  });
  for (const [d, m] of [
    [chat, 'gpt-4o-mini'],
    [similar, 'gpt-4o'],
    [embed, 'text-embedding-3-small'],
  ] as const)
    await call(request, 'POST', '/deployments', {
      name: d,
      spec: { provider, model: m, price: { input: '2.50', output: '10.00', cache_read: '1.25' } },
    });
  await call(request, 'POST', '/models', { name: embed, spec: { target: { deployment: embed } } });
  await call(request, 'POST', '/models', {
    name: chat,
    spec: { target: { deployment: chat }, policies: { cache: { enabled: true, ttl: '1h' } } },
  });
  await call(request, 'POST', '/models', {
    name: similar,
    spec: {
      target: { deployment: similar },
      policies: {
        cache: { enabled: true, semantic: { enabled: true, embedding_model: embed, threshold: 0.9 } },
      },
    },
  });
  await call(request, 'POST', '/teams', {
    name: team,
    spec: { organization: 'default', policies: { cache: { ttl: '6h' } } },
  });
  await call(request, 'POST', '/projects', { name: project, spec: { team } });
  const key = await call(request, 'POST', '/keys', { name: keyName, spec: { project } });
  secret = ((await key.json()) as { secret: string }).secret;
});

test.afterAll(async ({ request }) => {
  // Leave the gateway as it was, so the specs after this one see no extra Models.
  for (const path of [
    `/models/${similar}`,
    `/models/${chat}`,
    `/models/${embed}`,
    `/keys/${keyName}`,
    `/projects/${project}`,
    `/teams/${team}`,
    `/deployments/${chat}`,
    `/deployments/${similar}`,
    `/deployments/${embed}`,
    `/providers/${provider}`,
  ])
    await call(request, 'DELETE', path);
  await call(request, 'PATCH', '/settings/cache', { spec: { semantic_store: null } });
  await new Promise((resolve) => server.close(resolve));
});

test('a model is set to refuse requests when the cache is down, with per-item embeddings and prompt caching', async ({
  signedIn: page,
  request,
}) => {
  await open(page);
  const table = page.getByRole('table', { name: 'Caching per model' });
  await table.getByRole('switch', { name: `Turn on provider prompt caching for ${chat}` }).click();
  await expect(page.getByText('Prompt caching turned on')).toBeVisible();
  const row = table.getByRole('row').filter({ hasText: chat }).first();
  await row.getByRole('button', { name: 'Settings' }).click();
  const drawer = page.getByRole('dialog', { name: `Cache settings for ${chat}` });
  await drawer.getByRole('radio', { name: /Also ignore predicted output/ }).click();
  await drawer.getByLabel(/Most responses to keep/).fill('500');
  await drawer.getByRole('radio', { name: /Refuse requests/ }).click();
  await drawer.getByRole('switch', { name: /embeddings request on its own/ }).click();
  await drawer.getByRole('button', { name: 'Save settings' }).click();
  await expect(drawer).toBeHidden();
  const spec = (
    (await (await call(request, 'GET', `/models/${chat}`)).json()) as {
      spec: { policies: { cache: Record<string, unknown> } };
    }
  ).spec.policies.cache;
  expect(spec).toMatchObject({
    enabled: true,
    max_entries: 500,
    fail_mode: 'closed',
    embeddings: { per_item: true },
    key: { ignore_fields: ['/prediction'] },
    prompt_caching: { auto_inject: true },
  });
  await page.getByRole('button', { name: `Show details of ${chat}` }).click();
  const settings = page.getByRole('region', { name: 'Settings' });
  await expect(settings).toContainText('If the cache is downRefuse requests');
  await expect(settings).toContainText('Cached-read discount50% off input');
});

test('a cached response opens with its key and answer, and a selection is removed', async ({
  signedIn: page,
  request,
}) => {
  await send(request, chat, 'What is the capital of France?');
  expect(await send(request, chat, 'What is the capital of France?')).toBe('hit');
  await open(page, `?tab=entries&model=${chat}`);
  const table = page.getByRole('table', { name: 'Cached responses' });
  await expect(table.getByRole('row')).toHaveCount(2);
  await expect(page.getByText(/Showing 1 of 1/)).toBeVisible();
  await table.getByRole('row').nth(1).getByRole('cell').nth(1).getByRole('button').click();
  const drawer = page.getByRole('dialog', { name: 'Cached response' });
  await expect(drawer.locator('pre')).toContainText('"content": "Cached answer."');
  await expect(drawer.getByText(chat).first()).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(drawer).toBeHidden();
  await table.getByRole('checkbox', { name: 'Select every response shown' }).click();
  await page.getByRole('button', { name: 'Remove selected' }).click();
  await page
    .getByRole('alertdialog', { name: 'Remove 1 cached response?' })
    .getByRole('button', { name: 'Remove responses' })
    .click();
  await expect(page.getByText('1 response removed')).toBeVisible();
  const left = (await (await call(request, 'GET', `/cache/entries?model=${chat}`)).json()) as {
    items: unknown[];
  };
  expect(left.items).toHaveLength(0);
});

test('a wrong similar match is reported and counted', async ({ signedIn: page, request }) => {
  await send(request, similar, 'Summarise this support ticket for me');
  expect(await send(request, similar, 'Summarize this support ticket for me')).toBe('hit');
  await open(page, `?tab=entries&model=${similar}&match=semantic`);
  const table = page.getByRole('table', { name: 'Cached responses' });
  await table
    .getByRole('row')
    .nth(1)
    .getByRole('button', { name: /^Actions for/ })
    .click();
  await page.getByRole('menuitem', { name: 'Report wrong match' }).click();
  await page
    .getByRole('alertdialog', { name: 'Report this as a wrong match?' })
    .getByRole('button', { name: 'Report wrong match' })
    .click();
  await expect(page.getByText('Wrong match reported')).toBeVisible();
  const status = (await (await call(request, 'GET', '/runtime/cache')).json()) as {
    semantic: { scope: string; false_positive_reports: number }[];
  };
  expect(status.semantic.find((s) => s.scope === `model/${similar}`)?.false_positive_reports).toBe(1);
  await page.getByRole('tab', { name: 'Statistics' }).click();
  const matching = page.getByRole('table', { name: 'Similar-prompt matching' });
  await expect(matching.getByRole('row').filter({ hasText: similar })).toContainText('(1)');
  await expect(
    page.getByRole('table', { name: 'Cache overrides' }).getByRole('row').filter({ hasText: team }),
  ).toContainText('Keep 6 h');
});

test('a clearing rule is created from a preset, runs, and shows its firing and activity', async ({
  signedIn: page,
  request,
}) => {
  await open(page, '?tab=rules');
  await page.getByRole('button', { name: 'Create rule' }).first().click();
  const editor = page.getByRole('dialog', { name: 'Create clearing rule' });
  await editor.getByRole('button', { name: /Nightly refresh/ }).click();
  await editor.getByLabel('Name').fill(rule);
  await pickOption(page, editor.getByRole('combobox', { name: 'What to clear' }), 'A model’s responses');
  await pickOption(page, editor.getByRole('combobox', { name: 'Model' }), chat, chat);
  await expect(editor.getByText(`clear the responses of model ${chat}.`)).toBeVisible();
  await editor.getByRole('button', { name: 'Create rule' }).click();
  await expect(editor).toBeHidden();
  const created = (await (await call(request, 'GET', `/cache/invalidation_rules/${rule}`)).json()) as {
    spec: Record<string, unknown>;
  };
  expect(created.spec).toMatchObject({
    trigger: { event: 'schedule', schedule: '0 3 * * *' },
    action: { drop: 'model', value: chat },
  });
  const item = page.getByRole('listitem').filter({ hasText: rule });
  await item.getByRole('button', { name: 'Run now' }).click();
  await expect(page.getByText('Rule ran')).toBeVisible();
  await expect(page.getByLabel(`Firings of ${rule}`)).toContainText('1in 24 h');
  await page.getByRole('tab', { name: 'Activity' }).click();
  await expect(page.getByText(`Created clearing rule ${rule}`)).toBeVisible();
  await expect(page.getByText(`Ran rule ${rule} by hand`)).toBeVisible();
  await page.getByRole('tab', { name: 'Clearing rules' }).click();
  await page.getByRole('button', { name: `More actions for ${rule}` }).click();
  await page.getByRole('menuitem', { name: 'Delete' }).click();
  await page
    .getByRole('alertdialog', { name: 'Delete this clearing rule?' })
    .getByRole('button', { name: 'Delete rule' })
    .click();
  await expect(page.getByRole('listitem').filter({ hasText: rule })).toHaveCount(0);
});
