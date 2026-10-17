import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, pickOption, test, TOKEN, unique } from './fixtures';

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

test.describe('adaptive routing', () => {
  const provider = unique('adp');
  const route = unique('adr');
  const d1 = `${provider}-a`;
  const d2 = `${provider}-b`;

  test.beforeAll(async ({ request }) => {
    await call(request, 'PATCH', '/settings/observability', {
      spec: { feedback: { enabled: true, metrics: { thumbs: { type: 'boolean' } } } },
    });
    await call(request, 'POST', '/providers', {
      name: provider,
      spec: { type: 'openai', base_url: 'http://127.0.0.1:9/v1', auth: { api_key: 'sk-test-e2e' } },
    });
    for (const d of [d1, d2])
      await call(request, 'POST', '/deployments', { name: d, spec: { provider, model: 'gpt-4o-mini' } });
    await call(request, 'PUT', `/routes/${route}`, {
      spec: {
        tree: {
          type: 'conditional',
          rules: [{ name: 'existing', when: { field: 'identity.team', eq: 'nobody' }, target: d2 }],
          default: d1,
        },
      },
    });
  });

  test.afterAll(async ({ request }) => {
    await call(request, 'DELETE', `/routes/${route}`);
    for (const d of [d1, d2]) await call(request, 'DELETE', `/deployments/${d}`);
    await call(request, 'DELETE', `/providers/${provider}`);
    await call(request, 'PATCH', '/settings/observability', { spec: { feedback: null } });
  });

  test('the wizard builds an adaptive split that learns from feedback', async ({
    signedIn: page,
    request,
  }) => {
    await page.goto(`routing/?route=${route}`);
    await expect(page.getByRole('heading', { name: 'Routing policy' })).toBeVisible();
    await page.getByRole('button', { name: 'Create rule' }).first().click();
    const drawer = page.getByRole('dialog', { name: 'Create rule' });
    await drawer.getByLabel('Name').fill('learn-best');
    await drawer.getByLabel('Value of condition 1').fill('support');
    await drawer.getByRole('button', { name: 'Next: strategy' }).click();
    await drawer.getByRole('radio', { name: /Adaptive split/ }).click();
    await drawer.getByRole('button', { name: 'Next: targets' }).click();
    await pickOption(page, drawer.getByRole('combobox', { name: 'Deployment 1' }), new RegExp(`^${d1}`), d1);
    await pickOption(page, drawer.getByRole('combobox', { name: 'Deployment 2' }), new RegExp(`^${d2}`), d2);
    await drawer.getByLabel('Experiment').fill('support-trial');
    await pickOption(page, drawer.getByRole('combobox', { name: 'Feedback metric' }), 'thumbs');
    await drawer.getByRole('checkbox', { name: /Stop learning/ }).click();
    await drawer.getByRole('button', { name: 'Next: review' }).click();
    await drawer.getByRole('button', { name: 'Create rule' }).click();
    await expect(drawer).toBeHidden();

    const r = (await (await call(request, 'GET', `/routes/${route}`)).json()) as {
      spec: { tree: { rules: { name: string; target: unknown }[] } };
    };
    expect(r.spec.tree.rules.find((x) => x.name === 'learn-best')?.target).toMatchObject({
      type: 'weighted',
      algorithm: 'thompson',
      experiment: 'support-trial',
      metric: 'thumbs',
      children: [
        { id: 'arm-1', deployment: d1, weight: 1 },
        { id: 'arm-2', deployment: d2, weight: 1 },
      ],
      stop_after: { delta: 0.05 },
    });
  });
});
