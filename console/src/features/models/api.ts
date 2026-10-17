import { api, listAll } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';
import type {
  Alert,
  AlertRuleResource,
  AuditEntry,
  LatencyHistogram,
  UsageBucket,
  DeploymentHealth,
  DeploymentResource,
  DeploymentSpec,
  EffectivePrice,
  ModelResource,
  ModelSpec,
  ProviderResource,
  RouteResource,
  Tier,
} from '@/lib/api/types';
import type { CollectionApi } from '@/lib/collection-store';

import type { AccessRequest, CatalogStatus, DeploymentCapabilities } from './catalog';

type S = components['schemas'];
export type RoutingSettings = S['RoutingSettings'];
export type PoliciesSettings = S['PoliciesSettings'];
export type GuardrailResource = S['GuardrailResource'];
export type ProviderStatus = S['ProviderStatus'];
export type OrganizationResource = S['OrganizationResource'];

export type ModelCreateInput = { name: string; spec: ModelSpec };

/** A model's usage over the last 30 days, in total and per day. */
export interface ModelUsageRow {
  requests: number;
  cost: number;
  errors: number;
  daily: number[];
  days: { start: string; requests: number; cost: number; errors: number }[];
  /** The day buckets as the gateway sent them (tokens, latency, cache…). */
  buckets: UsageBucket[];
  /**
   * Requests that reached each provider. A request refused before routing names none, so these add
   * up to less than `requests`.
   */
  providers: Record<string, number>;
}

/** One read of usage analytics for a window, every page, optionally for one Model. */
async function usagePages(query: {
  from: string;
  to: string;
  interval: '1h' | '1d';
  group_by: components['parameters']['AnalyticsGroupBy'];
  model?: string[];
}): Promise<UsageBucket[]> {
  return listAll((cursor) => api.get('/analytics/usage', { query: { ...query, limit: 1000, cursor } }), 20);
}

export const modelsCollection: CollectionApi<ModelResource, ModelCreateInput, ModelSpec> = {
  list: () => listAll((cursor) => api.get('/models', { query: { limit: 200, cursor } })),
  create: (body) => api.post('/models', { body }),
  update: (name, spec, etag) => api.put('/models/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  remove: async (name, { reason, etag }) => {
    await api.delete('/models/{name}', { path: { name }, reason, ifMatch: etag });
  },
};

