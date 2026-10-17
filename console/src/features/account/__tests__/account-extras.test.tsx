import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import type { Me, Session } from '@/lib/api/types';
import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { usePreferences } from '@/stores/preferences';
import { useSession } from '@/stores/session';
import { useUi } from '@/stores/ui';
import { pickOption } from '@/test/utils';

import { AccountDialog } from '../components/account-dialog';
import { describeLocation } from '../describe';
import { resolveLocale } from '../i18n';
import { PhotoError, shrinkPhoto } from '../photo';
import { useAccount } from '../store';

const ADA: Me = {
  principal: { type: 'user', id: 'ada', authenticated_by: 'session' },
  bindings: [],
  permissions: { gateway: ['keys:read'] },
  session: null,
  user: { name: 'ada', display_name: 'Ada Lovelace', email: 'ada@example.com', mfa_methods: ['totp'] },
};

function open(section: string, me: Me = ADA) {
  useSession.setState({ status: 'authenticated', me });
  useAccount.setState({ sessions: emptySlot(), tokens: emptySlot(), factors: emptySlot() });
  window.history.replaceState(null, '', `/?account=${section}`);
  return render(<AccountDialog />);
}

const SESSION: Session = {
  id: 's1',
  created_at: '2026-10-01T10:00:00Z',
  last_seen_at: '2026-10-15T10:00:00Z',
  expires_at: '2026-10-29T10:00:00Z',
  client_ip: '203.0.113.7',
  user_agent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 Chrome/130.0 Safari/537.36',
  method: 'local',
  mfa: true,
  trusted: true,
  organization: null,
  current: false,
  location: { country: 'BR', region: 'SP', city: 'Sao Paulo' },
};

describe('account extras', () => {
  it('words a session location', () => {
    expect(describeLocation({ country: 'BR', region: 'SP', city: 'Sao Paulo' })).toBe('Sao Paulo, Brazil');
    expect(describeLocation({ country: null, region: null, city: null })).toBeNull();
    expect(describeLocation(null)).toBeNull();
  });

  it('resolves the language from the preference, then the browser, else English', () => {
    expect(resolveLocale('en-GB').tag).toBe('en');
    expect(resolveLocale(null, ['pt-BR', 'en-US']).tag).toBe('en');
    expect(resolveLocale('xx').tag).toBe('en');
  });

  it('refuses a file that is not an image before reading it', async () => {
    await expect(shrinkPhoto(new File(['x'], 'notes.txt', { type: 'text/plain' }))).rejects.toBeInstanceOf(
      PhotoError,
    );
  });

  it('a password account changes its email through a mailed link', async () => {
    let body: unknown = null;
    server.use(
      http.patch('*/admin/v1/auth/me', async ({ request }) => {
        body = await request.json();
        return HttpResponse.json(ADA);
      }),
    );
    open('profile');
    const dialog = await screen.findByRole('dialog', { name: 'Account' });
    expect(within(dialog).getByLabelText('Email')).toHaveValue('ada@example.com');
    const user = userEvent.setup();
    await user.click(within(dialog).getByRole('button', { name: 'Change email' }));
    await user.type(within(dialog).getByLabelText('New email'), 'ada@new.example');
    await user.click(within(dialog).getByRole('button', { name: 'Change email' }));
    expect(await within(dialog).findByRole('status')).toHaveTextContent('We sent a link to ada@new.example');
    expect(body).toEqual({ email: 'ada@new.example' });
  });

  it('shows the photo and removes it', async () => {
    usePreferences.setState({ photo: 'data:image/webp;base64,AAAA' });
    open('profile');
    const dialog = await screen.findByRole('dialog', { name: 'Account' });
    expect(within(dialog).getByRole('button', { name: 'Change photo' })).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));
    expect(usePreferences.getState().photo).toBeNull();
    expect(within(dialog).getByRole('button', { name: 'Upload photo' })).toBeInTheDocument();
  });

  it('lists trusted browsers with where they are, and forgets one', async () => {
    let revoked = '';
    let sessions: Session[] = [
      SESSION,
      { ...SESSION, id: 's2', trusted: false, current: true, location: null },
    ];
    server.use(
      http.get('*/admin/v1/auth/sessions', () => HttpResponse.json({ items: sessions, next_cursor: null })),
      http.delete('*/admin/v1/auth/sessions/:id', ({ params }) => {
        revoked = String(params.id);
        sessions = sessions.filter((s) => s.id !== revoked);
        return new HttpResponse(null, { status: 204 });
      }),
      http.get('*/admin/v1/auth/me/mfa', () => HttpResponse.json({ items: [], recovery_codes_remaining: 8 })),
    );
    open('security');
    const block = await screen.findByRole('region', { name: 'Trusted browsers' });
    expect(await within(block).findByText('Chrome on macOS')).toBeInTheDocument();
    expect(block).toHaveTextContent('Sao Paulo, Brazil');
    const table = screen.getByRole('table', { name: 'Your sessions' });
    expect(within(table).getByRole('columnheader', { name: 'Location' })).toBeInTheDocument();
    await userEvent.click(within(block).getByRole('button', { name: 'Forget' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Forget browser' }));
    await waitFor(() => expect(revoked).toBe('s1'));
    expect(await within(block).findByText(/No trusted browsers/)).toBeInTheDocument();
  });

  it('offers a security key next to the authenticator app when the browser has WebAuthn', async () => {
    const w = window as unknown as { PublicKeyCredential?: unknown };
    w.PublicKeyCredential = function PublicKeyCredential() {
      return undefined;
    };
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: { get: () => Promise.resolve(null), create: () => Promise.resolve(null) },
    });
    try {
      server.use(
        http.get('*/admin/v1/auth/sessions', () => HttpResponse.json({ items: [], next_cursor: null })),
        http.get('*/admin/v1/auth/me/mfa', () =>
          HttpResponse.json({ items: [], recovery_codes_remaining: 8 }),
        ),
      );
      open('security');
      await userEvent.click(await screen.findByRole('button', { name: 'Add security key' }));
      expect(screen.getByRole('button', { name: 'Register security key' })).toBeInTheDocument();
    } finally {
      delete w.PublicKeyCredential;
      Reflect.deleteProperty(navigator, 'credentials');
    }
  });

  it('keeps the language as a preference and reads the dialog strings from the catalog', async () => {
    usePreferences.setState({ locale: null });
    open('preferences');
    const dialog = await screen.findByRole('dialog', { name: 'Account' });
    expect(within(dialog).getByRole('navigation', { name: 'Account sections' })).toHaveTextContent(
      'Personal tokens',
    );
    await pickOption(within(dialog).getByLabelText('Language'), 'English');
    expect(usePreferences.getState().locale).toBe('en');
  });
});

