import { api, listAll } from '@/lib/api/client';
import type {
  AdminToken,
  AdminTokenCreated,
  Anomaly,
  AuditEntry,
  BudgetCounter,
  Invitation,
  KeyResource,
  OrganizationResource,
  ProjectResource,
  Role,
  RoleBinding,
  ScimStatus,
  Session,
  TeamResource,
  TeamSpec,
  UserResource,
  UserSpec,
} from '@/lib/api/types';
import type { components } from '@/lib/api/schema';
import type { CollectionApi } from '@/lib/collection-store';

import { ACTIVITY_ACTIONS } from './facts';

type OrganizationSpec = OrganizationResource['spec'];
type ProjectSpec = ProjectResource['spec'];
export type RoleBindingCreate = components['schemas']['RoleBindingCreate'];
export type InvitationCreate = components['schemas']['InvitationCreate'];
export type OffboardRequest = components['schemas']['OffboardRequest'];
export type AuthSettings = components['schemas']['AuthSettings'];
type AdminAuth = NonNullable<components['schemas']['Auth']['admin']>;
export type CustomRoleSpec = NonNullable<AdminAuth['roles']>[string];

const DAY = 86_400_000;
const iso = (t: number) => new Date(t).toISOString().replace(/\.\d{3}Z$/, 'Z');

/** A user's requests and spend: per day over 30 days, the last 24 hours, and their team's 30 days. */
export interface UserUsage {
  days: { start: string; requests: number; cost: number }[];
  requests24h: number;
  cost24h: number;
  requests30d: number;
  cost30d: number;
  /** The primary team's spend over the same 30 days; null without a primary team. */
  teamCost30d: number | null;
}

export const usersCollection: CollectionApi<UserResource, { name: string; spec: UserSpec }, UserSpec> = {
  list: () => listAll((cursor) => api.get('/users', { query: { limit: 200, cursor } })),
  create: (body) => api.post('/users', { body }),
  update: (name, spec, etag) => api.put('/users/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  remove: async (name, { reason, etag }) => {
    await api.delete('/users/{name}', { path: { name }, reason, ifMatch: etag });
  },
};

export const teamsCollection: CollectionApi<TeamResource, { name: string; spec: TeamSpec }, TeamSpec> = {
  list: () => listAll((cursor) => api.get('/teams', { query: { limit: 200, cursor } })),
  create: (body) => api.post('/teams', { body }),
  update: (name, spec, etag) => api.put('/teams/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  remove: async (name, { reason, etag }) => {
    await api.delete('/teams/{name}', { path: { name }, reason, ifMatch: etag });
  },
};

export const projectsCollection: CollectionApi<
  ProjectResource,
  { name: string; spec: ProjectSpec },
  ProjectSpec
> = {
  list: () => listAll((cursor) => api.get('/projects', { query: { limit: 200, cursor } })),
  create: (body) => api.post('/projects', { body }),
  update: (name, spec, etag) =>
    api.put('/projects/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  remove: async (name, { reason, etag }) => {
    await api.delete('/projects/{name}', { path: { name }, reason, ifMatch: etag });
  },
};

export const organizationsCollection: CollectionApi<
  OrganizationResource,
  { name: string; spec: OrganizationSpec },
  OrganizationSpec
> = {
  list: () => listAll((cursor) => api.get('/organizations', { query: { limit: 200, cursor } })),
  create: (body) => api.post('/organizations', { body }),
  update: (name, spec, etag) =>
    api.put('/organizations/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  remove: async (name, { reason, etag }) => {
    await api.delete('/organizations/{name}', { path: { name }, reason, ifMatch: etag });
  },
};

