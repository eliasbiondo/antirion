import type { APIRequestContext } from '@playwright/test';

import type { Locator, Page } from '@playwright/test';

import { ADMIN_URL, expect, test, TOKEN, unique } from './fixtures';

/** Picks an option of a Combobox whose list is named like its field (the page has a Models listbox too). */
async function pick(page: Page, trigger: Locator, name: string, option: RegExp, search: string) {
  await trigger.click();
  const list = page.getByRole('listbox', { name });
  await expect(list).toBeVisible();
  await page.keyboard.type(search);
  await list.getByRole('option', { name: option }).first().click();
  await expect(list).toBeHidden();
}

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

async function spec(request: APIRequestContext, model: string): Promise<Record<string, unknown>> {
  return (
    (await (await call(request, 'GET', `/models/${model}`)).json()) as { spec: Record<string, unknown> }
  ).spec;
}

const provider = unique('mcp');
const team = unique('mct');
const route = unique('mcr');
const tier = `t${Date.now().toString(36)}`;
const d1 = `${provider}-one`;
const d2 = `${provider}-two`;
const chain = unique('mc-chain');
const gated = unique('mc-gated');
const preview = unique('mc-preview');

test.beforeAll(async ({ request }) => {
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: 'http://127.0.0.1:9/v1', auth: { api_key: 'sk-test-e2e' } },
  });
  for (const d of [d1, d2])
    await call(request, 'POST', '/deployments', {
      name: d,
      spec: { provider, model: 'gpt-4o-mini', price: { input: '0.15', output: '0.60' } },
    });
  await call(request, 'POST', '/routes', { name: route, spec: { tree: d2 } });
  await call(request, 'POST', '/teams', { name: team, spec: { organization: 'default' } });
  await call(request, 'POST', '/models', { name: chain, spec: { target: d1 } });
  await call(request, 'POST', '/models', {
    name: gated,
    spec: { target: d1, lifecycle: { state: 'restricted' }, access: { teams: [] } },
  });
  await call(request, 'POST', '/models', {
    name: preview,
    spec: { target: d1, lifecycle: { state: 'preview' } },
  });
  await call(request, 'POST', `/models/${gated}/access_requests`, { scope: `team/${team}`, reason: 'e2e' });
});

test.afterAll(async ({ request }) => {
  for (const m of [chain, gated, preview]) await call(request, 'DELETE', `/models/${m}`);
  await call(request, 'DELETE', `/routes/${route}`);
  for (const d of [d1, d2]) await call(request, 'DELETE', `/deployments/${d}`);
  await call(request, 'DELETE', `/providers/${provider}`);
  await call(request, 'DELETE', `/teams/${team}`);
  await call(request, 'PATCH', '/settings/routing', { spec: { tiers: { [tier]: null } } });
});

test('a Model’s fallback chain, alias and shared route are edited from its page', async ({
  signedIn: page,
  request,
}) => {
  await page.goto(`models/?model=${chain}`);
  await page.getByRole('button', { name: 'Edit fallback chain' }).click();
  const editor = page.getByRole('dialog', { name: `Fallback chain of ${chain}` });
  await expect(editor.getByText('not other models’ names', { exact: false })).toBeVisible();
  await pick(
    page,
    editor.getByRole('combobox', { name: 'Deployment' }),
    'Deployment',
    new RegExp(`^${d2}`),
    d2,
  );
  await editor.getByRole('button', { name: 'Add step' }).click();
  await editor.getByRole('button', { name: `Move ${d2} up` }).click();
  await editor.getByRole('button', { name: 'Save chain' }).click();
  await expect(editor).toBeHidden();
  await expect
    .poll(async () => (await spec(request, chain)).target)
    .toEqual({ type: 'fallback', children: [d2, d1] });
  await expect(page.getByRole('list', { name: 'Fallback chain' })).toContainText('Fallback 1');

  const alias = unique('mca');
  await page.getByRole('button', { name: 'Add alias', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Add alias' });
  await dialog.getByRole('textbox', { name: 'Alias' }).fill(alias);
  await dialog.getByRole('textbox', { name: /Description/ }).fill('For e2e');
  await dialog.getByRole('button', { name: 'Add alias' }).click();
  await expect(dialog).toBeHidden();
  await expect
    .poll(async () => (await spec(request, chain)).aliases)
    .toEqual({ [alias]: { description: 'For e2e' } });

  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const edit = page.getByRole('dialog', { name: `Edit ${chain}` });
  await edit.getByRole('radio', { name: /A shared route/ }).click();
  await pick(
    page,
    edit.getByRole('combobox', { name: 'Shared route' }),
    'Shared route',
    new RegExp(`^${route}`),
    route,
  );
  await edit.getByRole('button', { name: 'Save changes' }).click();
  await expect(edit).toBeHidden();
  const routed = await spec(request, chain);
  expect(routed.route).toBe(route);
  expect(routed.target).toBeUndefined();
  expect(routed.aliases).toEqual({ [alias]: { description: 'For e2e' } });
});

test('an access request is approved, and a preview Model is promoted', async ({
  signedIn: page,
  request,
}) => {
  await page.goto(`models/?model=${gated}`);
  await expect(page.getByText('1 waiting').first()).toBeVisible();
  await page.getByRole('button', { name: 'Approve', exact: true }).click();
  const confirm = page.getByRole('alertdialog', { name: `Approve access to ${gated}?` });
  await confirm.getByRole('button', { name: 'Approve access' }).click();
  await expect(confirm).toBeHidden();
  await expect.poll(async () => (await spec(request, gated)).access).toEqual({ teams: [team] });
  await expect(page.getByText('No one is waiting', { exact: false })).toBeVisible();

  await page.goto(`models/?model=${preview}`);
  await page.getByRole('button', { name: 'Promote to available' }).click();
  const promote = page.getByRole('alertdialog', { name: `Promote ${preview} to available?` });
  await promote.getByRole('button', { name: 'Promote to available' }).click();
  await expect(promote).toBeHidden();
  await expect.poll(async () => (await spec(request, preview)).lifecycle).toEqual({ state: 'ga' });
});

test('a custom tier is added from the policy tab, and provider bars filter the list', async ({
  signedIn: page,
  request,
}) => {
  await page.goto('models/?tab=policy');
  await expect(page.getByText('Catalog policy')).toBeVisible();
  await page.getByRole('button', { name: 'Add tier' }).click();
  const dialog = page.getByRole('alertdialog', { name: 'Add a tier' });
  await dialog.getByRole('textbox', { name: 'Name' }).fill(tier);
  await dialog.getByRole('textbox', { name: /Description/ }).fill('From e2e');
  await dialog.getByRole('button', { name: 'Add tier' }).click();
  await expect(dialog).toBeHidden();
  const routing = (await (await call(request, 'GET', '/settings/routing')).json()) as {
    spec: { tiers?: Record<string, { description?: string }> };
  };
  expect(routing.spec.tiers?.[tier]).toEqual({ description: 'From e2e' });
  await expect(page.getByRole('table', { name: 'Tiers' })).toContainText('From e2e');

  await page.goto('models/');
  await page
    .getByRole('list', { name: /by provider/ })
    .getByRole('button', { name: new RegExp(provider) })
    .click();
  await expect(page).toHaveURL(new RegExp(`provider=${provider}`));
  await expect(page.getByRole('listbox', { name: 'Models' }).getByRole('option')).toHaveCount(3);
});