describe('personal notifications', () => {
  it('chooses email and push by severity and category, lists the browsers and sends a test', async () => {
    const puts: unknown[] = [];
    server.use(
      http.put('*/admin/v1/auth/me/preferences', async ({ request }) => {
        const body = (await request.json()) as Record<string, unknown>;
        puts.push(body);
        return HttpResponse.json(body);
      }),
    );
    useSession.setState({
      status: 'authenticated',
      me: {
        principal: { type: 'user', id: 'ada', authenticated_by: 'session' },
        bindings: [],
        permissions: { gateway: ['alerts:read'] },
        session: null,
        user: { name: 'ada', email: 'ada@example.com' },
      },
    });
    window.history.replaceState(null, '', '/?account=notifications');
    render(<AccountDialog />);
    const table = await screen.findByRole('table', { name: /Tell me about each alert/ });
    expect(within(table).getByRole('columnheader', { name: 'Push' })).toBeInTheDocument();
    await userEvent.click(within(table).getByRole('checkbox', { name: 'Email: critical alerts' }));
    await userEvent.click(within(table).getByRole('checkbox', { name: 'Push: critical alerts' }));
    await userEvent.click(within(table).getByRole('checkbox', { name: 'Push: budgets alerts' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save notification settings' }));
    await waitFor(() => expect(puts).toHaveLength(1));
    const saved = (puts[0] as { notifications: Record<string, unknown> }).notifications;
    expect(saved.email).toEqual({ severities: ['critical'], categories: [] });
    expect(saved.push).toEqual({
      severities: ['critical'],
      categories: ['availability', 'latency', 'safety', 'capacity', 'cache', 'anomaly'],
    });
    const browsers = await screen.findByRole('list', { name: 'Browsers that get push notifications' });
    expect(browsers).toHaveTextContent('Chrome on macOS');
    expect(browsers).toHaveTextContent('fcm.googleapis.com');
    expect(
      screen.getByText('This browser cannot receive notifications from the console.'),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Send a test' }));
    await waitFor(() =>
      expect(useUi.getState().toasts.map((t) => t.title)).toContain('Test notification sent'),
    );
  });
});