export const directoryApi = {
  bindings: (): Promise<RoleBinding[]> =>
    listAll((cursor) => api.get('/role_bindings', { query: { limit: 200, cursor } })),
  roles: async (): Promise<Role[]> => (await api.get('/roles')).items,
  createBinding: (body: RoleBindingCreate): Promise<RoleBinding> => api.post('/role_bindings', { body }),
  deleteBinding: (id: string, reason?: string) => api.delete('/role_bindings/{id}', { path: { id }, reason }),
  invitations: async (): Promise<Invitation[]> => (await api.get('/invitations')).items,
  invite: (body: InvitationCreate) => api.post('/invitations', { body }),
  cancelInvitation: (id: string) =>
    api.delete('/invitations/{invitation_id}', { path: { invitation_id: id } }),
  resendInvitation: (id: string) =>
    api.post('/invitations/{invitation_id}:resend', { path: { invitation_id: id } }),
  keys: (): Promise<KeyResource[]> =>
    listAll((cursor) => api.get('/keys', { query: { limit: 200, cursor } })),
  tokens: async (user: string): Promise<AdminToken[]> =>
    (await api.get('/users/{name}/tokens', { path: { name: user } })).items,
  createToken: (
    user: string,
    body: { description?: string; expires_at?: string },
  ): Promise<AdminTokenCreated> => api.post('/users/{name}/tokens', { path: { name: user }, body }),
  deleteToken: (user: string, kid: string) =>
    api.delete('/users/{name}/tokens/{kid}', { path: { name: user, kid } }),
  audit: async (user: string): Promise<AuditEntry[]> => {
    const [about, by] = await Promise.all([
      api.get('/audit', { query: { resource: `users/${user}`, limit: 30 } }),
      api.get('/audit', { query: { actor: user, limit: 30 } }),
    ]);
    const seen = new Set<string>();
    return [...about.items, ...by.items]
      .filter((e) => (seen.has(e.id) ? false : (seen.add(e.id), true)))
      .sort((a, b) => b.time.localeCompare(a.time));
  },
  setOrganizationEnabled: (
    name: string,
    enabled: boolean,
    etag: string | undefined,
    reason?: string,
  ): Promise<OrganizationResource> =>
    api.patch('/organizations/{name}', {
      path: { name },
      body: { spec: { enabled } },
      ifMatch: etag,
      reason,
    }),
  setProjectEnabled: (
    name: string,
    enabled: boolean,
    etag: string | undefined,
    reason?: string,
  ): Promise<ProjectResource> =>
    api.patch('/projects/{name}', { path: { name }, body: { spec: { enabled } }, ifMatch: etag, reason }),
  teamSpend: (): Promise<BudgetCounter[]> =>
    listAll((cursor) => api.get('/runtime/budgets', { query: { level: 'team', limit: 200, cursor } })),
  revokeSessions: (user: string) => api.delete('/users/{name}/sessions', { path: { name: user } }),
  resetMfa: (user: string, reason?: string) =>
    api.post('/users/{name}:reset_mfa', { path: { name: user }, reason }),
  offboard: (user: string, body: OffboardRequest) =>
    api.post('/users/{name}:offboard', { path: { name: user }, body }),
  rotateKey: (key: string, grace: string) =>
    api.post('/keys/{name}:rotate', { path: { name: key }, body: { grace_period: grace } }),
  sessions: (user: string): Promise<Session[]> =>
    listAll((cursor) =>
      api.get('/users/{name}/sessions', { path: { name: user }, query: { limit: 200, cursor } }),
    ),
  /** The open unusual-activity flags of the keys a user owns. */
  anomalies: async (user: string): Promise<Anomaly[]> =>
    (await api.get('/anomalies', { query: { user, state: 'open', limit: 100 } })).items,
  dismissAnomaly: (id: string) => api.post('/anomalies/{anomaly_id}:dismiss', { path: { anomaly_id: id } }),
  async usage(user: string, team: string | null, now: number): Promise<UserUsage> {
    const to = iso(now);
    const from30 = iso(now - 30 * DAY);
    const [days, day, teamTotal] = await Promise.all([
      api.get('/analytics/usage', { query: { from: from30, to, interval: '1d', user: [user] } }),
      api.get('/analytics/usage', { query: { from: iso(now - DAY), to, user: [user] } }),
      team
        ? api.get('/analytics/usage', { query: { from: from30, to, team: [team] } })
        : Promise.resolve(null),
    ]);
    const sum = (items: { requests: number; cost_usd: string }[]) =>
      items.reduce((a, b) => ({ requests: a.requests + b.requests, cost: a.cost + Number(b.cost_usd) }), {
        requests: 0,
        cost: 0,
      });
    const month = sum(days.items);
    const last = sum(day.items);
    return {
      days: days.items.map((b) => ({ start: b.start, requests: b.requests, cost: Number(b.cost_usd) })),
      requests24h: last.requests,
      cost24h: last.cost,
      requests30d: month.requests,
      cost30d: month.cost,
      teamCost30d: teamTotal ? sum(teamTotal.items).cost : null,
    };
  },
  authSettings: (): Promise<AuthSettings> => api.get('/settings/auth'),
  scimStatus: (): Promise<ScimStatus> => api.get('/auth/scim/status'),
  /** Writes custom roles as a merge patch of the sign-in settings; `null` removes a role. */
  setCustomRoles: (
    roles: Record<string, CustomRoleSpec | null>,
    etag: string | undefined,
    reason?: string,
  ): Promise<AuthSettings> =>
    api.patch('/settings/auth', {
      body: { spec: { admin: { roles } } },
      ifMatch: etag,
      reason,
    }),
  /** The scope's identity events, newest first: people, invitations, roles, rotations and flags. */
  async activity(): Promise<AuditEntry[]> {
    const reads = await Promise.all(
      ACTIVITY_ACTIONS.map((action) => api.get('/audit', { query: { action, kind: 'write', limit: 100 } })),
    );
    const seen = new Set<string>();
    return reads
      .flatMap((r) => r.items)
      .filter((e) => (seen.has(e.id) ? false : (seen.add(e.id), true)))
      .sort((a, b) => b.time.localeCompare(a.time))
      .slice(0, 200);
  },
  /** Offboardings and deletions of users over the last 30 days. */
  async offboardings(now: number): Promise<AuditEntry[]> {
    const from = iso(now - 30 * DAY);
    const reads = await Promise.all(
      ['users.offboard', 'users.delete'].map((action) =>
        api.get('/audit', { query: { action, from, limit: 200 } }),
      ),
    );
    return reads.flatMap((r) => r.items);
  },
  /** Deletes an organization or project; with `cascade`, everything below it goes too. */
  deleteLevel: async (
    kind: 'organization' | 'project',
    name: string,
    cascade: boolean,
    etag: string | undefined,
    reason?: string,
  ): Promise<void> => {
    const query = cascade ? { cascade: true } : {};
    if (kind === 'organization')
      await api.delete('/organizations/{name}', { path: { name }, query, reason, ifMatch: etag });
    else await api.delete('/projects/{name}', { path: { name }, query, reason, ifMatch: etag });
  },
};
