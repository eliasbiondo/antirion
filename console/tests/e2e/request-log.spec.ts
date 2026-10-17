import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

import type { APIRequestContext, Page } from '@playwright/test';

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

/** An upstream that answers every chat completion with the same short reply. */
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
          choices: [
            {
              index: 0,
              message: { role: 'assistant', content: 'Ahoy from the mock.' },
              finish_reason: 'stop',
            },
          ],
          usage: { prompt_tokens: 12, completion_tokens: 5, total_tokens: 17 },
        }),
      );
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

let server: Server;
let secret = '';
const model = unique('logm');

test.beforeAll(async ({ request }) => {
  server = await upstream();
  const port = (server.address() as AddressInfo).port;
  const provider = unique('logp');
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: `http://127.0.0.1:${port}/v1`, auth: { api_key: 'sk-test-e2e' } },
  });
  await call(request, 'POST', '/deployments', {
    name: `${model}-dep`,
    spec: { provider, model: 'gpt-4o-mini', price: { input: '0.15', output: '0.60' } },
  });
  await call(request, 'POST', '/models', { name: model, spec: { target: `${model}-dep` } });
  const key = await call(request, 'POST', '/keys', {
    name: unique('logk'),
    spec: { project: 'default', tags: ['env:e2e'] },
  });
  secret = ((await key.json()) as { secret: string }).secret;
  await call(request, 'PUT', '/settings/policies', {
    spec: { observability: { content: { enabled: true } } },
  });
});

test.afterAll(async ({ request }) => {
  await call(request, 'PUT', '/settings/policies', { spec: {} });
  await call(request, 'PUT', '/settings/observability', { spec: {} });
  await new Promise((resolve) => server.close(resolve));
});

async function send(request: APIRequestContext, text: string) {
  const response = await request.fetch(`${DATA_URL}/v1/chat/completions`, {
    method: 'POST',
    headers: { authorization: `Bearer ${secret}`, 'content-type': 'application/json' },
    data: {
      model,
      messages: [
        { role: 'system', content: 'Answer like a pirate.' },
        { role: 'user', content: text },
      ],
    },
  });
  expect(response.status(), await response.text()).toBe(200);
}

/** Moves the pointer off the table, so live rows are not held. */
async function away(page: Page) {
  await page.mouse.move(2, 2);
}

test('a new request streams into the live log; its drawer reads the conversation and replays it', async ({
  signedIn: page,
  request,
}) => {
  await send(request, 'warm up');
  await page.goto('request-log/?range=15m');
  await expect(page.getByRole('button', { name: 'Pause live updates' })).toBeVisible();
  await expect(page.getByRole('table', { name: 'Requests' })).toBeVisible();
  await away(page);
  const prompt = `Where is the treasure ${unique('q')}?`;
  await send(request, prompt);
  const row = page.getByRole('row').filter({ hasText: prompt });
  await expect(row).toBeVisible({ timeout: 15_000 });

  await row.click();
  const drawer = page.getByRole('dialog').filter({ has: page.getByRole('tab', { name: /Messages/ }) });
  await drawer.getByRole('tab', { name: /Messages/ }).click();
  await expect(drawer.getByText('Answer like a pirate.')).toBeVisible();
  await expect(drawer.getByText('Ahoy from the mock.')).toBeVisible();
  await drawer.getByRole('tab', { name: 'Response', exact: true }).click();
  await expect(drawer.getByText(/Finished: stop/)).toBeVisible();
  await drawer.getByRole('tab', { name: 'Request', exact: true }).click();
  await expect(drawer.getByText('POST /v1/chat/completions')).toBeVisible();
  await drawer.getByRole('radio', { name: /Params/ }).click();
  await expect(drawer.getByText('No query parameters')).toBeVisible();

  await drawer.getByRole('button', { name: 'Replay' }).click();
  const confirm = page.getByRole('dialog', { name: 'Replay this request?' });
  await confirm.getByRole('switch', { name: 'Edit the body before sending' }).click();
  const body = confirm.getByLabel('Request body');
  const edited = (await body.inputValue()).replace(prompt, `${prompt} again`);
  await body.fill(edited);
  await confirm.getByRole('button', { name: 'Send the edited request' }).click();
  const sent = page.getByRole('dialog', { name: 'Request sent again' });
  await expect(sent.getByText('The gateway answered')).toBeVisible();
  await sent.getByRole('button', { name: 'Open the new request' }).click();
  await expect(page.getByRole('button', { name: 'replay of an earlier request' })).toBeVisible();
  await page.getByRole('tab', { name: /Messages/ }).click();
  await expect(drawer.getByText(`${prompt} again`)).toBeVisible();
});

