import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

import type { APIRequestContext, Locator, Page } from '@playwright/test';

import { ADMIN_URL, expect, pickOption, test, TOKEN, unique } from './fixtures';

/** A self-signed signing certificate of a test identity provider (no key is kept). */
const CERT = [
  '-----BEGIN CERTIFICATE-----',
  'MIIC9DCCAdygAwIBAgIUKz76uKWEcqkSEIy8/mXk3loS8oIwDQYJKoZIhvcNAQEL',
  'BQAwGjEYMBYGA1UEAwwPZTJlLWlkcC5leGFtcGxlMB4XDTI2MTAxNTE3MjM1OVoX',
  'DTM2MTAxMjE3MjM1OVowGjEYMBYGA1UEAwwPZTJlLWlkcC5leGFtcGxlMIIBIjAN',
  'BgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAul5QyxjDwtlIXxX7buNn7gFz7MgI',
  'OJTBxZmeygCAVl6FzFFawTdLXrtmqCpIs4cJpDDz+6obzQhyUUIRNbpniCEsUbEq',
  '5er1DtV5MZUVzBx4zxYSHX0rwE7x8clz8oljHMK+mrz7LyxOKaEKr+Um5JWOuXJq',
  'bvtF4qOayDGs20J6h3CryQjHjL4Y2cHV3wspU5LaHEpvQWJ6Xhw/AkydF/BIlGcL',
  'zzuWzY3IzW6VCxiQ0hxxUqyV0GQl/HQMOJ/uYwXS6aM9AoQjL9DlkmtWWEodZxrA',
  'uq1XFgqU/+lOVkBRCp5xLHUxVr7PvObOzcWqovbrwu7NBt97uKZp6vs8PwIDAQAB',
  'ozIwMDAdBgNVHQ4EFgQUGTs5pbjLedFwrCaNDEYyArYeZxYwDwYDVR0TAQH/BAUw',
  'AwEB/zANBgkqhkiG9w0BAQsFAAOCAQEAYqXAGgcsQnANY11z/bAPeYInX7JtTGuv',
  'FePPobvggdrDMUNjh+PSq2DhBudhsjdwOmPw2M/iCQUYrPvcjTVMC6mtsUF9e0KW',
  'ef/7nd+wahnazEZXtWyrYDJq02S4hWu+LaCZ902fxADxBNNmZc4kjpJg7+g133d1',
  'r/j2D/5DTKpwucC781w6LOXfxVVyoOp8SRQtxIDpB76THT8RS50eslmQ2PUlnrG/',
  '4SJQyZmev6+b09jpeeZfebbhsSwkrbRLut+gcYM+qMhiet3E7LpccIzjFVZyOCqv',
  'byTcvXIwMaB2NvYmR70szxTB7l+51ubs2LRj2agwW3nstFt6a3xj5g==',
  '-----END CERTIFICATE-----',
].join('\n');

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

/** Adds a value to a creatable multi-select: open it, type, Enter, close. */
async function addValue(page: Page, combobox: Locator, value: string) {
  await combobox.click();
  await page.keyboard.type(value);
  await page.keyboard.press('Enter');
  await page.keyboard.press('Escape');
}

type Auth = { spec: { ip_sets?: Record<string, string[]>; admin?: Record<string, unknown> } };
type Org = { spec: Record<string, unknown> };

let metadataServer: Server;
let metadataUrl = '';
const saml = unique('saml');
const oidc = unique('oidc');
const github = unique('gh');

/** The identity provider's metadata, served the way a real provider publishes it. */
function metadata(entity: string): string {
  const body = CERT.replace(/-----[A-Z ]+-----/g, '').replace(/\s+/g, '');
  return `<?xml version="1.0"?>
<md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" entityID="${entity}">
  <md:IDPSSODescriptor protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol">
    <md:KeyDescriptor use="signing"><ds:KeyInfo><ds:X509Data><ds:X509Certificate>${body}</ds:X509Certificate></ds:X509Data></ds:KeyInfo></md:KeyDescriptor>
    <md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect" Location="${entity}/sso"/>
  </md:IDPSSODescriptor>
</md:EntityDescriptor>`;
}

test.beforeAll(async () => {
  metadataServer = createServer((_req, res) => {
    res.writeHead(200, { 'content-type': 'application/samlmetadata+xml' });
    res.end(metadata('https://e2e-idp.example'));
  });
  await new Promise<void>((resolve) => metadataServer.listen(0, '127.0.0.1', resolve));
  metadataUrl = `http://127.0.0.1:${(metadataServer.address() as AddressInfo).port}/metadata`;
});

