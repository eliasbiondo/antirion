import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { useShell } from '@/stores/shell';
import { useUi } from '@/stores/ui';
import { signInAs } from '@/test/utils';

import { TeamsUsersScreen } from '../components/teams-users-screen';
import { useDirectory, useOrganizations, useProjects, userActions, useTeams, useUsers } from '../store';

const meta = {
  etag: '"1"',
  generation: 1,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  managed_by: 'api',
};
const list = (items: unknown[]) => HttpResponse.json({ items, next_cursor: null });

function directory(users: unknown[], invitations: unknown[] = []) {
  useUsers.setState({ items: [], status: 'idle' });
  useTeams.setState({ items: [], status: 'idle' });
  useOrganizations.setState({ items: [], status: 'idle' });
  useProjects.setState({ items: [], status: 'idle' });
  useDirectory.setState({ sessions: {}, anomalies: {}, usage: {} });
  useShell.setState({ status: null });
  server.use(
    http.get('*/admin/v1/users', () => list(users)),
    http.get('*/admin/v1/teams', () => list([])),
    http.get('*/admin/v1/projects', () => list([])),
    http.get('*/admin/v1/organizations', () => list([])),
    http.get('*/admin/v1/role_bindings', () => list([])),
    http.get('*/admin/v1/roles', () => list([])),
    http.get('*/admin/v1/invitations', () => list(invitations)),
    http.get('*/admin/v1/keys', () => list([])),
    http.get('*/admin/v1/users/:name/tokens', () => list([])),
    http.get('*/admin/v1/audit', () => list([])),
    http.get('*/admin/v1/runtime/budgets', () => list([])),
    http.get('*/admin/v1/users/:name/sessions', () => list([])),
    http.get('*/admin/v1/anomalies', () => list([])),
    http.get('*/admin/v1/analytics/usage', () => list([])),
  );
}

const DAY = 86_400_000;
const ago = (days: number) => new Date(Date.now() - days * DAY).toISOString().replace(/\.\d{3}Z$/, 'Z');
const person = (
  name: string,
  metadata: Record<string, unknown> = {},
  spec: Record<string, unknown> = {},
) => ({
  name,
  spec: { kind: 'human', email: `${name}@example.com`, ...spec },
  metadata: { ...meta, created_at: ago(1), state: 'active', ...metadata },
});

const ADMIN = [
  'users:read',
  'users:create',
  'users:update',
  'users:delete',
  'role_bindings:read',
  'role_bindings:create',
  'role_bindings:delete',
  'teams:read',
  'teams:create',
  'organizations:read',
  'organizations:update',
  'organizations:delete',
  'projects:read',
  'audit:read',
  'analytics:read',
  'security:read',
  'keys:read',
  'keys:rotate',
  'security:write',
];

