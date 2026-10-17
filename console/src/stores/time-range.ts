import { useMemo } from 'react';
import { create } from 'zustand';

import {
  allowedIntervals,
  type CompareValue,
  defaultInterval,
  encodeCompare,
  type Interval,
  PREVIOUS,
  type RangeValue,
  resolveRange,
  type ResolvedRange,
} from '@/lib/time';

/** A limit on how far back a page can look: its length, and how to say it ("14 days"). */
export interface Retention {
  seconds: number;
  label: string;
}

interface TimeRangeState {
  range: RangeValue;
  interval: Interval;
  /** The window deltas compare with. */
  compare: CompareValue;
  /** Bumped by the topbar refresh; queries depend on it to re-run. */
  refreshTick: number;
  /** When the current window was resolved; presets move forward on refresh. */
  anchoredAt: number;
  /** How far back the page's data goes (the request log's retention), or null when unbounded. */
  retention: Retention | null;
  setRetention: (retention: Retention | null) => void;
  setRange: (range: RangeValue) => void;
  setInterval: (interval: Interval) => void;
  setCompare: (compare: CompareValue) => void;
  refresh: () => void;
  /** Opening another page: a preset window ends now again; a custom one stays as it is. */
  pageOpened: () => void;
}

/** The page's single time range (presets, custom, bucket size). It lives in the URL, not storage. */
export const useTimeRange = create<TimeRangeState>()((set, get) => ({
  range: { kind: 'preset', preset: '6h' },
  interval: '5m',
  compare: PREVIOUS,
  refreshTick: 0,
  anchoredAt: Date.now(),
  retention: null,
  setRetention(retention) {
    set({ retention });
  },
  setRange(range) {
    set({ range, interval: defaultInterval(range), anchoredAt: Date.now() });
  },
  setInterval(interval) {
    const resolved = resolveRange(get().range, new Date(get().anchoredAt));
    if (allowedIntervals(resolved.seconds).includes(interval)) set({ interval });
  },
  setCompare(compare) {
    set({ compare });
  },
  refresh() {
    set((s) => ({ refreshTick: s.refreshTick + 1, anchoredAt: Date.now() }));
  },
  pageOpened() {
    if (get().range.kind === 'preset') set({ anchoredAt: Date.now() });
  },
}));

/** The resolved `[from, to)` of the current range, anchored at the last refresh. */
export function useResolvedRange(): ResolvedRange {
  const range = useTimeRange((s) => s.range);
  const anchoredAt = useTimeRange((s) => s.anchoredAt);
  const compare = useTimeRange((s) => s.compare);
  return useMemo(() => resolveRange(range, new Date(anchoredAt), compare), [range, anchoredAt, compare]);
}

export interface QueryWindow {
  from: string;
  to: string;
  interval: Interval;
  /** Changes whenever any query of the page must run again. */
  key: string;
}

/** The window every analytics request of a page uses: the same `from`, `to` and bucket. */
export function useQueryWindow(): QueryWindow {
  const resolved = useResolvedRange();
  const interval = useTimeRange((s) => s.interval);
  const tick = useTimeRange((s) => s.refreshTick);
  const compare = useTimeRange((s) => s.compare);
  return useMemo(() => {
    const from = resolved.from.toISOString();
    const to = resolved.to.toISOString();
    const cmp = encodeCompare(compare) ?? 'previous';
    return { from, to, interval, key: `${from}|${to}|${interval}|${tick}|${cmp}` };
  }, [resolved, interval, tick, compare]);
}
