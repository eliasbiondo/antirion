import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { useSession } from '@/stores/session';

import { useLogin } from '../store';
import { LoginScreen } from '../components/login-screen';

const USER = {
  principal: { type: 'user', id: 'ada' },
  bindings: [],
  permissions: {},
  session: null,
};

function reset() {
  useLogin.setState({
    options: null,
    step: { kind: 'start' },
    error: null,
    passwordError: null,
    organization: '',
    email: '',
    flow: 'sign-in',
    busy: false,
    trustDevice: false,
  });
  useSession.setState({ status: 'unknown', me: null, expiredAt: null });
  window.history.replaceState(null, '', '/');
  server.use(
    http.get('*/admin/v1/auth/me', () =>
      HttpResponse.json(
        { error: { code: 'unauthenticated', message: 'no', request_id: 'x' } },
        { status: 401 },
      ),
    ),
  );
}

function tokenOnly() {
  server.use(
    http.get('*/admin/v1/auth/login_options', () =>
      HttpResponse.json({ error: { code: 'not_found', message: 'no', request_id: 'x' } }, { status: 404 }),
    ),
  );
}

async function signInWithPassword(user = userEvent.setup()) {
  await user.type(await screen.findByLabelText('Email'), 'ada@example.com');
  await user.type(screen.getByLabelText('Password'), 'correct horse');
  await user.click(screen.getByRole('button', { name: 'Sign in' }));
  return user;
}

