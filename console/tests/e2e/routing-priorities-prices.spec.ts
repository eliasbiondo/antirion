import type { APIRequestContext, Locator, Page } from '@playwright/test';

import { ADMIN_URL, expect, pickOption, test, TOKEN, unique } from './fixtures';

/** Routing wizard nodes, priority shares of limits and request-dependent prices. */

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

async function json<T>(request: APIRequestContext, path: string): Promise<T> {
  return (await (await call(request, 'GET', path)).json()) as T;
}

function card(dialog: Locator, title: string) {
  return dialog
    .locator('section')
    .filter({ has: dialog.page().getByRole('heading', { name: title, exact: true }) });
}

const provider = unique('p3p');
const route = unique('p3r');
const [r1, r2, strong, weak] = ['a', 'b', 's', 'w'].map((x) => unique(`p3${x}`)) as [
  string,
  string,
  string,
  string,
];
const upstream = unique('p3u');

test.beforeAll(async ({ request }) => {
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: 'http://127.0.0.1:9/v1', auth: { api_key: 'sk-test-e2e' } },
  });
  for (const [d, m] of [
    [r1, 'llama'],
    [r2, 'llama'],
    [strong, 'big'],
    [weak, upstream],
  ] as const)
    await call(request, 'POST', '/deployments', {
      name: d,
      spec: { provider, model: m, price: { input: '1.00', output: '2.00' } },
    });
  await call(request, 'PUT', `/routes/${route}`, {
    spec: {
      tree: {
        type: 'conditional',
        rules: [{ name: 'existing', when: { field: 'identity.team', eq: 'nobody' }, target: r1 }],
        default: r1,
      },
    },
  });
  await call(request, 'POST', '/models', { name: weak, spec: { target: weak } });
});

test.afterAll(async ({ request }) => {
  await call(request, 'PATCH', '/settings/policies', { spec: { resilience: null } });
  await call(request, 'PATCH', '/settings/pricing', { spec: { models: { [`openai/${upstream}`]: null } } });
  await call(request, 'DELETE', `/models/${weak}`);
  await call(request, 'DELETE', `/routes/${route}`);
  for (const d of [r1, r2, strong, weak]) await call(request, 'DELETE', `/deployments/${d}`);
  await call(request, 'DELETE', `/providers/${provider}`);
});

async function openWizard(page: Page, name: string) {
  await page.goto(`routing/?route=${route}`);
  await expect(page.getByRole('heading', { name: 'Routing policy' })).toBeVisible();
  await page.getByRole('button', { name: 'Create rule' }).first().click();
  const drawer = page.getByRole('dialog', { name: 'Create rule' });
  await drawer.getByLabel('Name').fill(name);
  await drawer.getByLabel('Value of condition 1').fill('research');
  await drawer.getByRole('button', { name: 'Next: strategy' }).click();
  return drawer;
}

type Rules = { spec: { tree: { rules: { name: string; target: unknown }[] } } };

test('the wizard builds a pool with an external picker, adapter affinity, hedging and a failed-answer check', async ({
  signedIn: page,
  request,
}) => {
  const drawer = await openWizard(page, 'self-hosted');
  await drawer.getByRole('radio', { name: /Self-hosted pool/ }).click();
  await drawer.getByRole('button', { name: 'Next: targets' }).click();
  await pickOption(page, drawer.getByRole('combobox', { name: 'Deployment 1' }), new RegExp(`^${r1}`), r1);
  await drawer.getByRole('button', { name: 'Add deployment' }).click();
  await pickOption(page, drawer.getByRole('combobox', { name: 'Deployment 2' }), new RegExp(`^${r2}`), r2);
  await drawer.getByRole('radio', { name: 'An endpoint picker' }).click();
  // Next checks the step it leaves: the picker needs its address before the review.
  await drawer.getByRole('button', { name: 'Next: review' }).click();
  await expect(drawer.getByText('The picker’s address, e.g. http://picker.internal:9002')).toBeVisible();
  await expect(drawer.getByRole('button', { name: 'Next: review' })).toBeVisible();
  await drawer.getByLabel('Picker address').fill('http://127.0.0.1:23199');
  await pickOption(
    page,
    drawer.getByRole('combobox', { name: 'When the picker fails' }),
    /Let the gateway pick/,
  );
  await drawer.getByRole('checkbox', { name: /Keep requests for an adapter/ }).click();
  await drawer.getByRole('button', { name: 'Next: review' }).click();
  await drawer.getByRole('checkbox', { name: /Also ask another deployment/ }).click();
  await drawer.getByLabel('Hedge after').fill('300ms');
  await drawer.getByRole('button', { name: 'Add check' }).click();
  await drawer.getByLabel('Header of check 1').fill('x-fail');
  await drawer.getByLabel('Value of check 1').fill('1');
  await expect(
    drawer.getByText(/hedged after 300ms, moving on when an answer has header x-fail set to 1/).first(),
  ).toBeVisible();
  await drawer.getByRole('button', { name: 'Create rule' }).click();
  await expect(drawer).toBeHidden();

  const rules = (await json<Rules>(request, `/routes/${route}`)).spec.tree.rules;
  expect(rules.find((r) => r.name === 'self-hosted')?.target).toEqual({
    type: 'pool',
    children: [r1, r2],
    scheduler: { lora: true },
    picker: 'external',
    picker_service: { url: 'http://127.0.0.1:23199' },
    picker_on_error: 'builtin',
    policies: {
      resilience: {
        hedging: { enabled: true, delay: '300ms' },
        retry: { retry_on_response: [{ header: 'x-fail', equals: '1' }] },
      },
    },
  });

  await page.getByRole('button', { name: 'Show details of self-hosted' }).click();
  await expect(page.getByText('Endpoint picker · this node')).toBeVisible();
  await expect(page.getByText('Not asked yet')).toBeVisible();
  await expect(page.getByText('http://127.0.0.1:23199').last()).toBeVisible();
});