test.afterAll(async ({ request }) => {
  await call(request, 'PATCH', '/organizations/default', {
    spec: {
      allowed_ips: null,
      max_key_lifetime: null,
      key_policy: null,
      policies: null,
      logo: null,
      local_login: null,
    },
  });
  await call(request, 'PATCH', '/settings/auth', {
    spec: {
      admin: {
        local: { sso_required_domains: null, password_policy: null, mfa: null },
        session: { trusted_device: null },
      },
      ip_sets: null,
    },
  });
  await call(request, 'PATCH', '/settings/auth', {
    spec: { admin: { sso: { oidc: { [oidc]: null }, saml: { [saml]: null }, oauth: { [github]: null } } } },
  });
  await new Promise((resolve) => metadataServer.close(resolve));
});

test('general shows the organization facts and keeps an uploaded logo inline', async ({
  signedIn: page,
  request,
}) => {
  const dialog = await settings(page, 'general');
  const about = card(dialog, 'About this organization');
  await expect(about.getByText('Identifier')).toBeVisible();
  await expect(about.getByText('Configuration version')).toBeVisible();
  const png = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==',
    'base64',
  );
  const chooser = page.waitForEvent('filechooser');
  await dialog.getByRole('button', { name: 'Upload image' }).click();
  await (await chooser).setFiles({ name: 'logo.png', mimeType: 'image/png', buffer: png });
  await expect(dialog.getByText(/Uploaded image/)).toBeVisible();
  await dialog.getByRole('button', { name: 'Save changes' }).first().click();
  await expect(page.getByText('Organization saved')).toBeVisible();
  const org = await json<Org>(request, '/organizations/default');
  expect(String(org.spec.logo)).toMatch(/^data:image\/png;base64,/);
  await dialog.getByRole('button', { name: 'Remove logo' }).click();
  await dialog.getByRole('button', { name: 'Save changes' }).first().click();
  await expect
    .poll(async () => (await json<Org>(request, '/organizations/default')).spec.logo)
    .toBeUndefined();
});