export const modelsApi = {
  deployments: (): Promise<DeploymentResource[]> =>
    listAll((cursor) => api.get('/deployments', { query: { limit: 200, cursor } })),
  providers: (): Promise<ProviderResource[]> =>
    listAll((cursor) => api.get('/providers', { query: { limit: 200, cursor } })),
  routes: (): Promise<RouteResource[]> =>
    listAll((cursor) => api.get('/routes', { query: { limit: 200, cursor } })),
  tiers: async (): Promise<Tier[]> => (await api.get('/routing/tiers')).items,
  health: async (): Promise<DeploymentHealth[]> => (await api.get('/runtime/deployments')).items,
  price: (deployment: string): Promise<EffectivePrice> =>
    api.get('/pricing/deployments/{name}', { path: { name: deployment } }),
  audit: async (name: string): Promise<AuditEntry[]> =>
    (await api.get('/audit', { query: { resource: `models/${name}`, limit: 50 } })).items,
  createDeployment: (name: string, spec: DeploymentSpec): Promise<DeploymentResource> =>
    api.post('/deployments', { body: { name, spec } }),
  deleteDeployment: (name: string) => api.delete('/deployments/{name}', { path: { name } }),
  patchDeployment: (
    name: string,
    spec: Partial<DeploymentSpec>,
    etag: string | undefined,
    reason?: string,
  ): Promise<DeploymentResource> =>
    api.patch('/deployments/{name}', { path: { name }, body: { spec }, ifMatch: etag, reason }),
  setEnabled: (
    name: string,
    enabled: boolean,
    etag: string | undefined,
    reason?: string,
  ): Promise<ModelResource> =>
    api.patch('/models/{name}', { path: { name }, body: { spec: { enabled } }, ifMatch: etag, reason }),
  usage: async (from: string, to: string): Promise<Map<string, ModelUsageRow>> => {
    const [items, byProvider] = await Promise.all([
      usagePages({ from, to, interval: '1d', group_by: ['model'] }),
      usagePages({ from, to, interval: '1d', group_by: ['model', 'provider'] }),
    ]);
    const map = new Map<string, ModelUsageRow>();
    for (const row of items) {
      const model = row.group.model;
      if (!model) continue;
      const prev = map.get(model) ?? {
        requests: 0,
        cost: 0,
        errors: 0,
        daily: [],
        days: [],
        buckets: [],
        providers: {},
      };
      map.set(model, {
        ...prev,
        requests: prev.requests + row.requests,
        cost: prev.cost + Number(row.cost_usd),
        errors: prev.errors + row.errors,
        daily: [...prev.daily, row.requests],
        days: [
          ...prev.days,
          { start: row.start, requests: row.requests, cost: Number(row.cost_usd), errors: row.errors },
        ],
        buckets: [...prev.buckets, row],
      });
    }
    for (const row of byProvider) {
      const entry = row.group.model ? map.get(row.group.model) : undefined;
      const provider = row.group.provider;
      if (!entry || !provider) continue;
      entry.providers[provider] = (entry.providers[provider] ?? 0) + row.requests;
    }
    return map;
  },
  /** One Model's usage over a window, grouped by one more key (team, key, requested name…). */
  modelUsage: (
    model: string,
    from: string,
    to: string,
    group: 'team' | 'key' | 'requested_model' | 'route_node',
  ): Promise<UsageBucket[]> => usagePages({ from, to, interval: '1d', group_by: [group], model: [model] }),
  /** Every Model's usage by requested name (aliases), or every route's by tree position. */
  usageBy: (
    from: string,
    to: string,
    group: ('model' | 'requested_model' | 'route_node')[],
  ): Promise<UsageBucket[]> => usagePages({ from, to, interval: '1d', group_by: group }),
  histogram: (model: string, from: string, to: string): Promise<LatencyHistogram> =>
    api.get('/analytics/latency_histogram', { query: { from, to, model: [model] } }),
  /** The gateway's price settings: list prices by `<provider_type>/<upstream_model>`. */
  pricing: () => api.get('/settings/pricing'),
  /** Merges list prices into the price settings (a null member removes it). */
  patchPricing: (models: Record<string, unknown>, etag: string | undefined, reason?: string) =>
    api.patch('/settings/pricing', { body: { spec: { models } }, ifMatch: etag, reason }),
  /** Every deployment's effective price in one read. */
  prices: (): Promise<EffectivePrice[]> =>
    listAll((cursor) => api.get('/pricing/deployments', { query: { limit: 200, cursor } })),
  catalogStatus: (): Promise<CatalogStatus> => api.get('/catalog'),
  capabilities: (deployment: string): Promise<DeploymentCapabilities> =>
    api.get('/deployments/{name}/capabilities', { path: { name: deployment } }),
  accessRequests: (model: string): Promise<AccessRequest[]> =>
    listAll((cursor) =>
      api.get('/models/{name}/access_requests', { path: { name: model }, query: { limit: 200, cursor } }),
    ),
  approveAccess: (model: string, id: string, reason?: string): Promise<AccessRequest> =>
    api.post('/models/{name}/access_requests/{request_id}:approve', {
      path: { name: model, request_id: id },
      body: reason ? { reason } : {},
    }),
  denyAccess: (model: string, id: string, reason?: string): Promise<AccessRequest> =>
    api.post('/models/{name}/access_requests/{request_id}:deny', {
      path: { name: model, request_id: id },
      body: reason ? { reason } : {},
    }),
  replace: (
    name: string,
    spec: ModelSpec,
    etag: string | undefined,
    reason?: string,
  ): Promise<ModelResource> =>
    api.put('/models/{name}', { path: { name }, body: { spec }, ifMatch: etag, reason }),
  patch: (
    name: string,
    spec: Partial<ModelSpec>,
    etag: string | undefined,
    reason?: string,
  ): Promise<ModelResource> =>
    api.patch('/models/{name}', { path: { name }, body: { spec }, ifMatch: etag, reason }),
  routing: (): Promise<RoutingSettings> => api.get('/settings/routing'),
  patchRouting: (spec: Record<string, unknown>, etag: string | undefined, reason?: string) =>
    api.patch('/settings/routing', { body: { spec }, ifMatch: etag, reason }),
  policies: (): Promise<PoliciesSettings> => api.get('/settings/policies'),
  patchPolicies: (spec: Record<string, unknown>, etag: string | undefined, reason?: string) =>
    api.patch('/settings/policies', { body: { spec }, ifMatch: etag, reason }),
  guardrails: (): Promise<GuardrailResource[]> =>
    listAll((cursor) => api.get('/guardrails', { query: { limit: 200, cursor } })),
  organizations: (): Promise<OrganizationResource[]> =>
    listAll((cursor) => api.get('/organizations', { query: { limit: 200, cursor } })),
  providerStatus: async (): Promise<ProviderStatus[]> => (await api.get('/runtime/providers')).items,
  alertRules: (): Promise<AlertRuleResource[]> =>
    listAll((cursor) => api.get('/alerts/rules', { query: { limit: 200, cursor } })),
  /** Alert instances fired since `from`, newest first. */
  alerts: async (from: string): Promise<Alert[]> =>
    listAll((cursor) => api.get('/alerts', { query: { from, limit: 200, cursor } }), 5),
  /** Audit entries under a resource pattern (`models/*`), newest first. */
  auditUnder: async (resource: string, limit = 50): Promise<AuditEntry[]> =>
    (await api.get('/audit', { query: { resource, limit, kind: 'write' } })).items,
};