test('the wizard builds a strong-or-weak rule with the learned scorer', async ({
  signedIn: page,
  request,
}) => {
  const drawer = await openWizard(page, 'cheap-first');
  await drawer.getByRole('radio', { name: /Strong or weak model/ }).click();
  await drawer.getByRole('button', { name: 'Next: targets' }).click();
  await pickOption(
    page,
    drawer.getByRole('combobox', { name: 'Strong deployment' }),
    new RegExp(`^${strong}`),
    strong,
  );
  await pickOption(
    page,
    drawer.getByRole('combobox', { name: 'Weak deployment' }),
    new RegExp(`^${weak}`),
    weak,
  );
  await drawer.getByRole('radio', { name: 'A share of prompts' }).click();
  await drawer.getByLabel('Share, in percent').fill('30');
  // Next checks the step it leaves: the learned scorer needs its file first.
  await drawer.getByRole('button', { name: 'Next: review' }).click();
  await expect(drawer.getByText('The path of the trained model file')).toBeVisible();
  await drawer.getByLabel('Model file').fill('/etc/antirion/mf.json');
  await drawer.getByLabel('Embeddings model').fill('embed');
  await drawer.getByLabel('Strong model’s name in the file').fill('gpt-4');
  await drawer.getByLabel('Weak model’s name in the file').fill('mixtral');
  await expect(drawer.getByText(/send about 30% of prompts/).first()).toBeVisible();
  // The gateway reads the model file when the configuration is applied: a missing file is refused there.
  await drawer.getByRole('button', { name: 'Next: review' }).click();
  await drawer.getByRole('button', { name: 'Create rule' }).click();
  await expect(drawer.getByRole('alert').first()).toBeVisible();
  const rules = (await json<Rules>(request, `/routes/${route}`)).spec.tree.rules;
  expect(rules.some((r) => r.name === 'cheap-first')).toBe(false);
});

test('the defaults hedge slow answers and count a finish reason as a failure', async ({
  signedIn: page,
  request,
}) => {
  await page.goto(`routing/?route=${route}&tab=health`);
  await page.getByRole('button', { name: 'Edit defaults' }).click();
  const drawer = page.getByRole('dialog', { name: 'Edit routing defaults' });
  await drawer.getByRole('checkbox', { name: /Also ask another deployment/ }).click();
  await pickOption(page, drawer.getByRole('combobox', { name: 'Extra requests at most' }), '2');
  await drawer.getByRole('button', { name: 'Add check' }).click();
  await pickOption(
    page,
    drawer.getByRole('combobox', { name: 'Check 1', exact: true }),
    'Finish reason is one of',
  );
  await drawer.getByLabel('Finish reasons of check 1').fill('length, content_filter');
  await drawer.getByRole('button', { name: 'Save defaults' }).click();
  await expect(drawer).toBeHidden();
  const policies = await json<{ spec: { resilience?: unknown } }>(request, '/settings/policies');
  expect(policies.spec.resilience).toMatchObject({
    hedging: { enabled: true, max_extra: 2 },
    retry: { retry_on_response: [{ finish_reason: ['length', 'content_filter'] }] },
  });
  const panel = page.getByRole('region', { name: 'Slow and failed answers' });
  await expect(panel).toContainText('after 2s, up to 2 more');
  await expect(panel).toContainText('finish reason length or content_filter');
});

