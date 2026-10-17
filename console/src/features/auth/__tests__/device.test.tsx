import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { useSession } from '@/stores/session';

import { CliStep, loginCommand } from '../components/cli-step';
import { DeviceScreen, formatUserCode } from '../components/device-screen';
import { useLogin } from '../store';

const PENDING = {
  user_code: 'BCDF-GHJK',
  status: 'pending',
  requested_at: new Date().toISOString(),
  expires_at: new Date(Date.now() + 600_000).toISOString(),
  client_id: 'antirion-cli',
  client_ip: '10.0.0.7',
  organization: null,
  sso: false,
};

function signedIn() {
  useSession.setState({
    status: 'authenticated',
    expiredAt: null,
    me: {
      principal: { type: 'user', id: 'ada' },
      user: { name: 'ada', display_name: 'Ada Lovelace' },
      bindings: [],
      permissions: {},
      session: null,
    } as never,
  });
}

describe('sign-in from a terminal', () => {
  it('builds the command with the organization', () => {
    expect(loginCommand('https://gw.acme.test', '', false)).toBe(
      'antirion auth login --admin-url https://gw.acme.test',
    );
    expect(loginCommand('https://gw.acme.test', 'acme', true)).toBe(
      'antirion auth login --admin-url https://gw.acme.test --sso --organization acme',
    );
  });

  it('shows the command and where to approve it', () => {
    useLogin.setState({ organization: 'acme', options: null });
    render(<CliStep />);
    expect(screen.getByRole('heading', { name: 'Sign in from a terminal' })).toBeInTheDocument();
    expect(screen.getByText(/antirion auth login --admin-url .* --organization acme/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Copy the command' })).toBeInTheDocument();
    expect(screen.getByText(/\/device$/)).toBeInTheDocument();
  });
});

describe('approving a terminal sign-in', () => {
  it('reads a code into its printed form', () => {
    expect(formatUserCode('bcdf ghjk')).toBe('BCDF-GHJK');
    expect(formatUserCode('bcd')).toBe('BCD');
  });

  it('checks the code, shows the request and approves it', async () => {
    signedIn();
    window.history.replaceState(null, '', '/device/?code=bcdfghjk');
    const seen: string[] = [];
    server.use(
      http.post(/\/admin\/v1\/auth\/device:check$/, async ({ request }) => {
        seen.push(JSON.stringify(await request.json()));
        return HttpResponse.json(PENDING);
      }),
      http.post(/\/admin\/v1\/auth\/device:approve$/, () =>
        HttpResponse.json({ ...PENDING, status: 'approved' }),
      ),
    );
    const user = userEvent.setup();
    render(<DeviceScreen />);
    expect(screen.getByLabelText('Code')).toHaveValue('BCDF-GHJK');
    await user.click(screen.getByRole('button', { name: 'Continue' }));
    expect(await screen.findByRole('heading', { name: 'Approve this sign-in?' })).toBeInTheDocument();
    expect(seen).toEqual([JSON.stringify({ user_code: 'BCDF-GHJK' })]);
    expect(screen.getByText('10.0.0.7')).toBeInTheDocument();
    expect(screen.getByText('Ada Lovelace')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Approve' }));
    expect(await screen.findByRole('heading', { name: 'Sign-in approved' })).toBeInTheDocument();
  });

  it('says when a code is unknown or expired', async () => {
    signedIn();
    window.history.replaceState(null, '', '/device/');
    server.use(
      http.post(/\/admin\/v1\/auth\/device:check$/, () =>
        HttpResponse.json({ error: { code: 'not_found', message: 'no', request_id: 'x' } }, { status: 404 }),
      ),
    );
    const user = userEvent.setup();
    render(<DeviceScreen />);
    await user.type(screen.getByLabelText('Code'), 'BCDFGHJK');
    await user.click(screen.getByRole('button', { name: 'Continue' }));
    expect(await screen.findByRole('alert')).toHaveTextContent("isn't valid or has expired");
  });

  it('asks a reader who is not signed in to sign in first', () => {
    useSession.setState({ status: 'anonymous', me: null, expiredAt: null });
    window.history.replaceState(null, '', '/device/?code=BCDF-GHJK');
    render(<DeviceScreen />);
    const link = screen.getByRole('link', { name: 'Sign in to continue' });
    expect(link.getAttribute('href')).toContain(encodeURIComponent('/device/?code=BCDF-GHJK'));
  });
});