test('single sign-on providers are added, tested, counted and removed; SAML shows both sides', async ({
  signedIn: page,
  request,
}) => {
  test.setTimeout(60_000);
  const dialog = await settings(page, 'sign-in');
  const sso = card(dialog, 'Single sign-on');

  // OpenID Connect with a role mapping and a default role.
  await sso.getByRole('button', { name: 'Add provider' }).click();
  await page.getByRole('menuitem', { name: /OpenID Connect/ }).click();
  let drawer = page.getByRole('dialog', { name: 'Add OIDC provider' });
  await drawer.getByRole('textbox', { name: 'Name', exact: true }).fill(oidc);
  await drawer.getByLabel('Button label').fill('E2E OIDC');
  await drawer.getByLabel('Issuer').fill('https://login.e2e.example');
  await drawer.getByLabel('Client ID').fill('console');
  await drawer.getByLabel('Client secret').fill('oidc-secret-0123456789');
  await drawer.getByRole('button', { name: 'Add mapping' }).click();
  await drawer.getByLabel('Claim').fill('groups');
  await drawer.getByLabel('Value').fill('ops');
  await pickOption(page, drawer.getByRole('combobox', { name: 'Role' }).first(), /^gateway_viewer/);
  await drawer.getByRole('button', { name: 'Add default role' }).click();
  await pickOption(page, drawer.getByRole('combobox', { name: 'Role' }).nth(1), /^gateway_viewer/);
  await drawer.getByRole('button', { name: 'Add provider' }).click();
  await expect(page.getByText('Provider added').last()).toBeVisible();
  let auth = await json<Auth>(request, '/settings/auth');
  expect((auth.spec.admin as { sso: { oidc: Record<string, unknown> } }).sso.oidc[oidc]).toMatchObject({
    issuer: 'https://login.e2e.example',
    client_id: 'console',
    client_secret: '[REDACTED]',
    display_name: 'E2E OIDC',
    role_mappings: [{ claim: 'groups', value: 'ops', role: 'gateway_viewer', scope: 'gateway' }],
    default_role_bindings: [{ role: 'gateway_viewer', scope: 'gateway' }],
  });
  const row = sso.getByRole('row').filter({ hasText: 'E2E OIDC' });
  await expect(row).toContainText('https://login.e2e.example');
  await expect(row.getByRole('cell').nth(1)).toHaveText('0');
  const popup = page.waitForEvent('popup');
  await row.getByRole('button', { name: 'Test sign-in' }).click();
  expect((await popup).url()).toMatch(new RegExp(`/admin/v1/auth/oidc/${oidc}/login/?\\?test=true`));
  await (await popup).close();

  // Editing keeps the secret when the field is left blank.
  await row.click();
  drawer = page.getByRole('dialog', { name: 'E2E OIDC' });
  await drawer.getByRole('switch', { name: 'Create an account at first sign-in' }).click();
  await drawer.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByText('Provider saved')).toBeVisible();
  auth = await json<Auth>(request, '/settings/auth');
  expect((auth.spec.admin as { sso: { oidc: Record<string, unknown> } }).sso.oidc[oidc]).toMatchObject({
    jit_provisioning: false,
    client_secret: '[REDACTED]',
  });

  // GitHub.
  await sso.getByRole('button', { name: 'Add provider' }).click();
  await page.getByRole('menuitem', { name: 'GitHub' }).click();
  drawer = page.getByRole('dialog', { name: 'Add GitHub provider' });
  await drawer.getByRole('textbox', { name: 'Name', exact: true }).fill(github);
  await drawer.getByLabel('Client ID').fill('Iv1.e2e');
  await drawer.getByLabel('Client secret').fill('github-secret-0123456789');
  await addValue(page, drawer.getByRole('combobox', { name: 'Allowed GitHub organizations' }), 'acme');
  await drawer.getByRole('button', { name: 'Add provider' }).click();
  await expect(page.getByText('Provider added').last()).toBeVisible();
  await expect(sso.getByRole('row').filter({ hasText: github })).toContainText('Members of acme');

  // SAML from a metadata address: the gateway's side, the identity provider's, and a fetch.
  await sso.getByRole('button', { name: 'Add provider' }).click();
  await page.getByRole('menuitem', { name: 'SAML 2.0' }).click();
  drawer = page.getByRole('dialog', { name: 'Add SAML provider' });
  await drawer.getByRole('textbox', { name: 'Name', exact: true }).fill(saml);
  await expect(drawer.getByText(`/admin/v1/auth/saml/${saml}/acs`)).toBeVisible();
  await drawer.getByRole('textbox', { name: 'Metadata address' }).fill(metadataUrl);
  await drawer.getByRole('button', { name: 'Add provider' }).click();
  await expect(page.getByText('Provider added').last()).toBeVisible();
  await sso.getByRole('row').filter({ hasText: saml }).click();
  drawer = page.getByRole('dialog', { name: saml });
  await expect(drawer.getByText('Give these to your identity provider.')).toBeVisible();
  const download = drawer.getByRole('link', { name: 'Download metadata' });
  expect(await download.getAttribute('href')).toContain(`/admin/v1/auth/saml/${saml}/metadata`);
  await drawer.getByRole('button', { name: 'Fetch metadata now' }).click();
  await expect(page.getByText('Metadata fetched', { exact: true })).toBeVisible();
  await expect(drawer.getByText('https://e2e-idp.example', { exact: true })).toBeVisible();
  await expect(drawer.getByText('e2e-idp.example', { exact: true })).toBeVisible();
  await drawer.getByRole('button', { name: 'Cancel' }).click();
  await expect(sso.getByRole('row').filter({ hasText: saml })).toContainText('Metadata fetched');

  // Removing a provider.
  await sso
    .getByRole('row')
    .filter({ hasText: github })
    .getByRole('button', { name: `More for ${github}` })
    .click();
  await page.getByRole('menuitem', { name: 'Remove' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Remove provider' }).click();
  await expect(sso.getByRole('row').filter({ hasText: github })).toHaveCount(0);
});

test('the sign-in policy is edited, with domains that must use single sign-on', async ({
  signedIn: page,
  request,
}) => {
  await call(request, 'PATCH', '/settings/auth', {
    spec: { admin: { sso: { oidc: { [oidc]: { issuer: 'https://login.e2e.example', client_id: 'c' } } } } },
  });
  const dialog = await settings(page, 'sign-in');
  const policy = card(dialog, 'Sign-in & sessions');
  await policy.getByLabel('Minimum length').fill('14');
  await policy.getByRole('checkbox', { name: 'Security key or passkey' }).click();
  await pickOption(page, policy.getByRole('combobox', { name: 'Must change every' }), '90 days');
  await addValue(
    page,
    policy.getByRole('combobox', { name: 'Email domains that must use single sign-on' }),
    'corp.e2e.example',
  );
  await policy.getByRole('switch', { name: 'Offer “trust this browser”' }).click();
  await pickOption(page, policy.getByRole('combobox', { name: 'Trusted for' }), '7 days');
  await policy.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByText('Sign-in settings saved')).toBeVisible();
  const admin = (await json<Auth>(request, '/settings/auth')).spec.admin as Record<
    string,
    Record<string, unknown>
  >;
  expect(admin.local).toMatchObject({
    password_policy: { min_length: 14, max_age: '90d' },
    mfa: { factors: ['totp'] },
    sso_required_domains: ['corp.e2e.example'],
  });
  expect(admin.session).toMatchObject({ trusted_device: { enabled: true, max_age: '7d' } });
});

