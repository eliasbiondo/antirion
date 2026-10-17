import { describe, expect, it } from 'vitest';

import type { AuditEntry, Role } from '@/lib/api/types';

import {
  activityLabel,
  activitySubject,
  coverage,
  describeAgent,
  effectivePermissions,
  groupPermissions,
  isInactive,
  offboardedSince,
  permissionCatalogue,
} from '../facts';
import type { UserView } from '../types';
import { cascadeOf, cascadeText } from '../components/tenancy-detail';
import { humanDuration, providersOf } from '../components/access-panel';

const DAY = 86_400_000;
const NOW = Date.parse('2026-10-15T12:00:00Z');

function view(over: Partial<UserView> & { created?: string } = {}): UserView {
  const { created = '2026-01-01T00:00:00Z', ...rest } = over;
  return {
    user: {
      name: rest.name ?? 'ada',
      spec: {},
      metadata: {
        etag: '"1"',
        generation: 1,
        created_at: created,
        updated_at: created,
        managed_by: 'api',
      },
    },
    name: 'ada',
    displayName: 'Ada',
    email: 'ada@example.com',
    kind: 'human',
    state: 'active',
    primaryTeam: null,
    bindings: [],
    keys: 0,
    invitation: null,
    lastActive: null,
    flagged: false,
    signIn: null,
    mfa: [],
    ...rest,
  };
}

const roles: Role[] = [
  {
    name: 'gateway_viewer',
    builtin: true,
    bindable_at: ['gateway'],
    permissions: ['keys:read', 'teams:read', 'logs:read'],
  },
  {
    name: 'team_admin',
    builtin: true,
    bindable_at: ['team'],
    permissions: ['keys:read', 'keys:create', 'keys:rotate', 'projects:update'],
  },
  {
    name: 'finance_viewer',
    builtin: false,
    bindable_at: ['organization'],
    permissions: ['analytics:read'],
  },
];

