import { api, listAll } from '@/lib/api/client';
import type {
  AuditEntry,
  CacheEntry,
  CacheSettings,
  CacheStatus,
  ChangeRequest,
  InvalidationRuleResource,
  KeyResource,
  ModelResource,
  ModelSpec,
  OrganizationResource,
  ProjectResource,
  RouteResource,
  TeamResource,
  UsageBucket,
} from '@/lib/api/types';
import type { components } from '@/lib/api/schema';

export type InvalidateLevel = 'gateway' | 'organization' | 'team' | 'project' | 'route' | 'model';
export type PolicyCache = NonNullable<NonNullable<ModelSpec['policies']>['cache']>;
export type RuleSpec = InvalidationRuleResource['spec'];
export type RuleStatus = components['schemas']['InvalidationRuleStatus'];
export type EffectivePrice = components['schemas']['EffectivePrice'];
export type PoliciesSettings = components['schemas']['PoliciesSettings'];
type GovernanceSettings = components['schemas']['GovernanceSettings'];

export type EntrySort = 'hits' | 'last_hit_at' | 'created_at' | 'expires_at' | 'size';

export interface EntryQuery {
  model?: string | undefined;
  mode?: 'exact' | 'semantic' | undefined;
  q?: string | undefined;
  sort?: EntrySort | undefined;
  order?: 'asc' | 'desc' | undefined;
}

export interface EntryPage {
  items: CacheEntry[];
  next: string | null;
  /** False when the backend's entry index could not be read. */
  enumerable: boolean;
  total: number | null;
  totalBytes: number | null;
  totalHits: number | null;
}

const PAGE = 50;

/** Usage over a window, every page (a week of hourly buckets is more than one). */
function usage(
  from: string,
  to: string,
  extra: {
    interval?: '1h' | '1d';
    group_by?: ('model' | 'team' | 'deployment' | 'cache_prefix' | 'provider')[];
  },
): Promise<UsageBucket[]> {
  return listAll(
    (cursor) =>
      api.get('/analytics/usage', {
        query: { from, to, limit: 1000, ...extra, ...(cursor ? { cursor } : {}) },
      }),
    5,
  );
}