test('API access edits the allow-list, IP sets, default limits and key lifetimes, and checks a request', async ({
  signedIn: page,
  request,
}) => {
  const provider = unique('apip');
  const model = unique('apim');
  await call(request, 'POST', '/providers', {
    name: provider,
    spec: { type: 'openai', base_url: 'http://127.0.0.1:9/v1', auth: { api_key: 'sk-test-e2e' } },
  });
  await call(request, 'POST', '/deployments', {
    name: `${model}-dep`,
    spec: { provider, model: 'gpt-4o-mini' },
  });
  await call(request, 'POST', '/models', { name: model, spec: { target: `${model}-dep` } });
  const key = unique('apik');
  await call(request, 'POST', '/keys', { name: key, spec: {} });

  const dialog = await settings(page, 'api-access');
  const allow = card(dialog, 'IP allow-list');
  await allow.getByRole('button', { name: 'New IP set' }).click();
  const setDialog = page.getByRole('dialog', { name: 'New IP set' });
  await setDialog.getByLabel('Name').fill('office');
  await setDialog.getByLabel('Ranges').fill('203.0.113.0/24');
  await setDialog.getByRole('button', { name: 'Add set' }).click();
  await expect(page.getByText('IP set added')).toBeVisible();
  await addValue(page, allow.getByRole('combobox', { name: 'Addresses and ranges' }), '10.0.0.0/8');
  await allow.getByRole('combobox', { name: 'IP sets' }).click();
  await page
    .getByRole('listbox')
    .getByRole('option', { name: /^office/ })
    .click();
  await page.keyboard.press('Escape');
  await allow.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByText('IP allow-list saved')).toBeVisible();
  let org = await json<Org>(request, '/organizations/default');
  expect(org.spec.allowed_ips).toEqual(['10.0.0.0/8', 'set:office']);
  expect((await json<Auth>(request, '/settings/auth')).spec.ip_sets).toEqual({ office: ['203.0.113.0/24'] });

  const limits = card(dialog, 'Default rate limits');
  await limits.getByRole('button', { name: 'New limit' }).click();
  const limitDialog = page.getByRole('dialog', { name: 'New default limit' });
  await limitDialog.getByLabel('Name').fill('org_rpm');
  await limitDialog.getByLabel('At most').fill('600');
  await pickOption(page, limitDialog.getByRole('combobox', { name: 'Counted for' }), 'Each key');
  await limitDialog.getByRole('button', { name: 'Add limit' }).click();
  await expect(limits.getByRole('row').filter({ hasText: 'org_rpm' })).toContainText(
    '600 requests per minute',
  );
  org = await json<Org>(request, '/organizations/default');
  expect((org.spec.policies as { limits: Record<string, unknown> }).limits.org_rpm).toEqual({
    unit: 'requests',
    limit: 600,
    per: '1m',
    partition_by: ['key'],
  });

  const lifetimes = card(dialog, 'Key lifetimes');
  await pickOption(page, lifetimes.getByRole('combobox', { name: 'Longest a key may live' }), '90 days');
  await pickOption(page, lifetimes.getByRole('combobox', { name: 'New keys expire after' }), '30 days');
  await lifetimes.getByLabel('Remind owners, days before').fill('10, 2');
  await lifetimes.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByText('Key lifetimes saved')).toBeVisible();
  org = await json<Org>(request, '/organizations/default');
  expect(org.spec).toMatchObject({
    max_key_lifetime: '90d',
    key_policy: { default_lifetime: '30d', reminder_days: [10, 2] },
  });

  const sim = card(dialog, 'Try a request against the limits');
  await pickOption(page, sim.getByRole('combobox', { name: 'Key' }), key, key);
  await pickOption(page, sim.getByRole('combobox', { name: 'Model' }), model, model);
  await sim.getByRole('button', { name: 'Check limits' }).click();
  await expect(sim.getByRole('status')).toContainText(/would (all be let through|be refused)/);
  await expect(sim.getByText('Nothing was sent and no counter changed.')).toBeVisible();
});
