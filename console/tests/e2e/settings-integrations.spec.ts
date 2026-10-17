import type { APIRequestContext, Locator, Page } from '@playwright/test';

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

function card(dialog: Locator, title: string) {
  return dialog
    .locator('section')
    .filter({ has: dialog.page().getByRole('heading', { name: title, exact: true }) });
}

/** A connection of the catalog, by its name. */
function connection(dialog: Locator, name: string) {
  return dialog.getByRole('listitem').filter({ has: dialog.page().getByText(name, { exact: true }) });
}

type Spec = { spec: Record<string, unknown> };
const dig = (v: unknown, ...path: string[]): unknown =>
  path.reduce<unknown>((o, k) => (o as Record<string, unknown> | undefined)?.[k], v);

const channel = unique('mx-slack');
const hook = unique('mx-hook');
const warehouse = unique('wh');
const traces = unique('tr');
const siem = unique('siem');

test.beforeAll(async ({ request }) => {
  await call(request, 'POST', '/alerts/channels', {
    name: channel,
    spec: { type: 'slack', slack: { webhook_url: 'https://hooks.slack.example/services/T/B/e2e' } },
  });
  await call(request, 'PATCH', '/settings/observability', {
    spec: {
      alerts: { webhooks: { [hook]: { url: 'https://127.0.0.1:9/hook', events: ['budget.exhausted'] } } },
    },
  });
});

test.afterAll(async ({ request }) => {
  await request.fetch(`${ADMIN_URL}/admin/v1/alerts/rules/notify-budget-exceeded`, {
    method: 'DELETE',
    headers: { authorization: `Bearer ${TOKEN}` },
  });
  await call(request, 'DELETE', `/alerts/channels/${channel}`);
  await call(request, 'PATCH', '/settings/observability', {
    spec: {
      alerts: { webhooks: { [hook]: null } },
      metrics: { otlp: null },
      traces: null,
      request_logs: { sinks: { http: { [warehouse]: null }, langfuse: { [traces]: null }, otlp: null } },
    },
  });
  await call(request, 'PATCH', '/settings/security', { spec: { secrets: { stores: { vault: null } } } });
  await call(request, 'PATCH', '/settings/governance', { spec: { audit_sinks: { [siem]: null } } });
  await call(request, 'PATCH', '/settings/smtp', { spec: { enabled: false } });
});

