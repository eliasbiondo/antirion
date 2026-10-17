import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import type { Me } from '@/lib/api/types';
import { server } from '@/mocks/node';

import { DEFAULT_PREFERENCES, MAX_FAVOURITES, PREFERENCES_KEY, usePreferences } from '../preferences';
import { MAX_RECENT_ITEMS, useRecents } from '../recents';
import { decodeScope, encodeScope, scopeFilters } from '../scope';
import { hasPermission, highestRole, useSession } from '../session';
import { useTimeRange } from '../time-range';

const ME: Me = {
  principal: { type: 'bootstrap', id: 'bootstrap', authenticated_by: 'session' },
  bindings: [
    { id: 'rb', principal: { type: 'user', id: 'bootstrap' }, role: 'gateway_admin', scope: 'gateway' },
  ],
  permissions: { gateway: ['keys:read'] },
  session: null,
};

describe('session', () => {
  it('signs in with a token and keeps it nowhere', async () => {
    let sent: unknown = null;
    server.use(
      http.post('*/admin/v1/auth/session', async ({ request }) => {
        sent = await request.json();
        return HttpResponse.json(ME);
      }),
    );
    await useSession.getState().signInWithToken('ak-antirion-secret-token');
    expect(sent).toEqual({ token: 'ak-antirion-secret-token' });
    expect(useSession.getState().status).toBe('authenticated');
    expect(JSON.stringify(localStorage)).not.toContain('ak-antirion-secret-token');
    expect(JSON.stringify(useSession.getState())).not.toContain('ak-antirion-secret-token');
  });

  it('marks the session expired, not anonymous, when a call answers 401', () => {
    useSession.setState({ status: 'authenticated', me: null });
    useSession.getState().markExpired();
    expect(useSession.getState().status).toBe('expired');
  });

  it('checks permissions by scope', () => {
    const me = {
      ...ME,
      permissions: { 'organization/acme': ['keys:create'], 'team/core': ['keys:rotate'] },
    } as typeof ME;
    expect(hasPermission(me, 'keys:create', 'acme')).toBe(true);
    expect(hasPermission(me, 'keys:create', 'other')).toBe(false);
    expect(hasPermission(me, 'keys:rotate', null)).toBe(true);
    expect(hasPermission(null, 'keys:read', null)).toBe(false);
    expect(highestRole(ME)).toBe('gateway_admin');
  });
});

describe('preferences', () => {
  it('persists display preferences only', () => {
    usePreferences.getState().set({ theme: 'dark', density: 'comfortable' });
    const stored = JSON.parse(localStorage.getItem(PREFERENCES_KEY) ?? '{}') as {
      state: Record<string, unknown>;
    };
    expect(stored.state.theme).toBe('dark');
    expect(Object.keys(stored.state).sort()).toEqual(Object.keys(DEFAULT_PREFERENCES).sort());
  });
  it('is dark by default, and a stored old default of system becomes dark', async () => {
    expect(DEFAULT_PREFERENCES.theme).toBe('dark');
    localStorage.setItem(PREFERENCES_KEY, JSON.stringify({ state: { theme: 'system' }, version: 0 }));
    await usePreferences.persist.rehydrate();
    expect(usePreferences.getState().theme).toBe('dark');
    localStorage.setItem(PREFERENCES_KEY, JSON.stringify({ state: { theme: 'system' }, version: 1 }));
    await usePreferences.persist.rehydrate();
    expect(usePreferences.getState().theme).toBe('system');
  });
  it('toggles the theme and the sidebar', () => {
    usePreferences.getState().set({ theme: 'light' });
    usePreferences.getState().toggleTheme();
    expect(usePreferences.getState().theme).toBe('dark');
    const before = usePreferences.getState().sidebarCollapsed;
    usePreferences.getState().toggleSidebar();
    expect(usePreferences.getState().sidebarCollapsed).toBe(!before);
  });
});

