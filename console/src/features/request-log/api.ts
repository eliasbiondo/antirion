import { api, API_BASE, ApiError } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';
import type { LogList, LogRecord, LogView } from '@/lib/api/types';
import { parseDuration } from '@/lib/format';

import { filtersToQuery, type LogFilters, type LogSort, type SortOrder } from './types';

/** A facet's value and its count; `requested` marks a name asked for that no Model has. */
export interface FacetValue {
  value: string;
  count: number;
  requested?: boolean;
}

export interface LogQuery {
  from: string;
  to: string;
  filters: LogFilters;
  sort: LogSort;
  order: SortOrder;
  organization: string | null;
  /** The scope's environment tag (`env:prod`). */
  tag: string | null;
}

export type LogViewUpdate = components['schemas']['LogViewUpdate'];
export type LogViewCreate = components['schemas']['LogViewCreate'];
export type LatencyHistogram = components['schemas']['LatencyHistogram'];
export type ReplayResult = components['schemas']['PlaygroundResult'];

function scoped(q: LogQuery) {
  const tags = [...q.filters.tag, ...(q.tag ? [q.tag] : [])];
  return {
    from: q.from,
    to: q.to,
    ...filtersToQuery(q.filters),
    ...(q.organization ? { organization: [q.organization] } : {}),
    ...(tags.length ? { tag: tags } : {}),
  };
}

/** The equality filters the live tail accepts; the rest are checked on each record it sends. */
function tailParams(q: LogQuery): URLSearchParams {
  const params = new URLSearchParams();
  const f = q.filters;
  if (q.organization) params.append('organization', q.organization);
  for (const v of f.provider) params.append('provider', v);
  for (const v of f.model) params.append('model', v);
  for (const v of f.team) params.append('team', v);
  for (const v of f.project) params.append('project', v);
  for (const v of f.key) params.append('key', v);
  for (const v of f.requestId) params.append('request_id', v);
  if (q.tag) params.append('tag', q.tag);
  params.set('from', q.from);
  return params;
}

/**
 * Query parameters of the contract this gateway refused as unknown. The log stops sending them and
 * applies those filters to the loaded records instead, so the screen works against an older gateway.
 */
const unsupported = new Set<string>();

export function unsupportedParams(): ReadonlySet<string> {
  return unsupported;
}

function unknownParam(error: unknown): string | null {
  if (!(error instanceof ApiError) || error.status !== 400 || error.code !== 'invalid_parameter') return null;
  return /unknown query parameter "([a-z_]+)"/.exec(error.message)?.[1] ?? null;
}

async function listWithFallback(query: Record<string, unknown>): Promise<LogList> {
  for (let i = 0; i < 12; i += 1) {
    const sent = Object.fromEntries(Object.entries(query).filter(([k]) => !unsupported.has(k)));
    try {
      return await api.get('/logs', { query: sent });
    } catch (error) {
      const param = unknownParam(error);
      if (!param || unsupported.has(param)) throw error;
      unsupported.add(param);
    }
  }
  return api.get('/logs', { query: {} });
}

/** Lookups in flight, so the drawer and the recent list opening the same request share one read. */
const lookups = new Map<string, Promise<LogRecord>>();

async function lookup(id: string, isRecord: boolean): Promise<LogRecord> {
  if (!isRecord) {
    // A request id is found through the list's filter, so it never asks for a record that isn't there.
    const page = await api.get('/logs', { query: { request_id: id, limit: 1 } }).catch(() => null);
    const record = page?.items[0];
    if (record) return record;
  }
  return api.get('/logs/{id}', { path: { id } });
}

