import type { APIRequestContext } from '@playwright/test';

import { ADMIN_URL, expect, test, TOKEN, unique } from './fixtures';

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

test('the Models screen says which catalog prices it uses', async ({ signedIn: page, request }) => {
  const provider = unique('cat');
  const model = unique('catm');
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: 'http://127.0.0.1:9/v1', auth: { api_key: 'sk-test-e2e' } },
  });
  await call(request, 'POST', '/deployments', {
    name: `${provider}-d`,
    spec: { provider, model: 'gpt-4o-mini' },
  });
  await call(request, 'POST', '/models', {
    name: model,
    spec: { target: { deployment: `${provider}-d` } },
  });
  try {
    await page.goto('models/');
    await expect(page.getByText(/Prices \d{4}-\d{2}-\d{2}/).first()).toBeVisible();
  } finally {
    await call(request, 'DELETE', `/models/${model}`);
    await call(request, 'DELETE', `/deployments/${provider}-d`);
    await call(request, 'DELETE', `/providers/${provider}`);
  }
});
