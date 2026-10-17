import { create } from 'zustand';

import type { UsageBucket } from '@/lib/api/types';
import { asApiError, emptySlot, type Slot } from '@/lib/slot';

import { analyticsApi, type UsageQuery } from './api';

interface AnalyticsState {
  /** Usage series by a caller-chosen name ("series", "byModel", "previous"…). */
  usage: Record<string, Slot<UsageBucket[]>>;
  loadUsage: (name: string, query: UsageQuery, options?: { quiet?: boolean }) => Promise<void>;
}

/** Aggregates from the analytics API, shared by the Overview and Analytics screens. */
export const useAnalytics = create<AnalyticsState>()((set, get) => ({
  usage: {},
  async loadUsage(name, query, options) {
    const previous = get().usage[name] ?? emptySlot<UsageBucket[]>();
    if (!options?.quiet || previous.data === null)
      set((s) => ({ usage: { ...s.usage, [name]: { ...previous, status: 'loading', error: null } } }));
    try {
      const data = await analyticsApi.usage(query);
      set((s) => ({ usage: { ...s.usage, [name]: { data, status: 'ready', error: null, at: Date.now() } } }));
    } catch (error) {
      set((s) => ({
        usage: {
          ...s.usage,
          [name]: { data: s.usage[name]?.data ?? null, status: 'error', error: asApiError(error), at: null },
        },
      }));
    }
  },
}));

export interface Totals {
  requests: number;
  errors: number;
  cost: number;
  input: number;
  output: number;
  cacheHits: number;
  saved: number;
  fallbacks: number;
  flags: number;
  p50: number | null;
  p95: number | null;
  p99: number | null;
}

/** Sums a usage series; latency percentiles are request-weighted means of the buckets'. */
export function totals(rows: UsageBucket[] | null | undefined): Totals | null {
  if (!rows) return null;
  const t: Totals = {
    requests: 0,
    errors: 0,
    cost: 0,
    input: 0,
    output: 0,
    cacheHits: 0,
    saved: 0,
    fallbacks: 0,
    flags: 0,
    p50: null,
    p95: null,
    p99: null,
  };
  let w = 0;
  let p50 = 0;
  let p95 = 0;
  let p99 = 0;
  for (const r of rows) {
    t.requests += r.requests;
    t.errors += r.errors;
    t.cost += Number(r.cost_usd);
    t.input += r.input ?? 0;
    t.output += r.output ?? 0;
    t.cacheHits += r.cache_hits ?? 0;
    t.saved += Number(r.cache_saved_usd ?? 0);
    t.fallbacks += r.fallbacks ?? 0;
    t.flags += (r.guardrail_flags ?? 0) + (r.guardrail_blocks ?? 0) + (r.guardrail_redactions ?? 0);
    if (r.latency_ms?.p50 != null && r.requests > 0) {
      w += r.requests;
      p50 += r.latency_ms.p50 * r.requests;
      p95 += (r.latency_ms.p95 ?? 0) * r.requests;
      p99 += (r.latency_ms.p99 ?? 0) * r.requests;
    }
  }
  if (w > 0) {
    t.p50 = p50 / w;
    t.p95 = p95 / w;
    t.p99 = p99 / w;
  }
  return t;
}

/** Rows grouped by one dimension and summed, largest first. */
export function groupTotals(
  rows: UsageBucket[] | null | undefined,
  dimension: string,
): { name: string; totals: Totals }[] {
  if (!rows) return [];
  const groups = new Map<string, UsageBucket[]>();
  for (const r of rows) {
    const key = r.group[dimension] ?? 'other';
    groups.set(key, [...(groups.get(key) ?? []), r]);
  }
  return [...groups.entries()]
    .map(([name, list]) => ({ name, totals: totals(list) ?? ({} as Totals) }))
    .sort((a, b) => b.totals.requests - a.totals.requests);
}