export const logsApi = {
  list: (q: LogQuery, cursor?: string): Promise<LogList> => {
    const sorted = q.sort === 'started_at' && q.order === 'desc' ? {} : { sort: q.sort, order: q.order };
    return listWithFallback({ ...scoped(q), limit: 100, ...sorted, ...(cursor ? { cursor } : {}) });
  },
  /**
   * A request by its record id or by the request id clients see (`x-request-id`, the drawer's
   * header, exports): the request id is looked up first, then the record; `isRecord` (a row of a
   * loaded list) skips the first step.
   */
  get(id: string, isRecord = false): Promise<LogRecord> {
    const pending = lookups.get(id);
    if (pending) return pending;
    const promise = lookup(id, isRecord).finally(() => lookups.delete(id));
    lookups.set(id, promise);
    return promise;
  },
  aggregates: async (q: LogQuery) => {
    const r = await api.get('/analytics/usage', { query: scoped(q) });
    const rows = r.items;
    const sum = (f: (b: (typeof rows)[number]) => number) => rows.reduce((a, b) => a + f(b), 0);
    return {
      requests: sum((b) => b.requests),
      errors: sum((b) => b.errors),
      cost: sum((b) => Number(b.cost_usd)),
      input: sum((b) => b.input ?? 0),
      output: sum((b) => b.output ?? 0),
      cacheHits: sum((b) => b.cache_hits ?? 0),
      p50: rows[0]?.latency_ms?.p50 ?? null,
      p95: rows[0]?.latency_ms?.p95 ?? null,
      p99: rows[0]?.latency_ms?.p99 ?? null,
    };
  },
  facets: async (
    q: LogQuery,
    dimension: 'provider' | 'team' | 'model' | 'status_class' | 'environment',
  ): Promise<FacetValue[]> => {
    // A group's counts leave out its own filter, so its other values keep their counts once one is picked.
    const own: Partial<LogFilters> = {
      provider: { provider: [] },
      team: { team: [] },
      model: { model: [], requestedModel: [] },
      status_class: { statusClass: [] },
      environment: { environment: [] },
    }[dimension];
    const others = { ...q, filters: { ...q.filters, ...own } };
    // Models are grouped with the name asked for too: a request for a name no Model has (no `model`)
    // is listed by that name, and filtered with `requested_model`.
    const groupBy = dimension === 'model' ? (['model', 'requested_model'] as const) : ([dimension] as const);
    const r = await api.get('/analytics/usage', { query: { ...scoped(others), group_by: [...groupBy] } });
    const map = new Map<string, FacetValue>();
    for (const b of r.items) {
      const v = b.group[dimension];
      const asked = dimension === 'model' && !v ? b.group.requested_model : null;
      const value = v ?? asked;
      if (!value) continue;
      const key = `${asked ? 'asked:' : ''}${value}`;
      const prev = map.get(key);
      map.set(key, { value, count: (prev?.count ?? 0) + b.requests, ...(asked ? { requested: true } : {}) });
    }
    // A refused request for a Model's own name carries no `model`: it counts under that Model.
    for (const [key, v] of map)
      if (v.requested && map.has(v.value)) {
        const own = map.get(v.value);
        if (own) own.count += v.count;
        map.delete(key);
      }
    return [...map.values()].sort((a, b) => b.count - a.count);
  },
  views: async (): Promise<LogView[]> => (await api.get('/logs/views', { query: { limit: 200 } })).items,
  createView: (body: LogViewCreate): Promise<LogView> => api.post('/logs/views', { body }),
  updateView: (id: string, body: LogViewUpdate): Promise<LogView> =>
    api.patch('/logs/views/{view_id}', { path: { view_id: id }, body }),
  deleteView: async (id: string): Promise<void> => {
    await api.delete('/logs/views/{view_id}', { path: { view_id: id } });
  },
  /** The server's latency histogram of the filtered set, without the latency range itself. */
  histogram: (q: LogQuery): Promise<LatencyHistogram> =>
    api.get('/analytics/latency_histogram', {
      query: {
        ...Object.fromEntries(
          Object.entries(scoped(q)).filter(([k]) => k !== 'latency_ms_min' && k !== 'latency_ms_max'),
        ),
        buckets: 24,
      },
    }),
  /** How long the store keeps records, from the observability settings (default 30 days). */
  retention: async (): Promise<number | null> => {
    const r = await api.get('/settings/observability');
    const spec = r.spec as { request_logs?: { sinks?: { store?: { retention?: string } } } };
    return parseDuration(spec.request_logs?.sinks?.store?.retention ?? '30d');
  },
  /** The live tail's address: new records as server-sent events. */
  tailUrl: (q: LogQuery): string => `${API_BASE}/logs:tail?${tailParams(q).toString()}`,
  /** The export URL; the browser downloads it with the session cookie. */
  exportUrl(q: LogQuery, format: 'csv' | 'ndjson' = 'csv'): string {
    const params = new URLSearchParams();
    for (const [k, v] of Object.entries(scoped(q))) {
      // A parameter the list refused would fail the export too; those filters apply in the page only.
      if (unsupported.has(k)) continue;
      if (Array.isArray(v)) for (const x of v) params.append(k, x);
      else params.append(k, String(v));
    }
    if (!(q.sort === 'started_at' && q.order === 'desc')) {
      params.set('sort', q.sort);
      params.set('order', q.order);
    }
    params.set('format', format);
    return `${API_BASE}/logs:export?${params.toString()}`;
  },
  replay: (id: string, patch: Record<string, unknown>[] = []): Promise<ReplayResult> =>
    api.post('/logs/{id}:replay', { path: { id }, body: patch.length ? { body_patch: patch } : {} }),
};
