import { create } from 'zustand';

import type { LogRecord, LogView } from '@/lib/api/types';
import { asApiError, emptySlot, loadSlot, type Slot } from '@/lib/slot';

import { type FacetValue, type LatencyHistogram, type LogQuery, type LogViewUpdate, logsApi } from './api';
import { EMPTY_LOG_FILTERS, type LogFilters, type LogSort, type SortOrder } from './types';

export interface Aggregates {
  requests: number;
  errors: number;
  cost: number;
  input: number;
  output: number;
  cacheHits: number;
  p50: number | null;
  p95: number | null;
  p99: number | null;
}

/** Rows the table keeps while live; past it the oldest go, and "Load more" with them. */
const LIVE_CAP = 1000;

interface LogState {
  filters: LogFilters;
  sort: LogSort;
  order: SortOrder;
  live: boolean;
  records: Slot<LogRecord[]>;
  cursor: string | null;
  loadingMore: boolean;
  /** New records the live tail sent while the pointer rested on the table, newest first. */
  incoming: LogRecord[];
  /** Whether new records wait in `incoming` instead of joining the table. */
  held: boolean;
  aggregates: Slot<Aggregates>;
  facets: Record<string, FacetValue[]>;
  histogram: Slot<LatencyHistogram>;
  views: Slot<LogView[]>;
  detail: Slot<LogRecord>;
  setFilters: (filters: LogFilters) => void;
  /** A header click: the same column flips its order, another starts descending. */
  setSort: (sort: LogSort) => void;
  setSortOrder: (sort: LogSort, order: SortOrder) => void;
  setLive: (live: boolean) => void;
  /** Records from the live tail: shown at once, or held while the pointer is on the table. */
  receive: (records: LogRecord[]) => void;
  setHeld: (held: boolean) => void;
  /** Shows the held records. */
  release: () => void;
  load: (query: LogQuery, options?: { quiet?: boolean }) => Promise<void>;
  loadMore: (query: LogQuery) => Promise<void>;
  loadSide: (query: LogQuery) => Promise<void>;
  loadViews: () => Promise<void>;
  createView: (body: Parameters<typeof logsApi.createView>[0]) => Promise<LogView>;
  updateView: (id: string, body: LogViewUpdate) => Promise<LogView>;
  deleteView: (id: string) => Promise<void>;
  loadDetail: (id: string) => Promise<void>;
}

function merge(records: LogRecord[], into: LogRecord[]): LogRecord[] {
  const seen = new Set(into.map((r) => r.id));
  const fresh = records.filter((r) => !seen.has(r.id));
  return fresh.length ? [...fresh, ...into] : into;
}

/** The request log: filters, sort, the loaded page with its cursor, aggregates, facets and a detail. */
export const useLogs = create<LogState>()((set, get) => ({
  filters: EMPTY_LOG_FILTERS,
  sort: 'started_at',
  order: 'desc',
  live: true,
  records: emptySlot(),
  cursor: null,
  loadingMore: false,
  incoming: [],
  held: false,
  aggregates: emptySlot(),
  facets: {},
  histogram: emptySlot(),
  views: emptySlot(),
  detail: emptySlot(),

  setFilters(filters) {
    set({ filters });
  },
  setSort(sort) {
    const s = get();
    set({ sort, order: s.sort === sort && s.order === 'desc' ? 'asc' : 'desc' });
  },
  setSortOrder(sort, order) {
    set({ sort, order });
  },
  setLive(live) {
    set(live ? { live } : { live, held: false });
    if (live) get().release();
  },

  receive(records) {
    if (records.length === 0) return;
    const s = get();
    if (s.held) {
      set({ incoming: merge(records, s.incoming).slice(0, LIVE_CAP) });
      return;
    }
    const all = merge(records, s.records.data ?? []);
    set({
      records: { ...s.records, data: all.slice(0, LIVE_CAP), at: Date.now() },
      ...(all.length > LIVE_CAP ? { cursor: null } : {}),
    });
  },
  setHeld(held) {
    set({ held });
    if (!held) get().release();
  },
  release() {
    const s = get();
    if (s.incoming.length === 0) return;
    const current = s.records.data ?? [];
    set({ incoming: [], records: { ...s.records, data: merge(s.incoming, current), at: Date.now() } });
  },

  async load(query, options) {
    let cursor: string | null = null;
    if (!options?.quiet) set({ incoming: [] });
    await loadSlot(
      set,
      get,
      'records',
      async () => {
        const page = await logsApi.list(query);
        cursor = page.next_cursor ?? null;
        return page.items;
      },
      options,
    );
    set({ cursor });
  },

  async loadMore(query) {
    const cursor = get().cursor;
    if (!cursor) return;
    set({ loadingMore: true });
    try {
      const page = await logsApi.list(query, cursor);
      set((s) => ({
        records: { ...s.records, data: [...(s.records.data ?? []), ...page.items] },
        cursor: page.next_cursor ?? null,
      }));
    } catch (error) {
      set((s) => ({ records: { ...s.records, error: asApiError(error) } }));
    } finally {
      set({ loadingMore: false });
    }
  },

  async loadSide(query) {
    const dims = ['provider', 'team', 'model', 'status_class', 'environment'] as const;
    await Promise.all([
      loadSlot(set, get, 'aggregates', () => logsApi.aggregates(query), { quiet: true }),
      loadSlot(set, get, 'histogram', () => logsApi.histogram(query), { quiet: true }),
      get().loadViews(),
      ...dims.map(async (d) => {
        try {
          const values = await logsApi.facets(query, d);
          set((s) => ({ facets: { ...s.facets, [d]: values } }));
        } catch {
          set((s) => ({ facets: { ...s.facets, [d]: [] } }));
        }
      }),
    ]);
  },

  loadViews: () => loadSlot(set, get, 'views', logsApi.views, { quiet: true }),

  async createView(body) {
    const view = await logsApi.createView(body);
    set((s) => ({ views: { ...s.views, data: [...(s.views.data ?? []), view] } }));
    return view;
  },

  async updateView(id, body) {
    const view = await logsApi.updateView(id, body);
    set((s) => ({ views: { ...s.views, data: (s.views.data ?? []).map((v) => (v.id === id ? view : v)) } }));
    return view;
  },

  async deleteView(id) {
    await logsApi.deleteView(id);
    set((s) => ({ views: { ...s.views, data: (s.views.data ?? []).filter((v) => v.id !== id) } }));
  },

  loadDetail: (id) =>
    loadSlot(set, get, 'detail', () =>
      logsApi.get(
        id,
        (get().records.data ?? []).some((r) => r.id === id),
      ),
    ),
}));