test('live rows wait under the pointer and show from the pill', async ({ signedIn: page, request }) => {
  await page.goto('request-log/?range=15m');
  await expect(page.getByRole('table', { name: 'Requests' })).toBeVisible();
  await page.getByRole('table', { name: 'Requests' }).locator('tbody tr').first().hover();
  const prompt = `Held ${unique('h')}`;
  await send(request, prompt);
  const pill = page.getByRole('button', { name: /new request/ });
  await expect(pill).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('row').filter({ hasText: prompt })).toHaveCount(0);
  await pill.click();
  await expect(page.getByRole('row').filter({ hasText: prompt })).toBeVisible();
});

test('filters and the sort live in the URL', async ({ signedIn: page }) => {
  await page.goto('request-log/?range=15m&status=2xx&environment=e2e&sort=latency_ms:asc');
  await expect(page.getByRole('button', { name: 'Remove filter Status: 2xx' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Remove filter Environment: e2e' })).toBeVisible();
  await expect(page.getByRole('columnheader', { name: /Latency/ })).toHaveAttribute('aria-sort', 'ascending');
  await page
    .getByRole('navigation', { name: 'Saved views' })
    .getByRole('button', { name: /Errors$/ })
    .click();
  await expect(page).toHaveURL(/status=4xx&status=5xx/);
  await expect(page).not.toHaveURL(/environment=/);
  await page.getByRole('button', { name: 'Remove filter Status: 4xx, 5xx' }).click();
  await expect(page).not.toHaveURL(/status=/);
});

test('a filter a link sets shows as a chip, and Clear all removes it', async ({ signedIn: page }) => {
  await page.goto('request-log/?range=15m&project=zendesk-assistant&request_id=01a1-none');
  await expect(page.getByRole('button', { name: 'Remove filter Project: zendesk-assistant' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Remove filter Request ID: 01a1-none' })).toBeVisible();
  await page
    .getByRole('group', { name: 'Active filters' })
    .getByRole('button', { name: 'Clear all' })
    .click();
  await expect(page).not.toHaveURL(/project=|request_id=/);
});

test('the palette offers to open a request only for an id, and opens over a request drawer', async ({
  signedIn: page,
  request,
}) => {
  await send(request, `palette ${unique('p')}`);
  await page.goto('request-log/?range=15m');
  await page.getByRole('table', { name: 'Requests' }).locator('tbody tr').first().click();
  await expect(page.getByRole('tab', { name: /Messages/ })).toBeVisible();
  await page.keyboard.press('ControlOrMeta+k');
  const palette = page.getByRole('dialog', { name: 'Search' });
  await expect(palette).toBeVisible();
  await palette.getByRole('combobox', { name: 'Search' }).fill('no-such-thing-here');
  await expect(palette.getByText(/^Open request/)).toHaveCount(0);
  await palette.getByRole('combobox', { name: 'Search' }).fill('01a144e7-62e8-7049-9f29-296d6857d9a1');
  await expect(palette.getByText(/^Open request/)).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(palette).toBeHidden();
});

test('a saved view keeps filters, sort and columns; it is starred, renamed and deleted', async ({
  signedIn: page,
}) => {
  const name = unique('view');
  await page.goto('request-log/?range=15m&status=2xx&sort=cost_usd:desc');
  await page.getByRole('button', { name: 'Columns' }).click();
  const chooser = page.getByRole('dialog', { name: 'Columns' });
  await chooser.getByRole('checkbox', { name: 'Time to first token' }).click();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('columnheader', { name: /TTFT/ })).toBeVisible();

  const views = page.getByRole('navigation', { name: 'Saved views' });
  await views.getByRole('button', { name: 'Save view' }).click();
  const dialog = page.getByRole('dialog', { name: 'Save the current view' });
  await dialog.getByLabel('Name').fill(name);
  await dialog.getByRole('switch', { name: /Share with the organization/ }).click();
  await dialog.getByRole('button', { name: 'Save view' }).click();
  await expect(dialog).toBeHidden();
  await expect(views.getByRole('button', { name: `Unstar ${name}` })).toBeVisible();

  await page.goto('request-log/?range=15m');
  await page.getByRole('button', { name: 'Columns' }).click();
  await page.getByRole('dialog', { name: 'Columns' }).getByRole('button', { name: 'Reset' }).click();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('columnheader', { name: /TTFT/ })).toHaveCount(0);
  await views.getByRole('button', { name: new RegExp(`^${name}`) }).click();
  await expect(page.getByRole('button', { name: 'Remove filter Status: 2xx' })).toBeVisible();
  await expect(page).toHaveURL(/sort=cost_usd%3Adesc|sort=cost_usd:desc/);
  await expect(page.getByRole('columnheader', { name: /TTFT/ })).toBeVisible();

  await page.reload();
  await expect(page.getByRole('columnheader', { name: /TTFT/ })).toBeVisible();

  await views.getByRole('button', { name: `Unstar ${name}` }).click();
  await expect(views.getByRole('button', { name: `Star ${name}` })).toBeVisible();
  await views.getByRole('button', { name: `Actions for ${name}` }).click();
  await page.getByRole('menuitem', { name: 'Rename' }).click();
  const rename = page.getByRole('dialog', { name: `Rename ${name}` });
  await rename.getByLabel('Name').fill(`${name}-2`);
  await rename.getByRole('button', { name: 'Rename view' }).click();
  await views.getByRole('button', { name: `Actions for ${name}-2` }).click();
  await page.getByRole('menuitem', { name: 'Delete' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Delete view' }).click();
  await expect(views.getByRole('button', { name: new RegExp(`^${name}`) })).toHaveCount(0);
});

test('the time picker keeps to the log retention', async ({ signedIn: page, request }) => {
  await call(request, 'PUT', '/settings/observability', {
    spec: { request_logs: { sinks: { store: { retention: '7d' } } } },
  });
  await page.goto('request-log/?range=15m');
  await expect(page.getByText('Requests are kept for 7 days', { exact: false }).first()).toBeVisible();
  await page.getByRole('button', { name: /Last 15 minutes/ }).click();
  await expect(page.getByRole('button', { name: /Last 30 days/ })).toBeDisabled();
  await expect(page.getByRole('button', { name: /Last 7 days/ })).toBeEnabled();
  await page.keyboard.press('Escape');
  await page.goto('request-log/?range=30d');
  await expect(page.getByText(/so nothing before .* is left/)).toBeVisible();
  await call(request, 'PUT', '/settings/observability', { spec: {} });
});

test('the Overview filters its recent requests and opens the log with them', async ({ signedIn: page }) => {
  await page.goto('?range=15m');
  await expect(page.getByRole('heading', { name: 'Recent requests' })).toBeVisible();
  await page.getByRole('button', { name: 'Errors only' }).click();
  await expect(page.getByRole('button', { name: 'Errors only' })).toHaveAttribute('aria-pressed', 'true');
  await page.getByRole('link', { name: /View all/ }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Request log' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Remove filter Status: 4xx, 5xx' })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Provider headroom' })).toHaveCount(0);
});
