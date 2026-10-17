import { create } from 'zustand';

import type {
  AuditEntry,
  CacheSettings,
  CacheStatus,
  ChangeRequest,
  InvalidationRuleResource,
  ModelResource,
  RouteResource,
  UsageBucket,
} from '@/lib/api/types';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  cacheApi,
  type EffectivePrice,
  type EntryPage,
  type EntryQuery,
  type InvalidateLevel,
  type PolicyCache,
  type RuleSpec,
  type RuleStatus,
} from './api';
import { overridesOf, usageByModel, type ModelUsage, type Override } from './policy';

export interface CacheBucket {
  start: string;
  requests: number;
  hits: number;
  /** Requests a provider answered (not from the cache, not refused before routing). */
  sent: number;
  saved: number;
  cost: number;
}

export interface Window {
  requests: number;
  hits: number;
  saved: number;
  cost: number;
}

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

/** One bucket per hour from usage grouped by provider; rows without a provider were not sent to one. */
export function bucketsOf(rows: UsageBucket[]): CacheBucket[] {
  const byStart = new Map<string, CacheBucket>();
  for (const b of rows) {
    const prev = byStart.get(b.start) ?? { start: b.start, requests: 0, hits: 0, sent: 0, saved: 0, cost: 0 };
    const hits = b.cache_hits ?? 0;
    byStart.set(b.start, {
      start: b.start,
      requests: prev.requests + b.requests,
      hits: prev.hits + hits,
      sent: prev.sent + (b.group.provider ? Math.max(0, b.requests - hits) : 0),
      saved: prev.saved + Number(b.cache_saved_usd ?? 0),
      cost: prev.cost + Number(b.cost_usd),
    });
  }
  return [...byStart.values()].sort((a, b) => a.start.localeCompare(b.start));
}

export function totalOf(buckets: CacheBucket[]): Window {
  return buckets.reduce(
    (a, b) => ({
      requests: a.requests + b.requests,
      hits: a.hits + b.hits,
      saved: a.saved + b.saved,
      cost: a.cost + b.cost,
    }),
    { requests: 0, hits: 0, saved: 0, cost: 0 },
  );
}

/** What the two-person rule says about clearing the whole cache. */
export interface FlushControl {
  /** Whether a second person must approve a gateway-wide clear. */
  dualControl: boolean;
  /** Requests to clear the whole cache waiting for approval. */
  pending: ChangeRequest[];
}

export interface Stats {
  teams: { team: string; requests: number; hits: number; saved: number }[];
  prefixes: UsageBucket[];
  overrides: Override[];
  /** The gateway-level cache policy (`policies.cache` of the settings), when readable. */
  gateway: PolicyCache | null;
}

export interface BulkResult {
  removed: number;
  failed: { key: string; error: unknown }[];
}

interface CacheState {
  status: Slot<CacheStatus>;
  settings: Slot<CacheSettings>;
  models: Slot<ModelResource[]>;
  routes: Slot<Map<string, RouteResource>>;
  prices: Slot<Map<string, EffectivePrice>>;
  rules: Slot<InvalidationRuleResource[]>;
  ruleStatus: Slot<Map<string, RuleStatus>>;
  /** Hourly buckets of the last 48 hours: the chart shows the last 24, the hero compares both. */
  series: Slot<CacheBucket[]>;
  byModel: Slot<Map<string, ModelUsage>>;
  entries: Slot<EntryPage>;
  stats: Slot<Stats>;
  activity: Slot<AuditEntry[]>;
  flush: Slot<FlushControl>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  loadSeries: () => Promise<void>;
  loadModelFacts: () => Promise<void>;
  loadEntries: (query: EntryQuery) => Promise<void>;
  loadMoreEntries: (query: EntryQuery) => Promise<void>;
  loadStats: () => Promise<void>;
  loadActivity: () => Promise<void>;
  loadFlush: () => Promise<void>;
  setModelCache: (model: string, cache: Record<string, unknown>) => Promise<void>;
  invalidate: (level: InvalidateLevel, name: string | undefined, reason?: string) => Promise<number>;
  deleteEntry: (key: string, reason?: string) => Promise<void>;
  deleteEntries: (keys: string[], reason?: string) => Promise<BulkResult>;
  reportFalsePositive: (key: string) => Promise<void>;
  runRule: (rule: string) => Promise<number>;
  setRuleEnabled: (rule: string, enabled: boolean) => Promise<void>;
  saveRule: (name: string, spec: RuleSpec, existing: InvalidationRuleResource | null) => Promise<void>;
  deleteRule: (rule: string, reason?: string) => Promise<void>;
}