describe('favourites and recents', () => {
  it('stars and unstars items, newest first, within the cap', () => {
    usePreferences.getState().set({ favourites: [] });
    const toggle = usePreferences.getState().toggleFavourite;
    toggle({ kind: 'key', name: 'ci' });
    toggle({ kind: 'model', name: 'gpt' });
    expect(usePreferences.getState().favourites).toEqual([
      { kind: 'model', name: 'gpt' },
      { kind: 'key', name: 'ci' },
    ]);
    toggle({ kind: 'key', name: 'ci' });
    expect(usePreferences.getState().favourites).toEqual([{ kind: 'model', name: 'gpt' }]);
    for (let i = 0; i < MAX_FAVOURITES + 5; i += 1) toggle({ kind: 'route', name: `r${i}` });
    expect(usePreferences.getState().favourites).toHaveLength(MAX_FAVOURITES);
  });

  it('reads them from the account and writes back the ui members it does not know', async () => {
    const put: { body: unknown }[] = [];
    server.use(
      http.get('*/admin/v1/auth/me/preferences', () =>
        HttpResponse.json({
          ui: {
            favourites: [
              { kind: 'team', name: 'core' },
              { kind: 'nope', name: 'x' },
            ],
            banner_seen: 'x',
            price_unit: '1K',
          },
        }),
      ),
      http.put('*/admin/v1/auth/me/preferences', async ({ request }) => {
        put.push({ body: await request.json() });
        return HttpResponse.json({});
      }),
    );
    useSession.setState({
      status: 'authenticated',
      me: { ...ME, principal: { type: 'user', id: 'ada', authenticated_by: 'session' } },
    });
    await usePreferences.getState().pull();
    expect(usePreferences.getState().favourites).toEqual([{ kind: 'team', name: 'core' }]);
    expect(usePreferences.getState().priceUnit).toBe('1K');
    usePreferences.getState().toggleFavourite({ kind: 'user', name: 'ada' });
    await new Promise((r) => setTimeout(r, 900));
    const ui = (put.at(-1)?.body as { ui: Record<string, unknown> }).ui;
    expect(ui.banner_seen).toBe('x');
    expect(ui.favourites).toEqual([
      { kind: 'user', name: 'ada' },
      { kind: 'team', name: 'core' },
    ]);
    useSession.setState({ status: 'unknown', me: null });
  });

  it('remembers recent items and searches in this browser, newest first', () => {
    useRecents.getState().clear();
    for (let i = 0; i < MAX_RECENT_ITEMS + 2; i += 1)
      useRecents.getState().visit({ kind: 'key', name: `k${i}` });
    useRecents.getState().visit({ kind: 'key', name: 'k3' });
    expect(useRecents.getState().items[0]).toEqual({ kind: 'key', name: 'k3' });
    expect(useRecents.getState().items).toHaveLength(MAX_RECENT_ITEMS);
    useRecents.getState().searched(' slow ');
    useRecents.getState().searched('errors');
    useRecents.getState().searched('slow');
    expect(useRecents.getState().searches).toEqual(['slow', 'errors']);
    expect(localStorage.getItem('antirion.console.recent')).toContain('k3');
  });
});

describe('time range and scope', () => {
  it('resets the bucket to the preset default and refuses too many buckets', () => {
    useTimeRange.getState().setRange({ kind: 'preset', preset: '30d' });
    expect(useTimeRange.getState().interval).toBe('1d');
    useTimeRange.getState().setInterval('1m');
    expect(useTimeRange.getState().interval).toBe('1d');
    useTimeRange.getState().setRange({ kind: 'preset', preset: '6h' });
    expect(useTimeRange.getState().interval).toBe('5m');
  });
  it('moves a preset window to now when another page opens, and keeps a custom one', () => {
    useTimeRange.setState({ range: { kind: 'preset', preset: '6h' }, anchoredAt: 0 });
    useTimeRange.getState().pageOpened();
    expect(useTimeRange.getState().anchoredAt).toBeGreaterThan(0);
    useTimeRange.setState({
      range: { kind: 'custom', from: 1_767_225_600_000, to: 1_767_312_000_000 },
      anchoredAt: 0,
    });
    useTimeRange.getState().pageOpened();
    expect(useTimeRange.getState().anchoredAt).toBe(0);
    useTimeRange.getState().setRange({ kind: 'preset', preset: '6h' });
  });
  it('encodes the scope and its filters', () => {
    expect(encodeScope({ kind: 'organization', name: 'acme' })).toBe('acme');
    expect(decodeScope('gateway')).toEqual({ kind: 'gateway' });
    expect(decodeScope('Not Valid')).toBeNull();
    expect(scopeFilters({ kind: 'organization', name: 'acme' }, 'prod')).toEqual({
      organization: 'acme',
      tag: 'env:prod',
    });
  });
});