test('connections open the channel editor and write telemetry, request-log copies and a secret store', async ({
  signedIn: page,
  request,
}) => {
  test.setTimeout(60_000);
  const dialog = await settings(page, 'notifications');
  const connections = card(dialog, 'Connections');
  await expect(connection(connections, 'Slack')).toContainText(channel);

  await connection(connections, 'PagerDuty').getByRole('button', { name: 'Connect' }).click();
  const channelDrawer = page.getByRole('dialog', { name: 'New notification channel' });
  await expect(channelDrawer).toBeVisible();
  await channelDrawer.getByRole('button', { name: 'Cancel' }).click();

  await connection(connections, 'OpenTelemetry collector').getByRole('button', { name: 'Connect' }).click();
  let drawer = page.getByRole('dialog', { name: 'OpenTelemetry collector' });
  await drawer.getByLabel('Collector address').fill('http://127.0.0.1:4318');
  await drawer.getByRole('button', { name: 'Add header' }).click();
  await drawer.getByLabel('Header 1 name').fill('x-e2e-key');
  await drawer.getByLabel('Header 1 value').fill('collector-secret-0123');
  await drawer.getByRole('switch', { name: 'Metrics' }).click();
  await drawer.getByRole('switch', { name: 'Traces' }).click();
  await drawer.getByRole('switch', { name: 'Request records as logs' }).click();
  await drawer.getByRole('button', { name: 'Save export' }).click();
  await expect(page.getByText('OpenTelemetry export saved')).toBeVisible();
  let obs = await json<Spec>(request, '/settings/observability');
  expect(dig(obs.spec, 'metrics', 'otlp')).toMatchObject({
    enabled: true,
    endpoint: 'http://127.0.0.1:4318',
  });
  expect(dig(obs.spec, 'traces')).toMatchObject({
    enabled: true,
    exporter: { endpoint: 'http://127.0.0.1:4318' },
  });
  expect(dig(obs.spec, 'request_logs', 'sinks', 'otlp', 'collector')).toMatchObject({
    endpoint: 'http://127.0.0.1:4318',
    headers: { 'x-e2e-key': '[REDACTED]' },
  });
  await expect(connection(connections, 'OpenTelemetry collector')).toContainText('Connected');
  await expect(connection(connections, 'OpenTelemetry collector')).toContainText(
    /Changed by .* ago|just now/,
  );

  await connection(connections, 'Data warehouse')
    .getByRole('button', { name: /Connect|Add another/ })
    .click();
  drawer = page.getByRole('dialog', { name: 'Send the request log to a data warehouse' });
  await drawer.getByRole('textbox', { name: 'Name', exact: true }).fill(warehouse);
  await drawer.getByLabel('Intake address').fill('https://ingest.e2e.example/antirion');
  await drawer.getByRole('button', { name: 'Add header' }).click();
  await drawer.getByLabel('Header 1 name').fill('authorization');
  await drawer.getByLabel('Header 1 value').fill('Bearer warehouse-0123456789');
  await drawer.getByRole('button', { name: 'Add destination' }).click();
  await expect(page.getByText('Destination added')).toBeVisible();
  // Editing leaves the hidden header as it is.
  await connection(connections, 'Data warehouse').getByRole('button', { name: warehouse }).click();
  drawer = page.getByRole('dialog', { name: warehouse });
  await expect(drawer.getByLabel('Header 1 value')).toHaveAttribute(
    'placeholder',
    'Hidden; leave blank to keep',
  );
  await drawer.getByRole('checkbox', { name: 'Include prompts and responses' }).click();
  await drawer.getByRole('button', { name: 'Save destination' }).click();
  await expect(page.getByText('Destination saved')).toBeVisible();
  obs = await json<Spec>(request, '/settings/observability');
  expect(dig(obs.spec, 'request_logs', 'sinks', 'http', warehouse)).toMatchObject({
    url: 'https://ingest.e2e.example/antirion',
    include_content: true,
    headers: { authorization: '[REDACTED]' },
  });

  await connection(connections, 'Trace ingestion')
    .getByRole('button', { name: /Connect|Add another/ })
    .click();
  drawer = page.getByRole('dialog', { name: 'Send traces to a trace-ingestion service' });
  await drawer.getByRole('textbox', { name: 'Name', exact: true }).fill(traces);
  await drawer.getByLabel('Service address').fill('https://traces.e2e.example');
  await drawer.getByLabel('Public key').fill('pk-e2e');
  await drawer.getByLabel('Secret key').fill('sk-e2e-0123456789');
  await drawer.getByRole('button', { name: 'Add destination' }).click();
  await expect(drawer).toBeHidden();
  await expect(page.getByText('Destination added').last()).toBeVisible();
  obs = await json<Spec>(request, '/settings/observability');
  expect(dig(obs.spec, 'request_logs', 'sinks', 'langfuse', traces)).toMatchObject({
    url: 'https://traces.e2e.example',
    public_key: 'pk-e2e',
    secret_key: '[REDACTED]',
  });

  await connection(connections, 'HashiCorp Vault').getByRole('button', { name: 'Connect' }).click();
  drawer = page.getByRole('dialog', { name: 'HashiCorp Vault' });
  await drawer.getByLabel('Vault address').fill('https://vault.e2e.example:8200');
  await drawer.getByLabel('Token').fill('hvs.e2e-token-0123456789');
  await drawer.getByRole('button', { name: 'Connect store' }).click();
  await expect(page.getByText('Secret store connected')).toBeVisible();
  const security = await json<Spec>(request, '/settings/security');
  expect(dig(security.spec, 'secrets', 'stores', 'vault')).toMatchObject({
    address: 'https://vault.e2e.example:8200',
    auth: { method: 'token', token: '[REDACTED]' },
  });
  await expect(connection(connections, 'HashiCorp Vault')).toContainText('Connected');
});

