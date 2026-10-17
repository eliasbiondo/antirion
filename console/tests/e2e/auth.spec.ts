import {
  ADMIN_URL,
  expect,
  openTokenForm,
  signIn,
  storedText,
  test,
  TOKEN,
  unique,
  watchCsp,
} from './fixtures';

test.describe('sign-in', () => {
  test('an anonymous visit goes to sign-in and comes back after it', async ({ page }) => {
    const csp = watchCsp(page);
    await page.goto('keys/');
    await expect(page).toHaveURL(/\/ui\/login\/\?next=/);
    await openTokenForm(page);
    await page.getByLabel('Admin token').fill(TOKEN);
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'API keys', level: 1 })).toBeVisible();
    expect(csp).toEqual([]);
  });

  test('a wrong token shows one generic error and stores nothing', async ({ page }) => {
    await page.goto('login/');
    await openTokenForm(page);
    await page.getByLabel('Admin token').fill('not-a-real-admin-token');
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await expect(page.getByRole('alert').filter({ hasText: /That token didn't work/ })).toBeVisible();
    const stored = await storedText(page);
    expect(stored).not.toContain('not-a-real-admin-token');
    expect(stored).not.toContain(TOKEN);
  });

  test('the account menu shows the principal and signs out', async ({ page }) => {
    await signIn(page);
    await page.getByRole('button', { name: /Account menu/ }).click();
    await expect(page.getByRole('menu', { name: 'Account' })).toContainText('Bootstrap admin');
    await expect(page.getByRole('menu', { name: 'Account' })).toContainText(/gateway admin/i);
    await page.getByRole('menuitem', { name: 'Sign out' }).click();
    await expect(page).toHaveURL(/\/ui\/login\//);
    // A gateway with no account yet opens on the bootstrap token instead.
    await expect(
      page.getByRole('heading', { name: /^(Sign in|Set up your gateway)$/, level: 1 }),
    ).toBeVisible();
    await page.goto('models/');
    await expect(page).toHaveURL(/\/ui\/login\//);
  });

  test('the bootstrap token sees what its account area offers, not refusals', async ({ signedIn }) => {
    await signedIn.goto('?account=tokens');
    const account = signedIn.getByRole('dialog', { name: 'Account' });
    await expect(account).toContainText('Personal tokens belong to a user account.');
    await expect(
      account.getByRole('navigation', { name: 'Account sections' }).getByRole('button'),
    ).toHaveText(['Profile', 'Preferences', 'Security']);
    await account.getByRole('button', { name: 'Security', exact: true }).click();
    await expect(account.getByRole('table', { name: 'Your sessions' })).toContainText('Admin token');
    await expect(account).not.toContainText(/not permitted/i);
  });

  test('an unknown path is a not-found page', async ({ page }) => {
    const response = await page.goto('no-such-screen/');
    expect(response?.status()).toBe(404);
    await expect(page.getByRole('heading', { name: 'Page not found' })).toBeVisible();
  });

  test('console responses carry the security headers', async ({ request }) => {
    const response = await request.get('login/');
    const headers = response.headers();
    expect(headers['content-security-policy']).toContain("script-src 'self'");
    expect(headers['x-frame-options']).toBe('DENY');
    const html = await response.text();
    expect(html).not.toMatch(/<script(?![^>]*\bsrc=)[^>]*>[^<]/);
    expect(html).not.toMatch(/\sstyle="/);
  });
});

/** A signing certificate for a SAML identity provider that never signs anything in these tests. */
const IDP_CERTIFICATE = `-----BEGIN CERTIFICATE-----
MIIC9jCCAd6gAwIBAgIUGP64rRvvzQcfRIjthOIsZrR9zsMwDQYJKoZIhvcNAQEL
BQAwGjEYMBYGA1UEAwwPaWRwLmUyZS5leGFtcGxlMCAXDTI2MTAxNTE4MTgwNloY
DzIxMjYwOTIxMTgxODA2WjAaMRgwFgYDVQQDDA9pZHAuZTJlLmV4YW1wbGUwggEi
MA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQC83A2HjyOUf1Sy4BzHs+2hbED0
9BowpVQuJF2UmGOErItlLTG/G/5H9SGrG6RpKMzquUT1OjSK9vmwJZmTuHCsUu3R
1YO1toWXXY4j3pBKxcN07baIdtleZfrU7g930fVEI4kJBJGOfjkWLNDu581e3z6U
2gmY6lHZ7YwWoD3/7sd/ye/myFctnPPfclw+RgtkWoqWjgQK3zGbcFy6RHJCDaAp
mxiXiDAhbbyLP15vgBFUgoEwfimSKl8KMkV4oCXZWbntJgxa1uSzlv0bOoDPxyQp
f2bRtjjQluuz534BpTKA/9knVxDbJ+uyNUP6SdZ771VpILxREgou8UheDAYrAgMB
AAGjMjAwMB0GA1UdDgQWBBQvYAosk7UVzKAS7jOQFNurY7EXzTAPBgNVHRMBAf8E
BTADAQH/MA0GCSqGSIb3DQEBCwUAA4IBAQB4RkIzPMbDyWSuFHN5Xy8w8U2RwWmu
mAYfQZf9AWIobhdsA6g28Yl1VWtfXnTbEsUUBvlDg+fwJvH7NsSMIMu9ah6WGTHI
VVD0/Saa/eW4BX25JQvF4iNz6aaG+RWqyJYSanQhh8vwRGcaOgK+t+1vw0bMV6M1
SXYUdga3a5kbspyPeobFzSfI/SxWBr5p4B915+29aJ6ecQrmmERadnkGi46285+i
6QYc9kfu8+w30nvcnd6shq5o00xGxNNu//8I13hlb7QuYerMSQyVct6BF1r4XQhh
OvltSRDMy/vx+I65pLZfSeXyXlFCg8BeoxxgfJIsXAOrm2iAaY3KOWZ4
-----END CERTIFICATE-----`;

test.describe('single sign-on with SAML', () => {
  const provider = unique('saml');
  const ssoUrl = 'https://idp.e2e.example/sso';

  async function patchAuth(request: import('@playwright/test').APIRequestContext, saml: unknown) {
    const response = await request.fetch(`${ADMIN_URL}/admin/v1/settings/auth`, {
      method: 'PATCH',
      headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/merge-patch+json' },
      data: { spec: { admin: { sso: { saml } } } },
    });
    expect(response.status(), await response.text()).toBeLessThan(300);
  }

  test.beforeAll(async ({ request }) => {
    await patchAuth(request, {
      [provider]: {
        display_name: 'Corp SAML',
        idp: {
          entity_id: 'https://idp.e2e.example/metadata',
          sso_url: ssoUrl,
          certificates: [IDP_CERTIFICATE],
        },
      },
    });
  });

  test.afterAll(async ({ request }) => {
    await patchAuth(request, { [provider]: null });
  });

  test("the sign-in page offers the provider and its button starts the provider's SAML login", async ({
    page,
    request,
  }) => {
    const csp = watchCsp(page);
    await page.goto('login/');
    const button = page.getByRole('link', { name: /Continue with Corp SAML/ });
    // A gateway without accounts opens on the bootstrap token, with single sign-on one step away.
    const other = page.getByRole('button', { name: 'Use another sign-in method' });
    await expect(button.or(other)).toBeVisible();
    if (await other.isVisible()) await other.click();
    await expect(button).toBeVisible();
    await expect(button).toContainText('SAML');
    const href = (await button.getAttribute('href')) ?? '';
    expect(href).toContain(`/admin/v1/auth/saml/${provider}/login?return_to=`);
    const started = await request.get(new URL(href, page.url()).toString(), { maxRedirects: 0 });
    expect(started.status()).toBe(302);
    expect(started.headers().location).toMatch(new RegExp(`^${ssoUrl}\\?SAMLRequest=`));
    expect(csp).toEqual([]);
  });
});