/** The response cache: backend status, per-Model policy, entries, clearing rules and statistics. */
export const useCache = create<CacheState>()((set, get) => {
  const reloadRules = () =>
    Promise.all([
      loadSlot(set, get, 'rules', cacheApi.rules, { quiet: true }),
      loadSlot(set, get, 'ruleStatus', ruleStatus, { quiet: true }),
    ]);
  const ruleStatus = async () => new Map((await cacheApi.ruleStatus()).map((r) => [r.name, r]));
  const dropEntries = (keys: string[]) =>
    set((s) => {
      if (!s.entries.data) return {};
      const gone = new Set(keys);
      const removed = s.entries.data.items.filter((e) => gone.has(e.key));
      const items = s.entries.data.items.filter((e) => !gone.has(e.key));
      const d = s.entries.data;
      return {
        entries: {
          ...s.entries,
          data: {
            ...d,
            items,
            total: d.total === null ? null : Math.max(0, d.total - removed.length),
            totalBytes:
              d.totalBytes === null
                ? null
                : Math.max(0, d.totalBytes - removed.reduce((a, e) => a + e.size_bytes, 0)),
            totalHits:
              d.totalHits === null
                ? null
                : Math.max(0, d.totalHits - removed.reduce((a, e) => a + e.hits, 0)),
          },
        },
      };
    });

  return {
    status: emptySlot(),
    settings: emptySlot(),
    models: emptySlot(),
    routes: emptySlot(),
    prices: emptySlot(),
    rules: emptySlot(),
    ruleStatus: emptySlot(),
    series: emptySlot(),
    byModel: emptySlot(),
    entries: emptySlot(),
    stats: emptySlot(),
    activity: emptySlot(),
    flush: emptySlot(),

    async load(options) {
      await Promise.all([
        loadSlot(set, get, 'status', cacheApi.status, options),
        loadSlot(set, get, 'settings', cacheApi.settings, options),
        loadSlot(set, get, 'models', cacheApi.models, options),
        loadSlot(set, get, 'rules', cacheApi.rules, options),
        loadSlot(set, get, 'ruleStatus', ruleStatus, options),
      ]);
    },

    async loadSeries() {
      const to = new Date();
      const from = new Date(to.getTime() - 2 * DAY);
      await loadSlot(set, get, 'series', async () =>
        bucketsOf(await cacheApi.series(from.toISOString(), to.toISOString())),
      );
    },

    async loadModelFacts() {
      const to = new Date();
      const from = new Date(to.getTime() - 30 * DAY);
      await Promise.all([
        loadSlot(set, get, 'byModel', async () =>
          usageByModel(await cacheApi.grouped(from.toISOString(), to.toISOString(), ['model'])),
        ),
        loadSlot(
          set,
          get,
          'prices',
          async () => new Map((await cacheApi.prices()).map((p) => [p.deployment, p])),
        ),
        loadSlot(set, get, 'routes', async () => new Map((await cacheApi.routes()).map((r) => [r.name, r]))),
      ]);
    },

    loadEntries: (query) => loadSlot(set, get, 'entries', () => cacheApi.entries(query)),

    async loadMoreEntries(query) {
      const current = get().entries.data;
      if (!current?.next) return;
      const page = await cacheApi.entries(query, current.next);
      set((s) => ({
        entries: {
          ...s.entries,
          data: { ...page, items: [...(s.entries.data?.items ?? []), ...page.items] },
        },
      }));
    },

    async loadStats() {
      const to = new Date();
      const from = new Date(to.getTime() - 30 * DAY);
      await loadSlot(set, get, 'stats', async () => {
        const optional = <T>(p: Promise<T>): Promise<T | null> => p.catch(() => null);
        const [teamUsage, prefixes, orgs, teams, projects, keys, routes, policies] = await Promise.all([
          cacheApi.grouped(from.toISOString(), to.toISOString(), ['team']),
          cacheApi.grouped(from.toISOString(), to.toISOString(), ['cache_prefix', 'deployment']),
          optional(cacheApi.organizations()),
          optional(cacheApi.teams()),
          optional(cacheApi.projects()),
          optional(cacheApi.keys()),
          optional(cacheApi.routes()),
          optional(cacheApi.policies()),
        ]);
        const teamRows = new Map<string, Stats['teams'][number]>();
        for (const b of teamUsage) {
          const team = b.group.team;
          if (!team) continue;
          const row = teamRows.get(team) ?? { team, requests: 0, hits: 0, saved: 0 };
          row.requests += b.requests;
          row.hits += b.cache_hits ?? 0;
          row.saved += Number(b.cache_saved_usd ?? 0);
          teamRows.set(team, row);
        }
        return {
          teams: [...teamRows.values()].sort((a, b) => b.saved - a.saved || b.hits - a.hits),
          prefixes,
          overrides: overridesOf({
            organizations: (orgs ?? []).map((o) => ({ name: o.name, cache: o.spec.policies?.cache })),
            teams: (teams ?? []).map((t) => ({
              name: t.name,
              organization: t.spec.organization,
              cache: t.spec.policies?.cache,
            })),
            projects: (projects ?? []).map((p) => ({
              name: p.name,
              team: p.spec.team,
              cache: p.spec.policies?.cache,
            })),
            keys: (keys ?? []).map((k) => ({
              name: k.name,
              project: k.spec.project,
              cache: k.spec.policies?.cache,
            })),
            routes: (routes ?? []).map((r) => ({ name: r.name, cache: r.spec.policies?.cache })),
          }),
          gateway: policies?.spec.cache ?? null,
        };
      });
    },

    loadActivity: () => loadSlot(set, get, 'activity', () => cacheApi.activity()),

    async loadFlush() {
      await loadSlot(set, get, 'flush', async () => {
        const [governance, pending] = await Promise.all([
          cacheApi.governance().catch(() => null),
          cacheApi.pending().catch(() => []),
        ]);
        return {
          dualControl: governance?.spec.dual_control?.actions?.includes('cache_flush_all') === true,
          pending: pending.filter(
            (r) =>
              r.kind === 'action' &&
              r.changes.some((c) => c.path.split('?')[0]?.endsWith('/cache:invalidate')),
          ),
        };
      });
    },

    async setModelCache(model, cache) {
      const current = get().models.data?.find((m) => m.name === model);
      const updated = await cacheApi.setModelCache(model, cache, current?.metadata.etag);
      set((s) => ({
        models: { ...s.models, data: (s.models.data ?? []).map((m) => (m.name === model ? updated : m)) },
      }));
    },

    async invalidate(level, name, reason) {
      try {
        const result = await cacheApi.invalidate(level, name, reason);
        await loadSlot(set, get, 'status', cacheApi.status, { quiet: true });
        return result.epoch;
      } finally {
        if (level === 'gateway') void get().loadFlush();
      }
    },

    async deleteEntry(key, reason) {
      await cacheApi.deleteEntry(key, reason);
      dropEntries([key]);
    },

    async deleteEntries(keys, reason) {
      const failed: BulkResult['failed'] = [];
      const done: string[] = [];
      // A few at a time: each removal is its own audited call.
      for (let i = 0; i < keys.length; i += 5) {
        await Promise.all(
          keys.slice(i, i + 5).map(async (key) => {
            try {
              await cacheApi.deleteEntry(key, reason);
              done.push(key);
            } catch (error) {
              // Already gone (expired, or removed by someone else) counts as removed.
              if ((error as { status?: number }).status === 404) done.push(key);
              else failed.push({ key, error });
            }
          }),
        );
      }
      dropEntries(done);
      void loadSlot(set, get, 'status', cacheApi.status, { quiet: true });
      return { removed: done.length, failed };
    },

    async reportFalsePositive(key) {
      await cacheApi.reportFalsePositive(key);
      dropEntries([key]);
      void loadSlot(set, get, 'status', cacheApi.status, { quiet: true });
    },

    async runRule(rule) {
      try {
        return await cacheApi.runRule(rule);
      } finally {
        void reloadRules();
      }
    },

    async setRuleEnabled(rule, enabled) {
      await cacheApi.setRuleEnabled(rule, enabled);
      await reloadRules();
    },

    async saveRule(name, spec, existing) {
      if (existing) await cacheApi.replaceRule(name, spec, existing.metadata.etag);
      else await cacheApi.createRule(name, spec);
      await reloadRules();
    },

    async deleteRule(rule, reason) {
      await cacheApi.deleteRule(rule, reason);
      await reloadRules();
    },
  };
});
