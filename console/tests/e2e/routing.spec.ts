import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { readFile } from 'node:fs/promises';

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

/** An upstream that answers every chat completion, with the model it was asked for. */
function upstream(): Promise<Server> {
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c: Buffer) => (body += c.toString()));
    req.on('end', () => {
      const model = (JSON.parse(body || '{}') as { model?: string }).model ?? 'gpt-4o-mini';
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          id: 'chatcmpl-e2e',
          object: 'chat.completion',
          created: 1,
          model,
          choices: [
            {
              index: 0,
              message: { role: 'assistant', content: `Hello from ${model}.` },
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
const provider = unique('rop');
const route = unique('ror');
const model = unique('rom');
const team = unique('rot');
const project = unique('roj');
const d1 = `${provider}-main`;
const d2 = `${provider}-next`;
const d3 = `${provider}-shadow`;

async function tree(request: APIRequestContext): Promise<{ rules: Record<string, unknown>[] }> {
  const r = (await (await call(request, 'GET', `/routes/${route}`)).json()) as {
    spec: { tree: { rules: Record<string, unknown>[] } };
  };
  return r.spec.tree;
}

async function send(request: APIRequestContext, session?: string) {
  const response = await request.fetch(`${DATA_URL}/v1/chat/completions`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${secret}`,
      'content-type': 'application/json',
      ...(session ? { 'x-antirion-session-id': session } : {}),
    },
    data: { model, messages: [{ role: 'user', content: 'Hello' }] },
  });
  expect(response.status(), await response.text()).toBe(200);
}

async function open(page: Page, tab?: string) {
  await page.goto(`routing/?route=${route}${tab ? `&tab=${tab}` : ''}`);
  await expect(page.getByRole('heading', { name: 'Routing policy' })).toBeVisible();
}

test.beforeAll(async ({ request }) => {
  server = await upstream();
  const port = (server.address() as AddressInfo).port;
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: `http://127.0.0.1:${port}/v1`, auth: { api_key: 'sk-test-e2e' } },
  });
  for (const [d, m] of [
    [d1, 'gpt-4o-mini'],
    [d2, 'gpt-4o'],
    [d3, 'o4-mini'],
  ] as const)
    await call(request, 'POST', '/deployments', {
      name: d,
      spec: { provider, model: m, price: { input: '0.15', output: '0.60' } },
    });
  await call(request, 'POST', '/teams', { name: team, spec: { organization: 'default' } });
  await call(request, 'POST', '/projects', { name: project, spec: { team } });
  await call(request, 'PUT', `/routes/${route}`, {
    spec: {
      affinity: { session: true },
      tree: {
        type: 'conditional',
        rules: [
          {
            name: 'compare-o4',
            description: 'Copy every request to the shadow deployment.',
            when: { field: 'identity.team', eq: team },
            target: { type: 'deployment', deployment: d1, mirror: { target: d3, fraction: 1 } },
          },
          {
            name: 'spill',
            when: { field: 'identity.team', eq: 'nobody' },
            target: { type: 'fallback', children: [d2, d1] },
          },
        ],
        default: d1,
      },
    },
  });
  await call(request, 'POST', '/models', { name: model, spec: { route } });
  const key = await call(request, 'POST', '/keys', { name: unique('rok'), spec: { project } });
  secret = ((await key.json()) as { secret: string }).secret;
  for (let i = 0; i < 3; i++) await send(request);
});

test.afterAll(async ({ request }) => {
  await call(request, 'PATCH', '/settings/policies', { spec: { resilience: null } });
  await new Promise((resolve) => server.close(resolve));
});

test('a rule shows its share, shadow results and last editor, and duplicates turned off', async ({
  signedIn: page,
  request,
}) => {
  await open(page);
  const row = page.getByRole('listitem').filter({ hasText: 'compare-o4' }).first();
  await expect(row.getByText(/\/ 24h/)).toBeVisible();
  await page.getByRole('button', { name: 'Show details of compare-o4' }).click();
  await expect(page.getByText('Shadow comparison · last 24 hours')).toBeVisible();
  await expect(page.getByText(/^\d+ responses?$/)).toBeVisible();
  await expect(page.getByText(/ago by bootstrap/).first()).toBeVisible();

  await page.getByRole('button', { name: 'More for spill' }).click();
  await page.getByRole('menuitem', { name: 'Duplicate' }).click();
  await expect(page.getByRole('heading', { name: 'spill (copy)' })).toBeVisible();
  const rules = (await tree(request)).rules;
  expect(rules.map((r) => r.name)).toEqual(['compare-o4', 'spill', 'spill (copy)']);
  expect(rules[2]?.enabled).toBe(false);
});

test('a rule’s link opens it with its details, and the rule filters live in the URL', async ({
  signedIn: page,
}) => {
  await page.goto(`routing/?route=${route}&rule=spill`);
  await expect(page.getByRole('button', { name: 'Hide details of spill', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Show details of compare-o4' })).toBeVisible();
  await page.getByRole('button', { name: 'More for spill', exact: true }).click();
  await expect(page.getByRole('menuitem', { name: 'Copy link' })).toBeVisible();
  await page.keyboard.press('Escape');
  await page.getByLabel('Search rules').fill('compare');
  await expect(page).toHaveURL(/[?&]q=compare/);
  await page.reload();
  await expect(page.getByLabel('Search rules')).toHaveValue('compare');
  await expect(page.getByRole('button', { name: 'Show details of compare-o4' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'More for spill', exact: true })).toHaveCount(0);
});

test('the wizard builds a canary with a ramp, an error budget, a sample and per-rule limits', async ({
  signedIn: page,
  request,
}) => {
  await open(page);
  await page.getByRole('button', { name: 'Create rule' }).first().click();
  const drawer = page.getByRole('dialog', { name: 'Create rule' });
  await drawer.getByLabel('Name').fill('o4-ramp');
  await drawer.getByLabel('Value of condition 1').fill(team);
  await drawer.getByLabel('Share of matching requests, in percent').fill('50');
  await drawer.getByRole('button', { name: 'Next: strategy' }).click();
  await drawer.getByRole('radio', { name: /Canary/ }).click();
  await drawer.getByRole('button', { name: 'Next: targets' }).click();
  await pickOption(
    page,
    drawer.getByRole('combobox', { name: 'Stable deployment' }),
    new RegExp(`^${d1}`),
    d1,
  );
  await pickOption(
    page,
    drawer.getByRole('combobox', { name: 'Candidate deployment' }),
    new RegExp(`^${d2}`),
    d2,
  );
  await drawer.getByRole('checkbox', { name: 'Timeouts' }).click();
  await drawer.getByRole('button', { name: 'Next: review' }).click();
  await drawer.getByLabel('Attempt timeout').fill('20s');
  await drawer.getByRole('button', { name: 'Create rule' }).click();
  await expect(drawer).toBeHidden();
  await expect(page.getByRole('heading', { name: 'o4-ramp' })).toBeVisible();

  const rule = (await tree(request)).rules.find((r) => r.name === 'o4-ramp');
  expect(rule?.when).toEqual({ field: 'identity.team', eq: team, sample: 0.5 });
  expect(rule?.target).toMatchObject({
    type: 'weighted',
    children: [
      { id: 'stable', deployment: d1, weight: 95 },
      { id: 'canary', deployment: d2, weight: 5 },
    ],
    canary: { child: 'canary', error_budget: { max_error_rate: 0.02, window: '5m', action: 'rollback' } },
    policies: {
      resilience: {
        timeouts: { attempt: '20s' },
        retry: { classes: { timeout: { actions: ['retry_same'] } } },
      },
    },
  });
});

test('defaults are edited, and a route’s sessions are reset', async ({ signedIn: page, request }) => {
  await send(request, unique('session'));
  await open(page, 'health');
  await expect(page.getByText(/Sessions kept on a deployment by route/)).toContainText(/[1-9]/);
  await page.getByRole('button', { name: 'Reset sessions' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Reset sessions' }).click();
  await expect(page.getByText('Sessions reset')).toBeVisible();

  await page.getByRole('button', { name: 'Edit defaults' }).click();
  const drawer = page.getByRole('dialog', { name: 'Edit routing defaults' });
  await drawer.getByLabel('Attempts at most').fill('4');
  await drawer.getByLabel('Each attempt').fill('45s');
  await drawer.getByRole('button', { name: 'Save defaults' }).click();
  await expect(drawer).toBeHidden();
  const policies = (await (await call(request, 'GET', '/settings/policies')).json()) as {
    spec: { resilience?: { retry?: { max_attempts?: number }; timeouts?: { attempt?: string } } };
  };
  expect(policies.spec.resilience?.retry?.max_attempts).toBe(4);
  expect(policies.spec.resilience?.timeouts?.attempt).toBe('45s');
});

test('live decisions show a new request with the rule it matched', async ({ signedIn: page, request }) => {
  await open(page, 'live');
  await expect(page.getByRole('button', { name: 'Pause' })).toBeVisible();
  await send(request);
  const table = page.getByRole('table', { name: 'Recent routing decisions' });
  await expect(table.getByText('compare-o4').first()).toBeVisible({ timeout: 15_000 });
  await expect(table.getByText(new RegExp(`Matched compare-o4 \\(team is ${team}\\)`)).first()).toBeVisible();
});

test('routing exports as YAML and a pending change opens from the Changes tab', async ({
  signedIn: page,
  request,
}) => {
  const body = { spec: { tree: { type: 'conditional', rules: (await tree(request)).rules, default: d2 } } };
  const cr = await call(request, 'POST', '/change_requests', {
    title: `Send the rest of ${route} to the next deployment`,
    changes: [{ method: 'PUT', path: `/routes/${route}`, body }],
  });
  const id = ((await cr.json()) as { id: string }).id;
  try {
    await open(page);
    await page.getByRole('button', { name: 'More', exact: true }).click();
    const download = page.waitForEvent('download');
    await page.getByRole('menuitem', { name: 'Export YAML' }).click();
    const yaml = await readFile(await (await download).path(), 'utf8');
    expect(yaml).toContain('routes:');
    expect(yaml).toContain(`${route}:`);
    expect(yaml).toContain('models:');

    await page.getByRole('tab', { name: /Changes/ }).click();
    const pending = page.getByRole('list', { name: 'Pending routing changes' });
    await pending.getByRole('button', { name: new RegExp(`Send the rest of ${route}`) }).click();
    await expect(page.getByRole('dialog', { name: 'Change request' })).toContainText('Waiting for review');
  } finally {
    await call(request, 'POST', `/change_requests/${id}:cancel`, {});
  }
});