describe('Teams & users', () => {
  it('gives a user one primary action and puts the rest in More, destructive last', async () => {
    useUsers.setState({ items: [], status: 'idle' });
    signInAs(ADMIN);
    directory([{ name: 'ada', spec: { kind: 'human', email: 'ada@example.com' }, metadata: meta }]);
    render(<TeamsUsersScreen />);
    await screen.findByRole('listbox', { name: 'People' });
    expect(screen.getByRole('button', { name: 'Edit' }).className).toMatch(/bg-accent/);
    expect(screen.queryByRole('button', { name: 'Reset MFA' })).not.toBeInTheDocument();
    const [, detailMore] = screen.getAllByRole('button', { name: 'More' });
    if (!detailMore) throw new Error('no More menu on the user');
    await userEvent.click(detailMore);
    const items = screen.getAllByRole('menuitem').map((m) => m.textContent);
    expect(items[0]).toBe('Copy link');
    expect(items[items.length - 1]).toBe('Delete');
    expect(items).toContain('Disable');
  });

  it('shows an account made by an unaccepted invitation as invited, not disabled', async () => {
    useUsers.setState({ items: [], status: 'idle' });
    signInAs(ADMIN);
    directory(
      [
        {
          name: 'margaret@example.com',
          spec: { kind: 'human', email: 'margaret@example.com', enabled: false },
          metadata: { ...meta, state: 'invited' },
        },
      ],
      [
        {
          id: 'inv-1',
          user: 'margaret@example.com',
          email: 'margaret@example.com',
          invited_by: 'bootstrap',
          created_at: meta.created_at,
          expires_at: '2026-12-01T00:00:00Z',
          expired: false,
        },
      ],
    );
    render(<TeamsUsersScreen />);
    const people = await screen.findByRole('listbox', { name: 'People' });
    expect(within(people).getByText('Invited')).toBeInTheDocument();
    expect(within(people).queryByText('Disabled')).not.toBeInTheDocument();
  });

  it('keeps an invitation whose link expired in the list, as expired with a resend', async () => {
    useUsers.setState({ items: [], status: 'idle' });
    signInAs(ADMIN);
    let resent = 0;
    directory(
      [
        {
          name: 'grace@example.com',
          spec: { kind: 'human', email: 'grace@example.com', enabled: false },
          metadata: { ...meta, state: 'invited' },
        },
      ],
      [
        {
          id: 'inv-2',
          user: 'grace@example.com',
          email: 'grace@example.com',
          invited_by: 'bootstrap',
          created_at: ago(9),
          expires_at: ago(2),
          expired: true,
        },
      ],
    );
    server.use(
      http.post('*/admin/v1/invitations/:id', () => {
        resent += 1;
        return HttpResponse.json({});
      }),
    );
    render(<TeamsUsersScreen />);
    const people = await screen.findByRole('listbox', { name: 'People' });
    expect(within(people).getAllByText('Invitation expired').length).toBeGreaterThan(0);
    expect(within(people).queryByText('Disabled')).not.toBeInTheDocument();
    expect(within(people).getByText(/link expired/)).toBeInTheDocument();
    expect(screen.getByText('This invitation has expired')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Resend invitation' }));
    await waitFor(() => expect(resent).toBe(1));
  });

  it('counts the people who need review and filters the list to them', async () => {
    signInAs(ADMIN);
    directory([
      person('ada', { last_active_at: ago(1) }),
      person('flo', { flagged: true, last_active_at: ago(1) }),
      person('ian', { created_at: ago(200), last_active_at: ago(70) }),
    ]);
    render(<TeamsUsersScreen />);
    const people = await screen.findByRole('listbox', { name: 'People' });
    const summary = screen.getByRole('region', { name: 'Directory summary' });
    expect(within(summary).getByText('1 flagged · 1 inactive')).toBeInTheDocument();
    await userEvent.click(within(summary).getByText('Need review'));
    expect(within(people).queryByText('ada@example.com', { exact: false })).not.toBeInTheDocument();
    expect(within(people).getByText('Flagged')).toBeInTheDocument();
    expect(within(people).getByText('Inactive')).toBeInTheDocument();
  });

  it('keeps the people filters in the URL and opens with them', async () => {
    signInAs(ADMIN);
    directory([
      person('ada', { last_active_at: ago(1) }),
      person('flo', { flagged: true, last_active_at: ago(1) }),
    ]);
    window.history.replaceState(null, '', '/teams/?state=flagged');
    render(<TeamsUsersScreen />);
    const people = await screen.findByRole('listbox', { name: 'People' });
    expect(within(people).queryByText('ada@example.com', { exact: false })).not.toBeInTheDocument();
    expect(within(people).getByText('flo@example.com', { exact: false })).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText('Search users'), 'f');
    expect(new URLSearchParams(window.location.search).get('q')).toBe('f');
    await userEvent.click(screen.getByRole('button', { name: 'Clear filters' }));
    expect(window.location.search).toBe('');
  });

  it("a team's link shows that team alone, and the teams export as CSV", async () => {
    signInAs(ADMIN);
    directory([person('ada', { last_active_at: ago(1) }, { primary_team: 'core' })]);
    server.use(
      http.get('*/admin/v1/teams', () =>
        list([
          { name: 'core', spec: { organization: 'default', display_name: 'Core' }, metadata: meta },
          { name: 'search', spec: { organization: 'default', display_name: 'Search' }, metadata: meta },
        ]),
      ),
    );
    window.history.replaceState(null, '', '/teams/?tab=teams&team=search');
    render(<TeamsUsersScreen />);
    const grid = await screen.findByRole('list', { name: 'Teams' });
    expect(within(grid).getByRole('heading', { name: 'Search' })).toBeInTheDocument();
    expect(within(grid).queryByRole('heading', { name: 'Core' })).not.toBeInTheDocument();
    expect(within(grid).getByRole('button', { name: 'Copy link to team Search' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Export CSV' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Only Search/ }));
    expect(within(grid).getByRole('heading', { name: 'Core' })).toBeInTheDocument();
  });

  it('disables inviting and adding people when every seat is taken', async () => {
    signInAs([...ADMIN, 'users:create']);
    directory([person('ada')]);
    useShell.setState({ status: { users: { active: 1, cap: 1 } } as never });
    render(<TeamsUsersScreen />);
    await screen.findByRole('listbox', { name: 'People' });
    expect(screen.getByText('All 1 seats in use')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Invite people' })).toBeDisabled();
  });

  it("shows a flagged user's open flag and dismisses it", async () => {
    signInAs([...ADMIN, 'keys:update']);
    directory([person('flo', { flagged: true })]);
    const dismissed: string[] = [];
    server.use(
      http.get('*/admin/v1/anomalies', () =>
        list([
          {
            id: 'an-1',
            key: 'notebook',
            owner: 'flo',
            kind: 'network',
            severity: 'warning',
            first_seen: ago(0),
            last_seen: ago(0),
            count: 3,
            detail: { network: '10.2.2.0/24' },
            state: 'open',
          },
        ]),
      ),
      http.post('*/admin/v1/anomalies/:id', ({ params }) => {
        dismissed.push(String(params.id));
        return HttpResponse.json({});
      }),
    );
    render(<TeamsUsersScreen />);
    const banner = await screen.findByText('Flagged: unusual activity on a key they own');
    const box = banner.closest<HTMLElement>('[role="status"]');
    if (!box) throw new Error('no banner');
    expect(await within(box).findByText('10.2.2.0/24')).toBeInTheDocument();
    expect(within(box).getByRole('link', { name: 'Review requests' }).getAttribute('href')).toMatch(
      /request-log\/?\?key=notebook$/,
    );
    await userEvent.click(within(box).getByRole('button', { name: 'Dismiss' }));
    await waitFor(() => expect(useUi.getState().toasts.some((t) => t.title === 'Flag dismissed')).toBe(true));
    expect(dismissed).toEqual(['an-1:dismiss']);
  });

  it("lists a user's sessions and what their roles let them do", async () => {
    signInAs(ADMIN);
    directory([
      person(
        'ada',
        { last_login_method: 'local', mfa_methods: ['totp'] },
        {
          role_bindings: [{ role: 'team_admin', scope: 'team/platform' }],
        },
      ),
    ]);
    server.use(
      http.get('*/admin/v1/roles', () =>
        list([
          {
            name: 'team_admin',
            builtin: true,
            bindable_at: ['team'],
            permissions: ['keys:read', 'keys:rotate'],
          },
        ]),
      ),
      http.get('*/admin/v1/users/:name/sessions', () =>
        list([
          {
            id: 's1',
            created_at: ago(0),
            last_seen_at: ago(0),
            expires_at: ago(-1),
            user_agent: 'Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0',
            client_ip: '203.0.113.7',
            method: 'local',
            mfa: true,
            trusted: false,
            current: false,
          },
        ]),
      ),
    );
    render(<TeamsUsersScreen />);
    const sessions = await screen.findByRole('table', { name: 'Sessions of ada' });
    expect(within(sessions).getByText('Firefox on Linux')).toBeInTheDocument();
    expect(within(sessions).getByText('Two-step')).toBeInTheDocument();
    expect(screen.getByText('Two-step: authenticator app')).toBeInTheDocument();
    expect(
      await screen.findByText((_, el) => el?.tagName === 'DD' && el.textContent === 'see, rotate'),
    ).toBeInTheDocument();
  });

  it('creates a custom role as a patch of the sign-in settings', async () => {
    signInAs([...ADMIN, 'security:write']);
    directory([person('ada')]);
    let patched: unknown = null;
    server.use(
      http.get('*/admin/v1/roles', () =>
        list([
          {
            name: 'viewer',
            builtin: true,
            bindable_at: ['team'],
            permissions: ['analytics:read', 'logs:read'],
          },
        ]),
      ),
      http.get('*/admin/v1/settings/auth', () =>
        HttpResponse.json({ spec: {}, metadata: meta }, { headers: { etag: '"1"' } }),
      ),
      http.patch('*/admin/v1/settings/auth', async ({ request }) => {
        patched = await request.json();
        return HttpResponse.json({ spec: {}, metadata: meta });
      }),
    );
    window.history.replaceState(null, '', '/?tab=roles');
    render(<TeamsUsersScreen />);
    await userEvent.click(await screen.findByRole('button', { name: 'Create role' }));
    const drawer = screen.getByRole('dialog', { name: 'Create role' });
    await userEvent.type(within(drawer).getByLabelText(/^Name/), 'finance_viewer');
    await userEvent.click(within(drawer).getByRole('checkbox', { name: 'Analytics: see' }));
    await userEvent.click(within(drawer).getByRole('button', { name: 'Create role' }));
    await waitFor(() => expect(useUi.getState().toasts.some((t) => t.title === 'Role created')).toBe(true));
    expect(patched).toEqual({
      spec: {
        admin: {
          roles: {
            finance_viewer: {
              permissions: ['analytics:read'],
              bindable_at: ['organization', 'team', 'project'],
            },
          },
        },
      },
    });
    window.history.replaceState(null, '', '/');
  });

  it('deletes an organization with everything in it after saying what goes', async () => {
    signInAs(ADMIN);
    directory([person('ada')]);
    let deleted: string | null = null;
    server.use(
      http.get('*/admin/v1/organizations', () => list([{ name: 'acme', spec: {}, metadata: meta }])),
      http.get('*/admin/v1/teams', () =>
        list([{ name: 'research', spec: { organization: 'acme' }, metadata: meta }]),
      ),
      http.get('*/admin/v1/projects', () =>
        list([{ name: 'papers', spec: { team: 'research' }, metadata: meta }]),
      ),
      http.get('*/admin/v1/keys', () =>
        list([{ name: 'paper-bot', spec: { project: 'papers' }, metadata: meta }]),
      ),
      http.delete('*/admin/v1/organizations/:name', ({ request }) => {
        deleted = request.url;
        return new HttpResponse(null, { status: 204 });
      }),
    );
    window.history.replaceState(null, '', '/?tab=organizations&detail=acme');
    render(<TeamsUsersScreen />);
    const drawer = await screen.findByRole('dialog', { name: 'Organization acme' });
    expect(
      await within(drawer).findByText(/Its 1 team, 1 project and 1 key are deleted with it/),
    ).toBeInTheDocument();
    await userEvent.click(within(drawer).getByRole('button', { name: 'Delete' }));
    const confirm = screen.getByRole('alertdialog', { name: 'Delete organization acme?' });
    expect(
      within(confirm).getByText(/Everything in it is deleted too: 1 team, 1 project and 1 key/),
    ).toBeInTheDocument();
    const [typed] = within(confirm).getAllByRole('textbox');
    if (!typed) throw new Error('no name field');
    await userEvent.type(typed, 'acme');
    await userEvent.click(within(confirm).getByRole('button', { name: 'Delete organization' }));
    await waitFor(() =>
      expect(useUi.getState().toasts.some((t) => t.title === 'Organization deleted')).toBe(true),
    );
    expect(deleted).toContain('cascade=true');
    window.history.replaceState(null, '', '/');
  });

  it('downgrades every role of a user to the viewer role at the same scope', async () => {
    signInAs(ADMIN);
    directory([person('ian')]);
    const calls: string[] = [];
    server.use(
      http.post('*/admin/v1/role_bindings', async ({ request }) => {
        const body = (await request.json()) as { role: string; scope: string };
        calls.push(`+${body.role}@${body.scope}`);
        return HttpResponse.json({ id: `new-${body.role}`, principal: { type: 'user', id: 'ian' }, ...body });
      }),
      http.delete('*/admin/v1/role_bindings/:id', ({ params }) => {
        calls.push(`-${String(params.id)}`);
        return new HttpResponse(null, { status: 204 });
      }),
    );
    await userActions.downgradeToViewer(
      {
        user: null,
        name: 'ian',
        displayName: 'Ian',
        email: '',
        kind: 'human',
        state: 'active',
        primaryTeam: null,
        bindings: [
          { id: 'rb-1', role: 'gateway_admin', scope: 'gateway' },
          { id: 'rb-2', role: 'team_admin', scope: 'team/platform' },
          { id: 'rb-3', role: 'developer', scope: 'team/platform' },
          { id: 'rb-4', role: 'viewer', scope: 'team/growth' },
        ],
        keys: 0,
        invitation: null,
        lastActive: null,
        flagged: false,
        signIn: null,
        mfa: [],
      },
      'inactive',
    );
    expect(calls).toEqual(['+gateway_viewer@gateway', '-rb-1', '+viewer@team/platform', '-rb-2', '-rb-3']);
  });

  it('makes Create team the primary action on the Teams tab, with Invite people under More', async () => {
    signInAs([...ADMIN, 'teams:create', 'invitations:create']);
    directory([person('ada')]);
    server.use(
      http.get('*/admin/v1/teams', () =>
        list([{ name: 'core', spec: { organization: 'default', display_name: 'Core' }, metadata: meta }]),
      ),
    );
    window.history.replaceState(null, '', '/teams/?tab=teams');
    render(<TeamsUsersScreen />);
    await screen.findByRole('list', { name: 'Teams' });
    expect(screen.getByRole('button', { name: 'Create team' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Invite people' })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'More' }));
    expect(screen.getByRole('menuitem', { name: 'Invite people' })).toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: 'Create team' })).not.toBeInTheDocument();
  });

  it("doesn't read sessions for someone invited", async () => {
    signInAs([...ADMIN, 'users:read_sessions']);
    directory([person('ivy', { state: 'invited' })]);
    const asked: string[] = [];
    server.use(
      http.get('*/admin/v1/users/:name/sessions', ({ params }) => {
        asked.push(String(params.name));
        return list([]);
      }),
    );
    window.history.replaceState(null, '', '/teams/?user=ivy');
    render(<TeamsUsersScreen />);
    await screen.findByRole('listbox', { name: 'People' });
    await new Promise((r) => setTimeout(r, 50));
    expect(asked).toEqual([]);
  });
});
