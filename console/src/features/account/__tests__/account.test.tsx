import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import type { Me } from '@/lib/api/types';
import { server } from '@/mocks/node';
import { usePreferences } from '@/stores/preferences';
import { useSession } from '@/stores/session';

import { AccountDialog } from '../components/account-dialog';
import { describeAgent, methodLabel, roleLabel } from '../describe';
import { useAccount } from '../store';
import { emptySlot } from '@/lib/slot';

const BOOTSTRAP: Me = {
  principal: { type: 'bootstrap', id: 'bootstrap', authenticated_by: 'session' },
  bindings: [
    { id: 'rb', principal: { type: 'user', id: 'bootstrap' }, role: 'gateway_admin', scope: 'gateway' },
  ],
  permissions: { gateway: ['keys:read'] },
  session: null,
};

const ADA: Me = {
  principal: { type: 'user', id: 'ada', authenticated_by: 'session' },
  bindings: [],
  permissions: { gateway: ['keys:read'] },
  session: null,
  user: { name: 'ada', display_name: 'Ada Lovelace', email: 'ada@example.com', mfa_methods: ['totp'] },
};

function open(section: string, me: Me) {
  useSession.setState({ status: 'authenticated', me });
  useAccount.setState({ sessions: emptySlot(), tokens: emptySlot(), factors: emptySlot() });
  window.history.replaceState(null, '', `/?account=${section}`);
  return render(<AccountDialog />);
}

/** Fails the test if the console asks for an account-only resource. */
function forbidAccountCalls(): string[] {
  const calls: string[] = [];
  const record = ({ request }: { request: Request }) => {
    calls.push(new URL(request.url).pathname);
    return HttpResponse.json(
      { error: { code: 'forbidden', message: 'no', request_id: 'r' } },
      { status: 403 },
    );
  };
  server.use(
    http.get('*/admin/v1/auth/me/tokens', record),
    http.get('*/admin/v1/auth/me/mfa', record),
    http.get('*/admin/v1/auth/me/preferences', record),
  );
  return calls;
}

describe('account dialog', () => {
  it('signed in with the bootstrap token: explains what applies instead of showing "not permitted"', async () => {
    const calls = forbidAccountCalls();
    open('profile', BOOTSTRAP);
    const dialog = await screen.findByRole('dialog', { name: 'Account' });
    const nav = within(dialog).getByRole('navigation', { name: 'Account sections' });
    expect(
      within(nav)
        .getAllByRole('button')
        .map((b) => b.textContent),
    ).toEqual(['Profile', 'Preferences', 'Security']);
    expect(dialog).toHaveTextContent("You're signed in with an admin token, so there's no profile to edit.");
    expect(dialog).toHaveTextContent('Signed in with the bootstrap token');
    expect(within(dialog).getByRole('link', { name: /Go to Teams & users/ })).toHaveAttribute(
      'href',
      expect.stringMatching(/\/teams\/?$/),
    );
    expect(dialog).not.toHaveTextContent(/not permitted/i);
    expect(calls).toEqual([]);
  });

  it('a link to an account-only section shows why it is not there, without calling the gateway', async () => {
    const calls = forbidAccountCalls();
    open('tokens', BOOTSTRAP);
    const dialog = await screen.findByRole('dialog', { name: 'Account' });
    expect(dialog).toHaveTextContent('Personal tokens belong to a user account.');
    expect(within(dialog).queryByRole('button', { name: 'Create token' })).not.toBeInTheDocument();
    expect(calls).toEqual([]);
  });

  it('keeps an admin token on the session list only, in words', async () => {
    const calls = forbidAccountCalls();
    server.use(
      http.get('*/admin/v1/auth/sessions', () =>
        HttpResponse.json({
          items: [
            {
              id: 's1',
              created_at: '2026-10-14T10:00:00Z',
              last_seen_at: '2026-10-14T10:00:00Z',
              expires_at: '2026-10-14T22:00:00Z',
              client_ip: '127.0.0.1',
              user_agent:
                'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36',
              method: 'token',
              mfa: false,
              trusted: false,
              current: true,
            },
          ],
          next_cursor: null,
        }),
      ),
    );
    open('security', BOOTSTRAP);
    const table = await screen.findByRole('table', { name: 'Your sessions' });
    expect(table).toHaveTextContent('Chrome on macOS');
    expect(table).toHaveTextContent('This browser');
    expect(table).toHaveTextContent('Admin token');
    expect(screen.queryByText('Two-step verification')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Sign out other sessions' })).toBeDisabled();
    expect(calls).toEqual([]);
  });

  it('lets a person change their password with the code the gateway asks for', async () => {
    let body: unknown = null;
    server.use(
      http.get('*/admin/v1/auth/sessions', () => HttpResponse.json({ items: [], next_cursor: null })),
      http.get('*/admin/v1/auth/me/mfa', () =>
        HttpResponse.json({
          items: [{ id: 'f1', type: 'totp', created_at: '2026-10-01T00:00:00Z', last_used_at: null }],
          recovery_codes_remaining: 7,
        }),
      ),
      http.post('*/admin/v1/auth/me:change_password', async ({ request }) => {
        body = await request.json();
        return new HttpResponse(null, { status: 204 });
      }),
    );
    open('security', ADA);
    const dialog = await screen.findByRole('dialog', { name: 'Account' });
    expect(await within(dialog).findByText('7 codes left', { exact: false })).toBeInTheDocument();
    expect(within(dialog).getByRole('button', { name: 'Remove' })).toBeDisabled();
    const user = userEvent.setup();
    await user.type(within(dialog).getByLabelText('Current password'), 'Old-password-2026');
    await user.type(within(dialog).getByLabelText('Verification code'), '123 456');
    await user.type(within(dialog).getByLabelText('New password'), 'New-password-2026');
    await user.type(within(dialog).getByLabelText('Confirm password'), 'New-password-2026');
    await user.click(within(dialog).getByRole('button', { name: 'Change password' }));
    await waitFor(() =>
      expect(body).toEqual({
        current_password: 'Old-password-2026',
        new_password: 'New-password-2026',
        totp: '123456',
      }),
    );
  });

  it('offers preferences to everyone and says where they are kept', async () => {
    open('preferences', BOOTSTRAP);
    const dialog = await screen.findByRole('dialog', { name: 'Account' });
    expect(dialog).toHaveTextContent('Saved in this browser only');
    await userEvent.click(within(dialog).getByRole('radio', { name: 'Green' }));
    expect(usePreferences.getState().accentHue).toBe(150);
  });
});

describe('account wording helpers', () => {
  it('names devices, sign-in methods and roles in words', () => {
    expect(
      describeAgent('Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:131.0) Gecko/20100101 Firefox/131.0'),
    ).toBe('Firefox on Windows');
    expect(
      describeAgent(
        'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1',
      ),
    ).toBe('Safari on iOS');
    expect(describeAgent('curl/8.7.1')).toBe('curl');
    expect(describeAgent(null)).toBe('Unknown device');
    expect(methodLabel('local')).toBe('Password');
    expect(methodLabel('oidc')).toBe('Single sign-on');
    expect(roleLabel('gateway_admin')).toBe('Gateway admin');
  });

  it('does not ask the gateway for preferences when the principal is an admin token', async () => {
    const calls = forbidAccountCalls();
    useSession.setState({ status: 'authenticated', me: BOOTSTRAP });
    await usePreferences.getState().pull();
    expect(calls).toEqual([]);
  });
});
