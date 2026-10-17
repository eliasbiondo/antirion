import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import type { LoginOptions, Me } from '@/lib/api/types';
import { server } from '@/mocks/node';
import { useSession } from '@/stores/session';

import { ConfirmEmailScreen } from '../components/confirm-email-screen';
import { LoginScreen } from '../components/login-screen';
import { useLogin } from '../store';
import { spanInWords, timeLeft } from '../words';

const BASE: LoginOptions = {
  methods: [
    { type: 'oidc', id: 'corp', display_name: 'Corp SSO' },
    { type: 'oauth', id: 'github', display_name: 'GitHub' },
    { type: 'local', id: 'local' },
    { type: 'token', id: 'token' },
  ],
  password_reset: true,
  organization: null,
  version: null,
  ticker: null,
  trusted_device: { enabled: true, max_age_days: 14 },
  password: { min_length: 12, max_length: 128, reset_ttl_seconds: 3600 },
  setup_required: false,
  device_authorization: false,
};

function reset(options: Partial<LoginOptions> = {}) {
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
    http.get('*/admin/v1/auth/login_options', () => HttpResponse.json({ ...BASE, ...options })),
  );
}

describe('sign-in facts', () => {
  it('words a link lifetime', () => {
    expect(spanInWords(1800)).toBe('30 minutes');
    expect(spanInWords(7200)).toBe('2 hours');
    expect(spanInWords(86_400 * 7)).toBe('7 days');
    expect(timeLeft(new Date(Date.now() - 1000).toISOString())).toBeNull();
  });

  it('states that password sign-in needs a second step', async () => {
    reset();
    render(<LoginScreen />);
    expect(await screen.findByText(/Password sign-in always asks for a second step/)).toBeInTheDocument();
  });

  it('offers to trust the browser for the configured days on the SSO buttons, only for OpenID providers', async () => {
    reset();
    render(<LoginScreen />);
    const box = await screen.findByRole('checkbox', { name: /Trust this browser for 14 days/ });
    const corp = screen.getByRole('link', { name: /Continue with Corp SSO/ });
    const github = screen.getByRole('link', { name: /Continue with GitHub/ });
    expect(corp.getAttribute('href')).not.toContain('trust_device');
    await userEvent.click(box);
    expect(corp.getAttribute('href')).toContain('trust_device=true');
    expect(github.getAttribute('href')).not.toContain('trust_device');
  });

  it('hides "trust this browser" when the gateway does not offer trusted devices', async () => {
    reset({ trusted_device: { enabled: false, max_age_days: null } });
    render(<LoginScreen />);
    await screen.findByRole('link', { name: /Continue with Corp SSO/ });
    expect(screen.queryByRole('checkbox', { name: /Trust this browser/ })).not.toBeInTheDocument();
  });

  it('asks for the second step with the days a trusted browser skips it', async () => {
    reset({ trusted_device: { enabled: true, max_age_days: 7 } });
    let body: unknown = null;
    server.use(
      http.post('*/admin/v1/auth/local/login', () => HttpResponse.json({ next: 'mfa', challenge: 'ch-1' })),
      http.post('*/admin/v1/auth/local/mfa', async ({ request }) => {
        body = await request.json();
        return HttpResponse.json(
          { error: { code: 'unauthenticated', message: 'no', request_id: 'r' } },
          { status: 401 },
        );
      }),
    );
    render(<LoginScreen />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText('Email'), 'ada@example.com');
    await user.type(screen.getByLabelText('Password'), 'correct horse');
    await user.click(screen.getByRole('button', { name: 'Sign in' }));
    await user.click(await screen.findByRole('checkbox', { name: /Trust this browser for 7 days/ }));
    await user.type(screen.getByLabelText('Verification code'), '123456');
    await screen.findByRole('alert');
    expect(body).toMatchObject({ challenge: 'ch-1', totp: '123456', trust_device: true });
  });

  it('opens on the bootstrap token when the gateway has no account yet', async () => {
    reset({ setup_required: true });
    render(<LoginScreen />);
    expect(await screen.findByRole('heading', { name: 'Set up your gateway' })).toBeInTheDocument();
    expect(screen.getByLabelText('Admin token')).toHaveAccessibleDescription(/Paste the bootstrap token/);
    expect(screen.getByText(/This gateway has no accounts yet/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Use another sign-in method' }));
    expect(await screen.findByRole('heading', { name: 'Sign in' })).toBeInTheDocument();
    expect(screen.getByLabelText('Email')).toBeInTheDocument();
  });

  it('says how long a reset link works after asking for one', async () => {
    reset();
    server.use(
      http.post('*/admin/v1/auth/local/password_reset', () =>
        HttpResponse.json({ status: 'accepted' }, { status: 202 }),
      ),
    );
    render(<LoginScreen />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Forgot password?' }));
    await user.type(screen.getByLabelText('Email'), 'ada@example.com');
    await user.click(screen.getByRole('button', { name: 'Send reset link' }));
    expect(await screen.findByRole('status')).toHaveTextContent('The link works once, for 1 hour.');
  });
});

const ADA: Me = {
  principal: { type: 'user', id: 'ada', authenticated_by: 'session' },
  bindings: [],
  permissions: {},
  session: null,
  user: { name: 'ada', display_name: 'Ada', email: 'ada@example.com' },
};

describe('confirm email', () => {
  it('confirms the change for the signed-in user', async () => {
    let body: unknown = null;
    useSession.setState({ status: 'unknown', me: null, expiredAt: null });
    server.use(
      http.get('*/admin/v1/auth/me', () => HttpResponse.json(ADA)),
      http.patch('*/admin/v1/auth/me', async ({ request }) => {
        body = await request.json();
        return HttpResponse.json({ ...ADA, user: { ...ADA.user, email: 'ada@new.example' } });
      }),
    );
    window.history.replaceState(null, '', '/confirm-email/?token=tok-1');
    render(<ConfirmEmailScreen />);
    expect(await screen.findByRole('heading', { name: 'Your email is confirmed' })).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('ada@new.example');
    expect(body).toEqual({ email_token: 'tok-1' });
  });

  it('asks to sign in first and comes back', async () => {
    useSession.setState({ status: 'unknown', me: null, expiredAt: null });
    server.use(
      http.get('*/admin/v1/auth/me', () =>
        HttpResponse.json(
          { error: { code: 'unauthenticated', message: 'no', request_id: 'x' } },
          { status: 401 },
        ),
      ),
    );
    window.history.replaceState(null, '', '/confirm-email/?token=tok-2');
    render(<ConfirmEmailScreen />);
    const link = await screen.findByRole('link', { name: 'Sign in' });
    expect(decodeURIComponent(link.getAttribute('href') ?? '')).toContain('next=/confirm-email/?token=tok-2');
  });

  it('explains a link that no longer works', async () => {
    useSession.setState({ status: 'unknown', me: null, expiredAt: null });
    server.use(
      http.get('*/admin/v1/auth/me', () => HttpResponse.json(ADA)),
      http.patch('*/admin/v1/auth/me', () =>
        HttpResponse.json(
          { error: { code: 'invalid_parameter', message: 'the token is not valid', request_id: 'r' } },
          { status: 400 },
        ),
      ),
    );
    window.history.replaceState(null, '', '/confirm-email/?token=old');
    render(<ConfirmEmailScreen />);
    expect(await screen.findByRole('heading', { name: 'This link no longer works' })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Open your profile' })).toBeInTheDocument();
  });
});
