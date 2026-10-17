import { readFileSync } from 'node:fs';

import type { APIRequestContext, Page } from '@playwright/test';

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

async function json<T>(request: APIRequestContext, path: string): Promise<T> {
  return (await (await call(request, 'GET', path)).json()) as T;
}

async function settings(page: Page, section: string) {
  await page.goto(`?settings=${section}`);
  const dialog = page.getByRole('dialog', { name: 'Gateway settings' });
  await expect(dialog).toBeVisible();
  return dialog;
}

/** A card of the settings dialog, by its title. */
function card(dialog: ReturnType<Page['getByRole']>, title: string) {
  return dialog
    .locator('section')
    .filter({ has: dialog.page().getByRole('heading', { name: title, exact: true }) });
}

const org = unique('dzo');

test.beforeAll(async ({ request }) => {
  await call(request, 'POST', '/organizations', { name: org, spec: { display_name: 'Danger zone test' } });
});

test.afterAll(async ({ request }) => {
  await call(request, 'PATCH', '/settings/policies', { spec: { observability: null, guardrails: null } });
  await call(request, 'PATCH', '/settings/governance', {
    spec: { dual_control: null, maintenance_windows: null },
  });
  await call(request, 'PATCH', '/settings/routing', { spec: { dry_run_all: null } });
  await request.fetch(`${ADMIN_URL}/admin/v1/organizations/${org}`, {
    method: 'DELETE',
    headers: { authorization: `Bearer ${TOKEN}` },
  });
});