describe('directory facts', () => {
  it('calls an enabled account inactive after 60 days without activity, counting from creation when never active', () => {
    expect(isInactive(view({ lastActive: new Date(NOW - 61 * DAY).toISOString() }), NOW)).toBe(true);
    expect(isInactive(view({ lastActive: new Date(NOW - 10 * DAY).toISOString() }), NOW)).toBe(false);
    expect(isInactive(view({ created: new Date(NOW - 5 * DAY).toISOString() }), NOW)).toBe(false);
    expect(isInactive(view({ created: new Date(NOW - 90 * DAY).toISOString() }), NOW)).toBe(true);
    expect(isInactive(view({ state: 'disabled', created: '2025-01-01T00:00:00Z' }), NOW)).toBe(false);
  });

  it('measures SSO and two-step coverage over active people only', () => {
    const c = coverage([
      view({ name: 'a', signIn: 'oidc', mfa: ['idp'] }),
      view({ name: 'b', signIn: 'local', mfa: ['totp', 'webauthn'] }),
      view({ name: 'c', signIn: 'token' }),
      view({ name: 'svc', kind: 'service' }),
      view({ name: 'off', state: 'disabled', signIn: 'oidc' }),
    ]);
    expect(c).toEqual({ people: 3, sso: 1, mfa: 2, kinds: { totp: 1, webauthn: 1, idp: 1, none: 1 } });
  });

  it('lists effective permissions per scope, leaving out at lower scopes what the gateway already grants', () => {
    const scopes = effectivePermissions(
      [
        { role: 'team_admin', scope: 'team/platform' },
        { role: 'gateway_viewer', scope: 'gateway' },
        { role: 'gone', scope: 'team/platform' },
      ],
      roles,
    );
    expect(scopes.map((s) => s.scope)).toEqual(['gateway', 'team/platform']);
    const team = scopes[1];
    expect(team?.unknownRoles).toEqual(['gone']);
    expect(team?.groups.map((g) => [g.label, g.permissions])).toEqual([
      ['Projects', ['projects:update']],
      ['API keys', ['keys:create', 'keys:rotate']],
    ]);
  });

  it('groups a catalogue of the built-in roles by area, in the order of actions', () => {
    expect(permissionCatalogue(roles)).not.toContain('analytics:read');
    expect(groupPermissions(['keys:rotate', 'keys:read', 'keys:create'])[0]?.permissions).toEqual([
      'keys:read',
      'keys:create',
      'keys:rotate',
    ]);
  });

  it('names a session by browser and system', () => {
    expect(
      describeAgent(
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15',
      ),
    ).toBe('Safari on macOS');
    expect(describeAgent('Mozilla/5.0 (Windows NT 10.0) AppleWebKit/537.36 Chrome/130.0 Safari/537.36')).toBe(
      'Chrome on Windows',
    );
    expect(describeAgent(undefined)).toBe('Unknown browser');
    expect(describeAgent('curl/8.4')).toBe('curl/8.4');
  });

  it('words the activity feed and finds who or what each entry is about', () => {
    const entry = (action: string, resource: string, extra: Partial<AuditEntry> = {}): AuditEntry =>
      ({
        id: action + resource,
        time: new Date(NOW - DAY).toISOString(),
        actor: { type: 'token', id: 'bootstrap' },
        action,
        resource,
        outcome: 'success',
        status_code: 200,
        ...extra,
      }) as AuditEntry;
    expect(activityLabel(entry('users.offboard', 'users/dorothy'))).toBe('Offboarded');
    expect(activitySubject(entry('invitations.create', 'users/joan@example.com'))).toEqual({
      kind: 'user',
      name: 'joan@example.com',
    });
    expect(activitySubject(entry('keys.anomaly_flagged', 'keys/notebook')).kind).toBe('key');
    expect(
      activitySubject(
        entry('role_bindings.create', 'role_bindings/rb-1', {
          changes: [{ op: 'create', path: 'x', before: null, after: { principal: { id: 'grace' } } }],
        }),
      ),
    ).toEqual({ kind: 'user', name: 'grace' });
    expect(
      offboardedSince(
        [
          entry('users.offboard', 'users/dorothy'),
          entry('users.delete', 'users/dorothy'),
          entry('users.delete', 'users/old', { time: new Date(NOW - 40 * DAY).toISOString() }),
          entry('users.delete', 'users/failed', { outcome: 'failed' }),
        ],
        NOW,
      ),
    ).toBe(1);
  });

  it('finds what a disable stops and a delete removes below an organization or project', () => {
    const meta = { etag: '"1"', generation: 1, created_at: '', updated_at: '', managed_by: 'api' as const };
    const teams = [
      { name: 't1', spec: { organization: 'acme' }, metadata: meta },
      { name: 't2', spec: { organization: 'other' }, metadata: meta },
    ];
    const projects = [
      { name: 'p1', spec: { team: 't1' }, metadata: meta },
      { name: 'p2', spec: { team: 't2' }, metadata: meta },
    ];
    const keys = [
      { name: 'k1', spec: { project: 'p1' }, metadata: meta },
      { name: 'k2', spec: { project: 'p2' }, metadata: meta },
    ];
    const c = cascadeOf('organization', 'acme', teams, projects, keys);
    expect(c).toEqual({ teams: ['t1'], projects: ['p1'], keys: ['k1'] });
    expect(cascadeText(c)).toBe('1 team, 1 project and 1 key');
    expect(cascadeOf('project', 'p2', teams as never, projects as never, keys as never)).toEqual({
      teams: [],
      projects: [],
      keys: ['k2'],
    });
    expect(cascadeText({ teams: [], projects: [], keys: [] })).toBeNull();
  });

  it('describes the ways to sign in and their durations in words', () => {
    expect(humanDuration('12h')).toBe('12 hours');
    expect(humanDuration('90m')).toBe('1.5 hours');
    expect(humanDuration('14d')).toBe('14 days');
    const providers = providersOf({
      admin: {
        sso: {
          oidc: { okta: { issuer: 'https://acme.okta.example', client_id: 'c', display_name: 'Okta' } },
          oauth: { gh: { type: 'github', client_id: 'c', client_secret: 's' } },
        },
        local: { enabled: true, mfa: { factors: ['totp'] } },
      },
    });
    expect(providers.map((p) => [p.name, p.kind])).toEqual([
      ['Okta', 'OpenID Connect'],
      ['gh', 'GitHub'],
      ['Email and password', 'Password'],
      ['Admin tokens', 'Token'],
    ]);
    expect(providers[0]?.facts[0]).toBe('acme.okta.example');
  });
});