test('the notification matrix edits rules and webhook subscriptions; the email server is set and tested', async ({
  signedIn: page,
  request,
}) => {
  const dialog = await settings(page, 'notifications');
  const matrix = card(dialog, 'Who gets what');
  const toSlack = matrix.getByRole('checkbox', { name: `Budget exceeded to ${channel}` });
  await expect(toSlack).not.toBeChecked();
  await toSlack.click();
  await expect(toSlack).toBeChecked();
  const rule = await json<Spec>(request, '/alerts/rules/notify-budget-exceeded');
  expect(rule.spec).toMatchObject({ kind: 'event', events: ['budget.exhausted'], channels: [channel] });

  const hookCell = matrix.getByRole('checkbox', { name: `Key anomalies to ${hook}` });
  await hookCell.click();
  await expect(hookCell).toBeChecked();
  let obs = await json<Spec>(request, '/settings/observability');
  expect(dig(obs.spec, 'alerts', 'webhooks', hook, 'events')).toEqual(['budget.exhausted', 'key.anomaly']);
  await matrix.getByRole('checkbox', { name: `Budget exceeded to ${hook}` }).click();
  await expect(matrix.getByRole('checkbox', { name: `Budget exceeded to ${hook}` })).not.toBeChecked();
  obs = await json<Spec>(request, '/settings/observability');
  expect(dig(obs.spec, 'alerts', 'webhooks', hook, 'events')).toEqual(['key.anomaly']);

  const mail = card(dialog, 'Email server');
  await mail.getByRole('switch', { name: 'Send email' }).click();
  await mail.getByLabel('Mail server').fill('127.0.0.1');
  await mail.getByLabel('Port').fill('2525');
  await pickOption(page, mail.getByRole('combobox', { name: 'Encryption' }), /None/);
  await mail.getByLabel('From').fill('gateway@e2e.example');
  await mail.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByText('Email server saved')).toBeVisible();
  const smtp = await json<Spec>(request, '/settings/smtp');
  expect(smtp.spec).toMatchObject({
    enabled: true,
    host: '127.0.0.1',
    port: 2525,
    tls: 'none',
    from: 'gateway@e2e.example',
  });
  await mail.getByLabel('Send a test message to').fill('ops@e2e.example');
  await mail.getByRole('button', { name: 'Send test' }).click();
  await expect(page.getByText(/Test message sent|The test message wasn't sent/)).toBeVisible();
});

test('webhooks pause, resume, test and open their log; audit destinations are added, tested and removed', async ({
  signedIn: page,
  request,
}) => {
  const dialog = await settings(page, 'webhooks');
  const hooks = card(dialog, 'Webhooks');
  const row = hooks.getByRole('row').filter({ hasText: hook });
  await expect(row).toContainText('Delivering');
  await expect(row).toContainText('Not signed');
  await row.getByRole('button', { name: `More for ${hook}` }).click();
  await page.getByRole('menuitem', { name: 'Pause' }).click();
  await expect(row).toContainText('Paused');
  expect(
    dig((await json<Spec>(request, '/settings/observability')).spec, 'alerts', 'webhooks', hook, 'enabled'),
  ).toBe(false);
  await row.getByRole('button', { name: `More for ${hook}` }).click();
  await page.getByRole('menuitem', { name: 'Resume' }).click();
  await expect(row).toContainText('Delivering');
  await row.getByRole('button', { name: 'Send test' }).click();
  await expect(page.getByText(/^Test (delivered|not delivered|not sent)$/).first()).toBeVisible();
  await row.getByRole('button', { name: `More for ${hook}` }).click();
  await page.getByRole('menuitem', { name: 'Delivery log' }).click();
  const log = page.getByRole('dialog', { name: `Deliveries to ${hook}` });
  await expect(log).toBeVisible();
  await log.getByRole('button', { name: 'Close' }).click();
  await hooks.getByText('How deliveries work').click();
  await expect(hooks.getByText(/x-antirion-signature/)).toBeVisible();

  const sinks = card(dialog, 'Audit log destinations');
  await sinks.getByRole('button', { name: 'Add destination' }).click();
  const drawer = page.getByRole('dialog', { name: 'New audit log destination' });
  await drawer.getByRole('textbox', { name: 'Name', exact: true }).fill(siem);
  await drawer.getByLabel('Address').fill('https://siem.e2e.example/intake');
  await pickOption(page, drawer.getByRole('combobox', { name: 'Keep trying for' }), '1 day');
  await drawer.getByRole('button', { name: 'Add destination' }).click();
  await expect(page.getByText('Destination added')).toBeVisible();
  const gov = await json<Spec>(request, '/settings/governance');
  expect(dig(gov.spec, 'audit_sinks', siem)).toMatchObject({
    url: 'https://siem.e2e.example/intake',
    max_retry_duration: '24h',
  });
  const sinkRow = sinks.getByRole('row').filter({ hasText: siem });
  await expect(sinkRow).toContainText('kept trying for 1 day');
  await expect(sinkRow.getByRole('button', { name: 'Test' })).toBeEnabled({ timeout: 15_000 });
  await sinkRow.getByRole('button', { name: 'Test' }).click();
  await expect(
    page.getByText(/Test entry (delivered|not delivered)|Couldn't send a test entry/),
  ).toBeVisible();
  await sinkRow.getByRole('button', { name: `More for ${siem}` }).click();
  await page.getByRole('menuitem', { name: 'Remove' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Remove destination' }).click();
  await expect(sinks.getByRole('row').filter({ hasText: siem })).toHaveCount(0);
});