test('data & privacy writes the gateway policy and adds and removes a custom entity', async ({
  signedIn: page,
  request,
}) => {
  const dialog = await settings(page, 'privacy');
  const content = card(dialog, 'Prompts and responses');
  await content.getByRole('switch', { name: 'Keep prompts and responses' }).click();
  await content.getByRole('switch', { name: 'Remove personal data before storing' }).click();
  await content.getByRole('radio', { name: /Replace it with a hash/ }).click();
  await content.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByText('Data settings saved')).toBeVisible();
  const policy = await json<{ spec: { observability?: Record<string, unknown> } }>(
    request,
    '/settings/policies',
  );
  expect(policy.spec.observability).toMatchObject({
    zero_data_retention: false,
    content: { enabled: true, redact_pii: true, on_pii: 'hash' },
  });

  const label = `E2E_${Date.now().toString(36).toUpperCase()}`;
  const entities = card(dialog, 'Custom entities');
  await entities.getByRole('button', { name: 'Add entity' }).click();
  const drawer = page.getByRole('dialog', { name: 'Add custom entity' });
  await drawer.getByLabel('Entity type').fill(label);
  await drawer.getByLabel('Patterns').fill('ACC-[0-9]{8}');
  await drawer.getByRole('button', { name: 'Add entity' }).click();
  await expect(drawer).toBeHidden();
  const name = `entity_${label.toLowerCase()}`;
  const table = entities.getByRole('table', { name: 'Custom entities' });
  await expect(table.getByRole('row').filter({ hasText: name })).toContainText('Every request');
  const attached = await json<{ spec: { guardrails?: { input?: string[] } } }>(request, '/settings/policies');
  expect(attached.spec.guardrails?.input).toContain(name);

  await table.getByRole('button', { name: `Actions for ${name}` }).click();
  await page.getByRole('menuitem', { name: 'Remove entity' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Remove entity' }).click();
  await expect(table.getByRole('row').filter({ hasText: name })).toHaveCount(0);
  const gone = await request.fetch(`${ADMIN_URL}/admin/v1/guardrails/${name}`, {
    headers: { authorization: `Bearer ${TOKEN}` },
  });
  expect(gone.status()).toBe(404);
});

test('the audit log leaves reads out on the server', async ({ signedIn: page }) => {
  const asked = page.waitForRequest((r) => /\/admin\/v1\/audit\?.*kind=write/.test(r.url()));
  await settings(page, 'audit');
  await asked;
});

test('a configuration version shows what rolling back changes, and the rollback undoes it', async ({
  signedIn: page,
  request,
}) => {
  const team = unique('rbt');
  await call(request, 'POST', '/teams', { name: team, spec: { organization: 'default' } });
  const running = await json<{ items: { generation: number }[] }>(request, '/config/generations?limit=2');
  const before = running.items[1]?.generation ?? 1;
  const dialog = await settings(page, 'configuration');
  const versions = dialog.getByRole('table', { name: 'Configuration versions' });
  await versions
    .getByRole('row')
    .filter({ hasText: String(before) })
    .first()
    .click();
  const drawer = page.getByRole('dialog', { name: `Version ${before}` });
  await expect(drawer.getByText('Rolling back would change 1 setting')).toBeVisible();
  await expect(drawer).toContainText(team);
  await drawer.getByRole('button', { name: 'Roll back to this version' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Roll back' }).click();
  await expect(page.getByText(`Rolled back to version ${before}`)).toBeVisible();
  const gone = await request.fetch(`${ADMIN_URL}/admin/v1/teams/${team}`, {
    headers: { authorization: `Bearer ${TOKEN}` },
  });
  expect(gone.status()).toBe(404);
});

test('the danger zone: two-person rule, a held cache clear, a maintenance window and the dry run', async ({
  signedIn: page,
  request,
}) => {
  const dialog = await settings(page, 'danger');
  const rule = card(dialog, 'Two-person rule');
  await rule.getByRole('checkbox', { name: /Clear the whole cache/ }).click();
  await rule.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByText('Two-person rule saved')).toBeVisible();

  const purge = card(dialog, 'Clear the whole cache');
  await expect(purge.getByText(/Needs a second person/)).toBeVisible();
  await purge.getByRole('button', { name: 'Clear cache' }).click();
  const confirm = page.getByRole('alertdialog', { name: 'Clear the whole cache?' });
  await confirm.getByRole('textbox', { name: /Type/ }).fill('gateway');
  await confirm.getByRole('button', { name: 'Clear cache' }).click();
  const waiting = card(dialog, 'Waiting for approval');
  await expect(waiting.getByText('Clear the whole cache')).toBeVisible();
  await expect(waiting.getByRole('link', { name: 'View request' })).toBeVisible();
  const held = await json<{ items: { id: string; kind: string }[] }>(request, '/change_requests?state=open');
  for (const r of held.items.filter((x) => x.kind === 'action'))
    await call(request, 'POST', `/change_requests/${r.id}:cancel`, {});

  const windows = card(dialog, 'Maintenance windows');
  await windows.getByRole('button', { name: 'Add window' }).click();
  const add = page.getByRole('dialog', { name: 'Add maintenance window' });
  await add.getByLabel(/What's planned/).fill('E2E upgrade');
  await add.getByRole('button', { name: 'Add window' }).click();
  await expect(add).toBeHidden();
  await expect(windows.getByRole('table', { name: 'Maintenance windows' })).toContainText('E2E upgrade');
  const gov = await json<{ spec: { maintenance_windows?: { description?: string }[] } }>(
    request,
    '/settings/governance',
  );
  expect(gov.spec.maintenance_windows?.map((w) => w.description)).toContain('E2E upgrade');

  const dry = card(dialog, 'Dry run every request');
  await dry.getByRole('switch', { name: 'Dry run every request' }).click();
  const start = page.getByRole('alertdialog', { name: 'Dry run every request?' });
  await start.getByRole('textbox', { name: /Type/ }).fill('dry run');
  await start.getByRole('button', { name: 'Start dry run' }).click();
  await expect(page.getByText('Dry run is on for every request')).toBeVisible();
  const status = await json<{ dry_run_all?: boolean }>(request, '/status');
  expect(status.dry_run_all).toBe(true);
  await dry.getByRole('switch', { name: 'Dry run every request' }).click();
  await page
    .getByRole('alertdialog', { name: 'Turn off the dry run?' })
    .getByRole('button', { name: 'Turn off dry run' })
    .click();
  await expect(page.getByText('Dry run is on for every request')).toBeHidden();
});

test('an organization is exported to a ZIP, then scheduled for deletion and kept', async ({
  signedIn: page,
  request,
}) => {
  const dialog = await settings(page, 'danger');
  const offboard = card(dialog, 'Export or delete an organization');
  await pickOption(page, offboard.getByRole('combobox', { name: 'Organization' }), new RegExp(org));
  await offboard.getByRole('button', { name: 'Start export' }).click();
  const download = offboard.getByRole('link', { name: 'Download ZIP' });
  await expect(download).toBeVisible({ timeout: 20_000 });
  const [file] = await Promise.all([page.waitForEvent('download'), download.click()]);
  const path = await file.path();
  expect(readFileSync(path).subarray(0, 2).toString()).toBe('PK');

  await offboard.getByRole('button', { name: 'Schedule deletion' }).click();
  const confirm = page.getByRole('alertdialog', { name: `Delete organization ${org}?` });
  await pickOption(page, confirm.getByRole('combobox', { name: 'Delete it after' }), '7 days');
  await confirm.getByRole('textbox', { name: /Type/ }).fill(org);
  await confirm.getByRole('button', { name: 'Schedule deletion' }).click();
  await expect(offboard.getByText('Deletion scheduled')).toBeVisible();
  const read = await json<{ metadata: { deletion?: { state: string } } }>(request, `/organizations/${org}`);
  expect(read.metadata.deletion?.state).toBe('scheduled');

  await offboard.getByRole('button', { name: 'Cancel deletion' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Cancel deletion' }).click();
  await expect(offboard.getByText('Deletion scheduled')).toBeHidden();
  const kept = await json<{ metadata: { deletion?: unknown } }>(request, `/organizations/${org}`);
  expect(kept.metadata.deletion).toBeUndefined();
});