describe('sign-in', () => {
  it('starts a SAML sign-in at the SAML login of its provider, never trusting the browser', async () => {
    reset();
    server.use(
      http.get('*/admin/v1/auth/login_options', () =>
        HttpResponse.json({
          methods: [
            { type: 'saml', id: 'corp-saml', display_name: 'Corp SAML' },
            { type: 'local', id: 'local', display_name: 'Email and password' },
          ],
          password_reset: true,
          organization: null,
          version: null,
          ticker: null,
          trusted_device: { enabled: true, max_age_days: 30 },
          password: null,
          setup_required: false,
        }),
      ),
    );
    render(<LoginScreen />);
    const saml = await screen.findByRole('link', { name: /Continue with Corp SAML/ });
    expect(saml).toHaveTextContent('SAML');
    const href = saml.getAttribute('href') ?? '';
    expect(href).toContain('/admin/v1/auth/saml/corp-saml/login?return_to=');
    expect(href).not.toContain('trust_device');
    expect(screen.queryByText(/Trust this browser/)).not.toBeInTheDocument();
  });

  it('orders SSO first, then email and password, then the admin token, under a step title', async () => {
    reset();
    render(<LoginScreen />);
    expect(await screen.findByRole('heading', { name: 'Sign in', level: 1 })).toBeInTheDocument();
    const sso = screen.getByRole('link', { name: /Continue with Google Workspace/ });
    expect(sso).toHaveAttribute(
      'href',
      expect.stringContaining('/admin/v1/auth/oidc/workspace/login?return_to='),
    );
    expect(screen.getByRole('link', { name: /Continue with Microsoft Entra/ })).toBeInTheDocument();
    const email = screen.getByLabelText('Email');
    expect(email).toHaveAttribute('autocomplete', 'username');
    expect(screen.getByLabelText('Password')).toHaveAttribute('autocomplete', 'current-password');
    expect(sso.compareDocumentPosition(email) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Forgot password?' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Use an admin token' })).toBeInTheDocument();
    expect(screen.queryByText(/Welcome back/)).not.toBeInTheDocument();
    expect(screen.getByText('Antirion v0.1.0')).toBeInTheDocument();
    // The ticker is configured in the mock, so it shows as one quiet line.
    expect(screen.getByLabelText('Gateway traffic')).toHaveTextContent('1.3M requests');
    expect(screen.queryByText(/region/i)).not.toBeInTheDocument();
  });

  it('shows no traffic figures when the gateway does not turn the ticker on', async () => {
    reset();
    server.use(
      http.get('*/admin/v1/auth/login_options', () =>
        HttpResponse.json({
          methods: [
            { type: 'local', id: 'local' },
            { type: 'token', id: 'token' },
          ],
          password_reset: false,
          organization: null,
          version: null,
          ticker: null,
        }),
      ),
    );
    render(<LoginScreen />);
    expect(await screen.findByLabelText('Email')).toBeInTheDocument();
    expect(screen.queryByLabelText('Gateway traffic')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Forgot password?' })).not.toBeInTheDocument();
    expect(screen.queryByText(/Antirion v/)).not.toBeInTheDocument();
  });

  it('opens on the admin-token form, with where the bootstrap token comes from, when it is the only method', async () => {
    reset();
    tokenOnly();
    render(<LoginScreen />);
    expect(await screen.findByLabelText('Admin token')).toHaveAttribute('autocomplete', 'current-password');
    expect(screen.getByRole('heading', { name: 'Sign in with an admin token' })).toBeInTheDocument();
    expect(screen.getByText('auth.admin.bootstrap_token')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Use another sign-in method' })).not.toBeInTheDocument();
  });

  it('shows one generic error for a refused token and stores nothing', async () => {
    reset();
    tokenOnly();
    server.use(
      http.post('*/admin/v1/auth/session', () =>
        HttpResponse.json(
          {
            error: {
              code: 'unauthenticated',
              message: 'A valid admin credential is required.',
              request_id: 'r',
            },
          },
          { status: 401 },
        ),
      ),
    );
    render(<LoginScreen />);
    await userEvent.type(await screen.findByLabelText('Admin token'), 'wrong-token-value');
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(await screen.findByRole('alert')).toHaveTextContent("That token didn't work.");
    expect(JSON.stringify(localStorage)).not.toContain('wrong-token-value');
  });

  it('keeps the email, clears the password and says nothing about which was wrong', async () => {
    reset();
    server.use(
      http.post('*/admin/v1/auth/local/login', () =>
        HttpResponse.json(
          { error: { code: 'unauthenticated', message: 'no', request_id: 'r' } },
          { status: 401 },
        ),
      ),
    );
    render(<LoginScreen />);
    await signInWithPassword();
    expect(await screen.findByRole('alert')).toHaveTextContent("That email and password don't match.");
    expect(screen.getByLabelText('Email')).toHaveValue('ada@example.com');
    expect(screen.getByLabelText('Password')).toHaveValue('');
    expect(screen.getByLabelText('Password')).toHaveFocus();
  });

  it('asks what to type when a field is empty or the email is malformed', async () => {
    reset();
    render(<LoginScreen />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText('Email'), 'ada');
    await user.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(screen.getByText('Enter an email like name@example.com')).toBeInTheDocument();
    expect(screen.getByText('Enter your password')).toBeInTheDocument();
    await user.type(screen.getByLabelText('Password'), 'x');
    expect(screen.queryByText('Enter your password')).not.toBeInTheDocument();
  });

  it('runs the password then second-factor steps, accepting a pasted "123 456"', async () => {
    reset();
    let mfaBody: unknown = null;
    server.use(
      http.post('*/admin/v1/auth/local/login', () => HttpResponse.json({ next: 'mfa', challenge: 'ch-1' })),
      http.post('*/admin/v1/auth/local/mfa', async ({ request }) => {
        mfaBody = await request.json();
        return HttpResponse.json(USER);
      }),
    );
    render(<LoginScreen />);
    const user = await signInWithPassword();
    expect(await screen.findByRole('heading', { name: 'Enter your verification code' })).toBeInTheDocument();
    expect(screen.getByText('ada@example.com')).toBeInTheDocument();
    const code = screen.getByLabelText('Verification code');
    expect(code).toHaveAttribute('autocomplete', 'one-time-code');
    expect(code).toHaveAttribute('inputmode', 'numeric');
    code.focus();
    await user.paste('123 456');
    await waitFor(() => expect(useSession.getState().status).toBe('authenticated'));
    expect(mfaBody).toMatchObject({ challenge: 'ch-1', totp: '123456', trust_device: false });
  });

  it('clears a refused code and explains that codes change', async () => {
    reset();
    server.use(
      http.post('*/admin/v1/auth/local/login', () => HttpResponse.json({ next: 'mfa', challenge: 'ch-1' })),
      http.post('*/admin/v1/auth/local/mfa', () =>
        HttpResponse.json(
          { error: { code: 'unauthenticated', message: 'no', request_id: 'r' } },
          { status: 401 },
        ),
      ),
    );
    render(<LoginScreen />);
    const user = await signInWithPassword();
    await user.type(await screen.findByLabelText('Verification code'), '000000');
    expect(await screen.findByRole('alert')).toHaveTextContent('Codes change every 30 seconds');
    expect(screen.getByLabelText('Verification code')).toHaveValue('');
    await user.click(screen.getByRole('button', { name: 'Use a recovery code' }));
    expect(screen.getByRole('heading', { name: 'Enter a recovery code' })).toBeInTheDocument();
    expect(screen.getByLabelText('Recovery code')).toBeInTheDocument();
  });

  it('enrols the first factor with a QR code drawn locally, then shows the recovery codes once as a step', async () => {
    reset();
    let enrolBody: unknown = null;
    server.use(
      http.post('*/admin/v1/auth/local/login', () =>
        HttpResponse.json({ next: 'enroll_mfa', challenge: 'ch-2' }),
      ),
      http.post('*/admin/v1/auth/me/mfa/totp', async ({ request }) => {
        enrolBody = await request.json();
        return HttpResponse.json(
          {
            factor_id: 'f1',
            secret: 'JBSWY3DPEHPK3PXP',
            otpauth_uri: 'otpauth://totp/Antirion:ada?secret=JBSWY3DPEHPK3PXP&issuer=Antirion',
          },
          { status: 201 },
        );
      }),
      http.post('*/admin/v1/auth/me/mfa/totp:confirm', () =>
        HttpResponse.json({ recovery_codes: ['aaaa-bbbb', 'cccc-dddd'] }),
      ),
      http.get('*/admin/v1/auth/me', () => HttpResponse.json(USER)),
    );
    const { container } = render(<LoginScreen />);
    const user = await signInWithPassword();
    expect(await screen.findByRole('heading', { name: 'Set up two-step verification' })).toBeInTheDocument();
    expect(screen.getByText('Step 1 of 2')).toBeInTheDocument();
    await waitFor(() => expect(container.querySelector('[data-secret]')).not.toBeNull());
    expect(container.querySelector('[data-secret]')?.textContent).toBe('JBSWY3DPEHPK3PXP');
    expect(screen.getByRole('img', { name: /QR code/ })).toBeInTheDocument();
    expect(enrolBody).toEqual({ challenge: 'ch-2' });
    await user.type(screen.getByLabelText('Verification code'), '654321');
    expect(await screen.findByRole('heading', { name: 'Save your recovery codes' })).toBeInTheDocument();
    expect(screen.getByText('Step 2 of 2')).toBeInTheDocument();
    expect(screen.getByRole('list', { name: 'Recovery codes' })).toHaveTextContent('aaaa-bbbb');
    expect(useSession.getState().status).toBe('authenticated');
    const proceed = screen.getByRole('button', { name: 'Continue to Antirion' });
    expect(proceed).toBeDisabled();
    await user.click(screen.getByLabelText("I've saved my recovery codes"));
    await user.click(proceed);
    expect(useLogin.getState().step.kind).toBe('done');
  });

  it('asks for the organization first and shows it with a way to change it', async () => {
    reset();
    const asked: (string | null)[] = [];
    server.use(
      http.get('*/admin/v1/auth/login_options', ({ request }) => {
        const organization = new URL(request.url).searchParams.get('organization');
        asked.push(organization);
        return HttpResponse.json({
          methods: [
            { type: 'token', id: 'token' },
            ...(organization ? [] : [{ type: 'local', id: 'local' }]),
          ],
          password_reset: false,
          organization,
          version: null,
          ticker: null,
        });
      }),
    );
    render(<LoginScreen />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Choose an organization' }));
    await user.type(screen.getByLabelText('Organization ID'), 'Acme');
    await user.click(screen.getByRole('button', { name: 'Continue' }));
    expect(await screen.findByRole('button', { name: 'Change' })).toBeInTheDocument();
    expect(screen.getByText('acme')).toBeInTheDocument();
    expect(asked).toContain('acme');
    // That organization does not offer passwords, so the token form is what remains.
    expect(screen.getByLabelText('Admin token')).toBeInTheDocument();
  });
});

describe('emailed links', () => {
  it('checks an invitation link before asking for a password and explains an expired one', async () => {
    reset();
    window.history.replaceState(null, '', '/accept-invitation/?token=old-token');
    let checked: unknown = null;
    let accepted = false;
    server.use(
      http.post(/\/auth\/invitations:check$/, async ({ request }) => {
        checked = await request.json();
        return HttpResponse.json({ state: 'expired', email: null, organization: null, expires_at: null });
      }),
      http.post('*/admin/v1/auth/invitations:accept', () => {
        accepted = true;
        return HttpResponse.json({});
      }),
    );
    render(<LoginScreen entry="invitation" />);
    expect(await screen.findByRole('heading', { name: 'This invitation has expired' })).toBeInTheDocument();
    expect(checked).toEqual({ token: 'old-token' });
    expect(accepted).toBe(false);
    expect(screen.queryByLabelText('New password')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Go to sign in' })).toBeInTheDocument();
  });

  it('tells a used invitation and a replaced one apart', async () => {
    for (const [state, heading] of [
      ['used', 'This invitation was already accepted'],
      ['unknown', 'This invitation link no longer works'],
    ] as const) {
      reset();
      window.history.replaceState(null, '', '/accept-invitation/?token=t');
      server.use(
        http.post(/\/auth\/invitations:check$/, () =>
          HttpResponse.json({ state, email: null, organization: null, expires_at: null }),
        ),
      );
      const { unmount } = render(<LoginScreen entry="invitation" />);
      expect(await screen.findByRole('heading', { name: heading })).toBeInTheDocument();
      unmount();
    }
  });

  it('says an invitation link without its token is incomplete, without calling the gateway', async () => {
    reset();
    window.history.replaceState(null, '', '/accept-invitation/');
    let called = false;
    server.use(
      http.post(/\/auth\/invitations:check$/, () => {
        called = true;
        return HttpResponse.json({});
      }),
    );
    render(<LoginScreen entry="invitation" />);
    expect(
      await screen.findByRole('heading', { name: 'This invitation link is incomplete' }),
    ).toBeInTheDocument();
    expect(called).toBe(false);
  });

  it('accepts a live invitation: password with a live rule, then the second factor as step 2 of 3', async () => {
    reset();
    window.history.replaceState(null, '', '/accept-invitation/?token=good-token');
    const bodies: unknown[] = [];
    server.use(
      http.post(/\/auth\/invitations:check$/, () =>
        HttpResponse.json({
          state: 'valid',
          email: 'ada@example.com',
          organization: { name: 'acme', display_name: 'Acme Corp' },
          expires_at: new Date(Date.now() + 3 * 86_400_000 + 60_000).toISOString(),
        }),
      ),
      http.post('*/admin/v1/auth/invitations:accept', async ({ request }) => {
        const body = (await request.json()) as { password: string };
        bodies.push(body);
        if (body.password.length < 12)
          return HttpResponse.json(
            {
              error: {
                code: 'validation_failed',
                message: 'The new password does not follow the password policy.',
                request_id: 'r',
                details: [{ path: 'min_length', code: 'password_policy', message: 'min_length' }],
              },
            },
            { status: 400 },
          );
        if (body.password === 'Used-before-2026')
          return HttpResponse.json(
            {
              error: {
                code: 'validation_failed',
                message: 'The new password does not follow the password policy.',
                request_id: 'r',
                details: [{ path: 'history', code: 'password_policy', message: 'history' }],
              },
            },
            { status: 400 },
          );
        return HttpResponse.json({ next: 'enroll_mfa', challenge: 'ch-3' });
      }),
      http.post('*/admin/v1/auth/me/mfa/totp', () =>
        HttpResponse.json({ factor_id: 'f1', secret: 'JBSWY3DPEHPK3PXP', otpauth_uri: 'otpauth://totp/x' }),
      ),
    );
    render(<LoginScreen entry="invitation" />);
    expect(await screen.findByText('Step 1 of 3')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Join Acme Corp' })).toBeInTheDocument();
    expect(screen.getByText('ada@example.com')).toBeInTheDocument();
    expect(screen.getByText('This link works once, for another 3 days.')).toBeInTheDocument();
    const user = userEvent.setup();
    await user.type(screen.getByLabelText('New password'), 'Used-before-2026');
    expect(screen.getByRole('list', { name: 'Password rules' })).toHaveTextContent('(done)');
    await user.type(screen.getByLabelText('Confirm password'), 'Used-before-2026');
    await user.click(screen.getByRole('button', { name: 'Set password and continue' }));
    expect(await screen.findByText("Choose a password you haven't used recently.")).toBeInTheDocument();
    await user.clear(screen.getByLabelText('New password'));
    await user.type(screen.getByLabelText('New password'), 'Fresh-password-2026');
    await user.clear(screen.getByLabelText('Confirm password'));
    await user.type(screen.getByLabelText('Confirm password'), 'Fresh-password-2026');
    await user.click(screen.getByRole('button', { name: 'Set password and continue' }));
    expect(await screen.findByRole('heading', { name: 'Set up two-step verification' })).toBeInTheDocument();
    expect(screen.getByText('Step 2 of 3')).toBeInTheDocument();
  });

  it('offers a new reset link when a reset link has expired', async () => {
    reset();
    window.history.replaceState(null, '', '/reset-password/?token=late');
    server.use(
      http.post(/\/auth\/local\/password_reset:check$/, () =>
        HttpResponse.json({ state: 'expired', expires_at: null }),
      ),
    );
    render(<LoginScreen entry="reset" />);
    expect(await screen.findByRole('heading', { name: 'This reset link has expired' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Send a new reset link' }));
    expect(screen.getByRole('heading', { name: 'Reset your password' })).toBeInTheDocument();
  });

  it('answers a reset request the same way whatever the address', async () => {
    reset();
    server.use(
      http.post('*/admin/v1/auth/local/password_reset', () =>
        HttpResponse.json({ status: 'accepted' }, { status: 202 }),
      ),
    );
    render(<LoginScreen />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText('Email'), 'ada@example.com');
    await user.click(screen.getByRole('button', { name: 'Forgot password?' }));
    // The email typed on the first step carries over.
    expect(screen.getByLabelText('Email')).toHaveValue('ada@example.com');
    await user.click(screen.getByRole('button', { name: 'Send reset link' }));
    expect(await screen.findByRole('heading', { name: 'Check your email' })).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('If ada@example.com has an account');
  });

  it('asks for the gateway policy length on the reset page and says how long the link works', async () => {
    reset();
    window.history.replaceState(null, '', '/reset-password/?token=fresh');
    server.use(
      http.get('*/admin/v1/auth/login_options', () =>
        HttpResponse.json({
          methods: [{ type: 'local', id: 'local' }],
          password_reset: true,
          organization: null,
          version: null,
          ticker: null,
          trusted_device: { enabled: false, max_age_days: null },
          password: { min_length: 16, max_length: 128, reset_ttl_seconds: 1800 },
          setup_required: false,
          device_authorization: false,
        }),
      ),
      http.post(/\/auth\/local\/password_reset:check$/, () =>
        HttpResponse.json({ state: 'valid', expires_at: new Date(Date.now() + 25 * 60_000).toISOString() }),
      ),
    );
    render(<LoginScreen entry="reset" />);
    expect(await screen.findByRole('heading', { name: 'Choose a new password' })).toBeInTheDocument();
    expect(screen.getByRole('list', { name: 'Password rules' })).toHaveTextContent('16 or more characters');
    expect(screen.getByText(/This link works once, for another 2\d minutes\./)).toBeInTheDocument();
    const user = userEvent.setup();
    await user.type(screen.getByLabelText('New password'), 'Fourteen-chars');
    await user.type(screen.getByLabelText('Confirm password'), 'Fourteen-chars');
    await user.click(screen.getByRole('button', { name: 'Reset password' }));
    expect(await screen.findByText('Use at least 16 characters')).toBeInTheDocument();
  });
});