test('a default limit keeps shares for priority classes', async ({ signedIn: page, request }) => {
  const name = `prio_${Date.now().toString(36)}`;
  await page.goto('?settings=api-access');
  const dialog = page.getByRole('dialog', { name: 'Gateway settings' });
  const limits = card(dialog, 'Default rate limits');
  await limits.getByRole('button', { name: 'New limit' }).click();
  const limit = page.getByRole('dialog', { name: 'New default limit' });
  await limit.getByLabel('Name').fill(name);
  await limit.getByLabel('At most').fill('1000');
  await limit.getByRole('button', { name: 'Add class' }).click();
  await limit.getByRole('textbox', { name: 'Class 1', exact: true }).fill('prod');
  await limit.getByLabel('Share of class 1, percent').fill('70');
  await limit.getByRole('button', { name: 'Add class' }).click();
  await limit.getByRole('textbox', { name: 'Class 2', exact: true }).fill('batch');
  await limit.getByLabel('Share of class 2, percent').fill('40');
  await limit.getByRole('button', { name: 'Add limit' }).click();
  await expect(limit.getByText('The shares add up to 110%; at most 100%')).toBeVisible();
  await limit.getByLabel('Share of class 2, percent').fill('20');
  await pickOption(page, limit.getByRole('combobox', { name: 'When a class has used its share' }), /borrow/);
  await limit.getByRole('button', { name: 'Add limit' }).click();
  await expect(limits.getByRole('row').filter({ hasText: name })).toContainText(
    'prod 70% · batch 20%, borrowing',
  );
  const org = await json<{ spec: { policies: { limits: Record<string, unknown> } } }>(
    request,
    '/organizations/default',
  );
  expect(org.spec.policies.limits[name]).toMatchObject({
    priority_shares: { prod: 0.7, batch: 0.2 },
    priority_mode: 'borrow',
  });
  await call(request, 'PATCH', '/organizations/default', {
    spec: { policies: { limits: { [name]: null } } },
  });
});

test('the Models price grid shows and edits prices by service tier', async ({ signedIn: page, request }) => {
  await page.goto(`models/?model=${weak}`);
  await expect(page.getByRole('heading', { name: weak }).first()).toBeVisible();
  await expect(page.getByText('base rates in every tier')).toBeVisible();
  await page.getByRole('button', { name: 'Edit tier prices' }).click();
  const dialog = page.getByRole('dialog', { name: 'Prices by service tier' });
  await expect(dialog).toContainText(`openai/${upstream}`);
  await dialog.getByRole('button', { name: 'Add tier' }).click();
  await dialog.getByLabel('Multiplier of tier 1').fill('0.5');
  await dialog.getByRole('button', { name: 'Add tier' }).click();
  await dialog.getByLabel('Tier 2', { exact: true }).fill('priority');
  await dialog.getByLabel('Multiplier of tier 2').fill('2');
  await dialog.getByLabel('Batches').fill('0.5');
  await dialog.getByRole('button', { name: 'Save tier prices' }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText('flex ×0.5 · priority ×2')).toBeVisible();
  const pricing = await json<{ spec: { models: Record<string, unknown> } }>(request, '/settings/pricing');
  expect(pricing.spec.models[`openai/${upstream}`]).toEqual({
    service_tiers: { flex: '0.5', priority: '2' },
    batch_multiplier: '0.5',
  });
});

test('Budgets edits the list prices the Models price grid edits, not the public Model name', async ({
  signedIn: page,
  request,
}) => {
  const key = `openai/${upstream}`;
  await call(request, 'PATCH', '/settings/pricing', {
    spec: { models: { [key]: { service_tiers: { flex: '0.5', priority: '2' }, batch_multiplier: null } } },
  });
  const openDrawer = async () => {
    await page.goto('budgets/');
    await page.getByRole('button', { name: 'Cost tools' }).click();
    await page.getByRole('menuitem', { name: 'Prices by service tier' }).click();
    return page.getByRole('dialog', { name: 'Prices by service tier' });
  };
  const listPrices = async () =>
    (await json<{ spec: { models: Record<string, unknown> } }>(request, '/settings/pricing')).spec.models;

  // The Model's tiers, set on its upstream model's list price, are the drawer's row for that key.
  let drawer = await openDrawer();
  const row = drawer.getByRole('group', { name: key });
  await expect(row.getByLabel('Tier multipliers')).toHaveValue('flex=0.5, priority=2');
  await row.getByLabel('Tier multipliers').fill('flex=0.4');
  await row.getByLabel('Batch').fill('0.5');
  await drawer.getByRole('button', { name: 'Save prices' }).click();
  await expect(drawer).toBeHidden();
  let models = await listPrices();
  expect(models[key]).toEqual({ service_tiers: { flex: '0.4' }, batch_multiplier: '0.5' });
  expect(models).not.toHaveProperty(weak);

  // A new row picks the upstream model among the ones deployments serve.
  drawer = await openDrawer();
  await drawer.getByRole('button', { name: `Remove ${key}` }).click();
  await drawer.getByRole('button', { name: 'Add an upstream model' }).click();
  const added = drawer.getByRole('group', { name: 'New upstream model' });
  await pickOption(page, added.getByRole('combobox'), key, upstream);
  await drawer.getByRole('group', { name: key }).getByLabel('Tier multipliers').fill('priority=3');
  await drawer.getByRole('button', { name: 'Save prices' }).click();
  await expect(drawer).toBeHidden();
  models = await listPrices();
  expect(models[key]).toEqual({ service_tiers: { priority: '3' } });
  expect(models).not.toHaveProperty(weak);

  // The Models price grid reads what Budgets wrote.
  await page.goto(`models/?model=${weak}`);
  await expect(page.getByText('priority ×3')).toBeVisible();
});