export const cacheApi = {
  status: (): Promise<CacheStatus> => api.get('/runtime/cache'),
  settings: (): Promise<CacheSettings> => api.get('/settings/cache'),
  models: (): Promise<ModelResource[]> =>
    listAll((cursor) => api.get('/models', { query: { limit: 200, cursor } })),
  setModelCache: (
    name: string,
    cache: Record<string, unknown>,
    etag: string | undefined,
  ): Promise<ModelResource> =>
    api.patch('/models/{name}', {
      path: { name },
      body: { spec: { policies: { cache } } },
      ifMatch: etag,
    }),
  invalidate: (level: InvalidateLevel, name: string | undefined, reason?: string) =>
    api.post('/cache:invalidate', { body: { level, ...(name ? { name } : {}) }, reason }),

  async entries(query: EntryQuery, cursor?: string): Promise<EntryPage> {
    const r = await api.get('/cache/entries', {
      query: {
        limit: PAGE,
        include_total: true,
        ...(query.model ? { model: query.model } : {}),
        ...(query.mode ? { mode: query.mode } : {}),
        ...(query.q ? { q: query.q } : {}),
        sort: query.sort ?? 'last_hit_at',
        order: query.order ?? 'desc',
        ...(cursor ? { cursor } : {}),
      },
    });
    return {
      items: r.items,
      next: r.next_cursor ?? null,
      enumerable: r.enumerable,
      total: r.total ?? null,
      totalBytes: r.total_size_bytes ?? null,
      totalHits: r.total_hits ?? null,
    };
  },
  /** One entry with its cached response (null for readers without `logs:read_content`). */
  entry: (key: string): Promise<CacheEntry> => api.get('/cache/entries/{key}', { path: { key } }),
  deleteEntry: (key: string, reason?: string) =>
    api.delete('/cache/entries/{key}', { path: { key }, reason }),
  reportFalsePositive: (key: string) =>
    api.post('/cache/entries/{key}:report_false_positive', { path: { key } }),

  rules: (): Promise<InvalidationRuleResource[]> =>
    listAll((cursor) => api.get('/cache/invalidation_rules', { query: { limit: 200, cursor } })),
  ruleStatus: (): Promise<RuleStatus[]> =>
    listAll((cursor) => api.get('/runtime/cache/invalidation_rules', { query: { limit: 200, cursor } })),
  runRule: async (rule: string): Promise<number> =>
    (await api.post('/cache/invalidation_rules/{rule}:run', { path: { rule } })).dropped,
  setRuleEnabled: (rule: string, enabled: boolean) =>
    api.patch('/cache/invalidation_rules/{rule}', { path: { rule }, body: { spec: { enabled } } }),
  createRule: (name: string, spec: RuleSpec): Promise<InvalidationRuleResource> =>
    api.post('/cache/invalidation_rules', { body: { name, spec } }),
  replaceRule: (name: string, spec: RuleSpec, etag: string | undefined): Promise<InvalidationRuleResource> =>
    api.put('/cache/invalidation_rules/{rule}', { path: { rule: name }, body: { spec }, ifMatch: etag }),
  deleteRule: (name: string, reason?: string) =>
    api.delete('/cache/invalidation_rules/{rule}', { path: { rule: name }, reason }),

  /** Hourly buckets of a window (the chart and the hero). */
  /** Hourly usage by provider: a request refused before routing names none. */
  series: (from: string, to: string) => usage(from, to, { interval: '1h', group_by: ['provider'] }),
  /** One bucket per group over a window. */
  grouped: (from: string, to: string, group: ('model' | 'team' | 'deployment' | 'cache_prefix')[]) =>
    usage(from, to, { group_by: group }),

  prices: (): Promise<EffectivePrice[]> =>
    listAll((cursor) => api.get('/pricing/deployments', { query: { limit: 200, cursor } })),
  routes: (): Promise<RouteResource[]> =>
    listAll((cursor) => api.get('/routes', { query: { limit: 200, cursor } })),
  policies: (): Promise<PoliciesSettings> => api.get('/settings/policies'),
  organizations: (): Promise<OrganizationResource[]> =>
    listAll((cursor) => api.get('/organizations', { query: { limit: 200, cursor } })),
  teams: (): Promise<TeamResource[]> =>
    listAll((cursor) => api.get('/teams', { query: { limit: 200, cursor } })),
  projects: (): Promise<ProjectResource[]> =>
    listAll((cursor) => api.get('/projects', { query: { limit: 200, cursor } })),
  keys: (): Promise<KeyResource[]> =>
    listAll((cursor) => api.get('/keys', { query: { limit: 200, cursor } })),

  governance: (): Promise<GovernanceSettings> => api.get('/settings/governance'),
  /** Open requests the two-person rule holds. */
  pending: async (): Promise<ChangeRequest[]> =>
    (await api.get('/change_requests', { query: { state: ['open', 'approved'], limit: 200 } })).items,

  /**
   * Cache activity, newest first: invalidations, removals, reports, rule firings and rule edits
   * (`cache.*`), storage settings, and edits of the cache policy of Models and the gateway default.
   */
  async activity(): Promise<AuditEntry[]> {
    const reads = await Promise.all([
      api.get('/audit', { query: { action: 'cache.*', limit: 100 } }),
      api.get('/audit', { query: { resource: 'settings/cache', kind: 'write', limit: 30 } }),
      api.get('/audit', { query: { resource: 'settings/policies', kind: 'write', limit: 30 } }),
      api.get('/audit', { query: { resource: 'models/*', kind: 'write', limit: 100 } }),
    ]);
    const seen = new Set<string>();
    return reads
      .flatMap((r) => r.items)
      .filter((e) => (seen.has(e.id) ? false : (seen.add(e.id), true)))
      .sort((a, b) => b.time.localeCompare(a.time));
  },
};
