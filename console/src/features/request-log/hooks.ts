'use client';

import { usePathname, useRouter, useSearchParams } from 'next/navigation';
import { useEffect, useEffectEvent, useMemo, useRef, useState } from 'react';

import { usePermission } from '@/components/ui/gate';
import type { LogRecord } from '@/lib/api/types';
import { formatDuration } from '@/lib/format';
import { usePolling } from '@/lib/hooks';
import { decodeFilters, encodeFilters } from '@/lib/url-filters';
import { usePreferences } from '@/stores/preferences';
import { scopeOrganization, useScope } from '@/stores/scope';
import { useQueryWindow, useTimeRange } from '@/stores/time-range';

import { type LogQuery, logsApi } from './api';
import { useLogs } from './store';
import {
  fromUrlState,
  LOG_URL_DEFAULTS,
  type LogFilters,
  type LogSort,
  matchesFilters,
  type SortOrder,
  toUrlState,
} from './types';

/**
 * The query the log reads: the page's window and scope with the log's filters and sort, or with
 * the filters given (the Overview's quick filters).
 */
export function useLogQuery(override?: { filters: LogFilters; sort?: LogSort; order?: SortOrder }): LogQuery {
  const window = useQueryWindow();
  const scope = useScope((s) => s.scope);
  const environment = useScope((s) => s.environment);
  const stored = useLogs((s) => s.filters);
  const storedSort = useLogs((s) => s.sort);
  const storedOrder = useLogs((s) => s.order);
  const filters = override?.filters ?? stored;
  const sort = override ? (override.sort ?? 'started_at') : storedSort;
  const order = override ? (override.order ?? 'desc') : storedOrder;
  return useMemo(
    () => ({
      from: window.from,
      to: window.to,
      filters,
      sort,
      order,
      organization: scopeOrganization(scope),
      tag: environment ? `env:${environment}` : null,
    }),
    [window, filters, sort, order, scope, environment],
  );
}

/** Loads the log on every change of the query, with the facets, aggregates and views when `side`. */
export function useLogData(query: LogQuery, side = true): void {
  const load = useLogs((s) => s.load);
  const loadSide = useLogs((s) => s.loadSide);
  useEffect(() => {
    void load(query);
    if (side) void loadSide(query);
  }, [query, load, loadSide, side]);
}

export type LiveMode = 'stream' | 'poll' | 'off';

/**
 * Brings new records into the table while live: the gateway's live tail when the browser and the
 * gateway allow it, else a poll for records newer than the newest one shown. Only a range that
 * ends now, sorted newest first, takes new rows. Returns how it is getting them.
 */
export function useLiveTail(query: LogQuery, enabled: boolean): LiveMode {
  const receive = useLogs((s) => s.receive);
  const [failed, setFailed] = useState<string | null>(null);
  const filters = query.filters;
  const key = logsApi.tailUrl(query);
  const canStream = typeof EventSource !== 'undefined' && failed !== key;
  const mode: LiveMode = !enabled ? 'off' : canStream ? 'stream' : 'poll';

  const onRecord = useEffectEvent((record: LogRecord) => {
    if (record.started_at >= query.from && matchesFilters(record, filters)) receive([record]);
  });
  useEffect(() => {
    if (mode !== 'stream') return;
    const source = new EventSource(key, { withCredentials: true });
    let opened = false;
    source.onopen = () => {
      opened = true;
    };
    source.addEventListener('log', (e) => {
      try {
        onRecord(JSON.parse((e as MessageEvent<string>).data) as LogRecord);
      } catch {
        // A record that does not parse is skipped; the next full load shows it.
      }
    });
    source.onerror = () => {
      // Refused before it ever opened (an older gateway, too many tails): poll instead. A stream
      // that dropped after opening reconnects by itself.
      if (!opened) {
        source.close();
        setFailed(key);
      }
    };
    return () => source.close();
  }, [mode, key]);

  const poll = async () => {
    const newest = useLogs.getState().records.data?.[0]?.started_at ?? query.from;
    try {
      const page = await logsApi.list({ ...query, from: newest, to: new Date().toISOString() });
      receive(page.items.filter((r) => matchesFilters(r, filters)));
    } catch {
      // The next poll tries again.
    }
  };
  usePolling(() => void poll(), mode === 'poll');
  return mode;
}

/**
 * Keeps the log's filters and sort in the URL: a pasted link sets them, and a change of them
 * rewrites the URL in place.
 */
export function useLogUrlSync(): void {
  const params = useSearchParams();
  const router = useRouter();
  const pathname = usePathname();
  const filters = useLogs((s) => s.filters);
  const sort = useLogs((s) => s.sort);
  const order = useLogs((s) => s.order);
  const fromUrl = useRef<string | null>(null);

  const search = params.toString();
  useEffect(() => {
    const state = fromUrlState(decodeFilters(new URLSearchParams(search), LOG_URL_DEFAULTS));
    const encoded = JSON.stringify(state);
    fromUrl.current = encoded;
    const now = useLogs.getState();
    if (JSON.stringify({ filters: now.filters, sort: now.sort, order: now.order }) === encoded) return;
    useLogs.setState({ filters: state.filters, sort: state.sort, order: state.order });
  }, [search]);

  useEffect(() => {
    // The store as it is now: the effect above may have just set it from the URL.
    const now = useLogs.getState();
    const encoded = JSON.stringify({ filters: now.filters, sort: now.sort, order: now.order });
    if (encoded === fromUrl.current) return;
    const next = encodeFilters(
      new URLSearchParams(window.location.search),
      toUrlState(now.filters, now.sort, now.order),
      LOG_URL_DEFAULTS,
    ).toString();
    if (next === window.location.search.replace(/^\?/, '')) return;
    router.replace(next ? `${pathname}?${next}` : pathname, { scroll: false });
  }, [filters, sort, order, router, pathname]);
}

/** "14 days", "36 hours". */
function retentionLabel(seconds: number): string {
  if (seconds % 86_400 === 0) {
    const d = seconds / 86_400;
    return d === 1 ? '1 day' : `${d} days`;
  }
  if (seconds % 3600 === 0) {
    const h = seconds / 3600;
    return h === 1 ? '1 hour' : `${h} hours`;
  }
  return formatDuration(seconds);
}

/**
 * The store's retention, read from the observability settings by those who may read them, and
 * set as the time picker's limit while the log is open.
 */
export function useRetention(): { seconds: number; label: string } | null {
  const canRead = usePermission('config:read');
  const retention = useTimeRange((s) => s.retention);
  const setRetention = useTimeRange((s) => s.setRetention);
  useEffect(() => {
    if (!canRead) return;
    let live = true;
    logsApi
      .retention()
      .then((seconds) => {
        if (live && seconds) setRetention({ seconds, label: retentionLabel(seconds) });
      })
      .catch(() => undefined);
    return () => {
      live = false;
      setRetention(null);
    };
  }, [canRead, setRetention]);
  return retention;
}

/** The columns the log shows: the user's choice, else the standard set. */
export function useLogColumnsPreference(): [string[] | null, (columns: string[] | null) => void] {
  const columns = usePreferences((s) => s.logColumns);
  const set = usePreferences((s) => s.set);
  return [columns, (logColumns) => set({ logColumns })];
}
