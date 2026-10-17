import { api, listAll } from '@/lib/api/client';
import type {
  Alert,
  AuditEntry,
  ConfigDocument,
  DeploymentHealth,
  DeploymentResource,
  Health,
  LogRecord,
  RouteResource,
  Tier,
  UsageBucket,
} from '@/lib/api/types';
import type { components } from '@/lib/api/schema';

export type RouteSimulation = components['schemas']['RouteSimulation'];
export type RouteStateEntry = components['schemas']['RouteStateEntry'];

/**
 * What a route meets: the Models that name it (no other Model's requests reach it) and what its
 * root does with a request no rule matches (`error`, its `default`, or `none` when it has no rules).
 */
export interface RouteReach {
  unmatched: 'error' | 'default' | 'none' | null;
  models: string[] | null;
}
export type PoliciesSettings = components['schemas']['PoliciesSettings'];
export type ResilienceSettings = components['schemas']['ResilienceSettings'];
export type ServerSettings = components['schemas']['ServerSettings'];
type Route = components['schemas']['Route'];

export const routingApi = {
  routes: (): Promise<RouteResource[]> =>
    listAll((cursor) => api.get('/routes', { query: { limit: 200, cursor } })),
  deployments: (): Promise<DeploymentResource[]> =>
    listAll((cursor) => api.get('/deployments', { query: { limit: 200, cursor } })),
  health: async (): Promise<DeploymentHealth[]> => (await api.get('/runtime/deployments')).items,
  save: (name: string, spec: Route, etag: string | undefined, reason?: string): Promise<RouteResource> =>
    api.put('/routes/{name}', { path: { name }, body: { spec }, ifMatch: etag, reason }),
  remove: async (name: string, reason?: string): Promise<void> => {
    await api.delete('/routes/{name}', { path: { name }, reason });
  },
  simulate: (name: string, spec: Route, from: string, to: string): Promise<RouteSimulation> =>
    api.post('/routes/{name}:simulate', { path: { name }, body: { spec, from, to } }),
  resilience: (): Promise<ResilienceSettings> => api.get('/settings/resilience'),
  patchResilience: (spec: Record<string, unknown>, etag: string | undefined): Promise<ResilienceSettings> =>
    api.patch('/settings/resilience', { body: { spec }, ifMatch: etag }),
  policies: (): Promise<PoliciesSettings> => api.get('/settings/policies'),
  patchPolicies: (spec: Record<string, unknown>, etag: string | undefined): Promise<PoliciesSettings> =>
    api.patch('/settings/policies', { body: { spec }, ifMatch: etag }),
  server: (): Promise<ServerSettings> => api.get('/settings/server'),
  patchServer: (spec: Record<string, unknown>, etag: string | undefined): Promise<ServerSettings> =>
    api.patch('/settings/server', { body: { spec }, ifMatch: etag }),
  /** The gateway's admission: requests in flight against the cap, on the answering node. */
  gatewayHealth: (): Promise<Health> => api.get('/health'),
  tiers: async (): Promise<Tier[]> => (await api.get('/routing/tiers')).items,
  /** Each node of a route with its children, canary and mirror, on the answering replica. */
  async state(name: string): Promise<{ items: RouteStateEntry[]; reach: RouteReach }> {
    const items: RouteStateEntry[] = [];
    let reach: RouteReach = { unmatched: null, models: null };
    let cursor: string | undefined;
    for (let page = 0; page < 20; page += 1) {
      const r = await api.get('/routes/{name}/state', {
        path: { name },
        query: { limit: 200, ...(cursor ? { cursor } : {}) },
      });
      items.push(...r.items);
      if (page === 0) reach = { unmatched: r.unmatched ?? null, models: r.models ?? null };
      cursor = r.next_cursor ?? undefined;
      if (!cursor) break;
    }
    return { items, reach };
  },
  /** Ends every session binding of a route; returns how many there were. */
  resetAffinity: async (name: string): Promise<number> =>
    (await api.delete('/routes/{name}/affinity', { path: { name } })).deleted,
  /** Changes to routes and to the resilience and policy settings, newest first. */
  audit: async (): Promise<AuditEntry[]> => {
    const reads = await Promise.all(
      ['routes/*', 'settings/resilience', 'settings/policies', 'settings/server'].map((resource) =>
        api.get('/audit', { query: { resource, kind: 'write', limit: 50 } }),
      ),
    );
    return reads.flatMap((r) => r.items).sort((a, b) => b.time.localeCompare(a.time));
  },
  /** Alert instances of the last 7 days (firing ones mark rules, every one feeds the change feed). */
  alerts: (from: string): Promise<Alert[]> =>
    listAll((cursor) => api.get('/alerts', { query: { from, limit: 200, cursor } }), 3),
  /** Requests per tree position, with blocks and fallbacks, over a window. */
  usageBy: (
    from: string,
    to: string,
    group: ('route_node' | 'guardrail_category')[],
  ): Promise<UsageBucket[]> =>
    listAll(
      (cursor) =>
        api.get('/analytics/usage', {
          query: { from, to, group_by: group, limit: 1000, ...(cursor ? { cursor } : {}) },
        }),
      5,
    ),
  /** The newest records, for decision timing and the live table's first rows. */
  recent: async (from: string, limit: number): Promise<LogRecord[]> =>
    (await api.get('/logs', { query: { from, limit } })).items,
  guardrails: async (): Promise<string[]> =>
    (await listAll((cursor) => api.get('/guardrails', { query: { limit: 200, cursor } }))).map((g) => g.name),
  /** The whole configuration as JSON; the export keeps its routes and Models. */
  config: (): Promise<ConfigDocument> => api.get('/config'),
};
