import { api, listAll } from '@/lib/api/client';
import type {
  Anomaly,
  AuditEntry,
  BudgetCounter,
  KeyCreated,
  KeyResource,
  KeyRotated,
  KeySpec,
  OrganizationResource,
  ProjectResource,
  TeamResource,
  UserResource,
} from '@/lib/api/types';
import type { components } from '@/lib/api/schema';
import type { CollectionApi } from '@/lib/collection-store';

export type LimitCounter = components['schemas']['LimitCounter'];

export type KeyCreateInput = { name: string; spec: KeySpec };

export const keysCollection: CollectionApi<KeyResource, KeyCreateInput, KeySpec> & {
  createWithSecret: (body: KeyCreateInput) => Promise<KeyCreated>;
} = {
  list: () => listAll((cursor) => api.get('/keys', { query: { limit: 200, cursor } })),
  create: async (body) => {
    const created = await api.post('/keys', { body });
    return { name: created.name, spec: created.spec, metadata: created.metadata };
  },
  createWithSecret: (body) => api.post('/keys', { body }),
  update: (name, spec, etag) => api.put('/keys/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  remove: async (name, { reason, etag }) => {
    await api.delete('/keys/{name}', { path: { name }, reason, ifMatch: etag });
  },
};

export const keysApi = {
  setEnabled: (
    name: string,
    enabled: boolean,
    etag: string | undefined,
    reason?: string,
  ): Promise<KeyResource> =>
    api.patch('/keys/{name}', { path: { name }, body: { spec: { enabled } }, ifMatch: etag, reason }),
  rotate: (name: string, gracePeriod: string, reason?: string): Promise<KeyRotated> =>
    api.post('/keys/{name}:rotate', { path: { name }, body: { grace_period: gracePeriod }, reason }),
  projects: (): Promise<ProjectResource[]> =>
    listAll((cursor) => api.get('/projects', { query: { limit: 200, cursor } })),
  teams: (): Promise<TeamResource[]> =>
    listAll((cursor) => api.get('/teams', { query: { limit: 200, cursor } })),
  users: (): Promise<UserResource[]> =>
    listAll((cursor) => api.get('/users', { query: { limit: 200, cursor } })),
  audit: async (name: string): Promise<AuditEntry[]> =>
    (await api.get('/audit', { query: { resource: `keys/${name}`, limit: 50 } })).items,
  budgets: async (): Promise<BudgetCounter[]> =>
    listAll((cursor) => api.get('/runtime/budgets', { query: { limit: 200, cursor } })),
  anomalies: async (): Promise<Anomaly[]> =>
    (await api.get('/anomalies', { query: { state: 'open', limit: 100 } })).items,
  dismissAnomaly: (id: string) => api.post('/anomalies/{anomaly_id}:dismiss', { path: { anomaly_id: id } }),
  /** Every key's requests and spend per hour over `[from, to)`, summed across keys. */
  hourly: async (
    from: string,
    to: string,
    key?: string,
  ): Promise<{ start: string; requests: number; cost: number }[]> => {
    const items = await listAll(
      (cursor) =>
        api.get('/analytics/usage', {
          query: {
            from,
            to,
            interval: '1h',
            limit: 1000,
            ...(key ? { key: [key] } : {}),
            ...(cursor ? { cursor } : {}),
          },
        }),
      10,
    );
    const by = new Map<string, { start: string; requests: number; cost: number }>();
    for (const b of items) {
      const prev = by.get(b.start) ?? { start: b.start, requests: 0, cost: 0 };
      by.set(b.start, {
        start: b.start,
        requests: prev.requests + b.requests,
        cost: prev.cost + Number(b.cost_usd),
      });
    }
    return [...by.values()].sort((a, b) => a.start.localeCompare(b.start));
  },
  /** A key's source addresses over `[from, to)`, with their country when GeoIP knows it. */
  topIps: async (
    key: string,
    from: string,
    to: string,
  ): Promise<{ ip: string; country: string | null; requests: number; errors: number }[]> => {
    const result = await api.get('/analytics/usage', {
      query: { from, to, key: [key], group_by: ['client_ip', 'client_country'] },
    });
    const by = new Map<string, { ip: string; country: string | null; requests: number; errors: number }>();
    for (const b of result.items) {
      const ip = b.group.client_ip;
      if (!ip) continue;
      const prev = by.get(ip) ?? { ip, country: b.group.client_country ?? null, requests: 0, errors: 0 };
      by.set(ip, { ...prev, requests: prev.requests + b.requests, errors: prev.errors + b.errors });
    }
    return [...by.values()].sort((a, b) => b.requests - a.requests);
  },
  /** The key's rate-limit counters with their current use. */
  limits: async (name: string): Promise<LimitCounter[]> =>
    listAll((cursor) =>
      api.get('/runtime/limits', { query: { level: 'key', owner: name, limit: 200, cursor } }),
    ),
  /** Key events across the scope: creations, edits, rotations, revocations, reminders and flags. */
  activity: async (): Promise<AuditEntry[]> =>
    (await api.get('/audit', { query: { resource: 'keys/*', limit: 200 } })).items,
  /** Sign-in and key settings: named IP sets and whether anomaly detection is on. */
  auth: () => api.get('/settings/auth'),
  organizations: (): Promise<OrganizationResource[]> =>
    listAll((cursor) => api.get('/organizations', { query: { limit: 200, cursor } })),
  organization: (name: string): Promise<OrganizationResource> =>
    api.get('/organizations/{name}', { path: { name } }),
  /** Writes an organization's key policy as a merge patch. */
  setKeyPolicy: (
    organization: string,
    policy: Record<string, unknown>,
    etag: string | undefined,
    reason?: string,
  ) =>
    api.patch('/organizations/{name}', {
      path: { name: organization },
      body: { spec: { key_policy: policy } },
      ifMatch: etag,
      reason,
    }),
  /** The regions the deployments report, for the key form's region lists. */
  regions: async (): Promise<string[]> => {
    const deployments = await listAll((cursor) => api.get('/deployments', { query: { limit: 200, cursor } }));
    const regions = deployments.map(
      (d) =>
        (d.metadata as { region?: string | null }).region ?? (d.spec as { region?: string }).region ?? null,
    );
    return [...new Set(regions.filter((r): r is string => Boolean(r)))].sort();
  },
  usage24h: async (from: string, to: string): Promise<Map<string, { requests: number; cost: number }>> => {
    const result = await api.get('/analytics/usage', { query: { from, to, group_by: ['key'] } });
    const map = new Map<string, { requests: number; cost: number }>();
    for (const row of result.items) {
      const key = row.group.key;
      if (!key) continue;
      const prev = map.get(key) ?? { requests: 0, cost: 0 };
      map.set(key, { requests: prev.requests + row.requests, cost: prev.cost + Number(row.cost_usd) });
    }
    return map;
  },
};
