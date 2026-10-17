'use client';

import Link from 'next/link';
import { useMemo, useRef, useState } from 'react';

import { useQueryParam, useScopedHref } from '@/components/layout/url-sync';
import { buttonClass } from '@/components/ui/button';
import { ChipToggle, Input } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { ProviderMark } from '@/components/ui/marks';
import { EmptyAction, EmptyState, ErrorState, LoadingRows, NoMatch } from '@/components/ui/states';
import { Tooltip } from '@/components/ui/tooltip';
import { formatCount } from '@/lib/format';
import { encodeFilters } from '@/lib/url-filters';
import { useProviderType } from '@/stores/provider-types';
import { useResolvedRange, useTimeRange } from '@/stores/time-range';

import { logsApi } from '@/features/request-log/api';
import { LogDrawer } from '@/features/request-log/components/log-drawer';
import { LogTable } from '@/features/request-log/components/log-table';
import { useLiveTail, useLogData, useLogQuery } from '@/features/request-log/hooks';
import { useLogs } from '@/features/request-log/store';
import {
  EMPTY_LOG_FILTERS,
  LOG_URL_DEFAULTS,
  type LogFilters,
  toUrlState,
} from '@/features/request-log/types';

interface Quick {
  q: string;
  provider: string | null;
  errors: boolean;
  cacheHits: boolean;
  flagged: boolean;
}

const NONE: Quick = { q: '', provider: null, errors: false, cacheHits: false, flagged: false };

function toFilters(f: Quick): LogFilters {
  return {
    ...EMPTY_LOG_FILTERS,
    q: f.q,
    provider: f.provider ? [f.provider] : [],
    statusClass: f.errors ? ['4xx', '5xx'] : [],
    cache: f.cacheHits ? ['hit'] : [],
    flagged: f.flagged,
  };
}

const SHOWN = 50;

/**
 * The newest requests with the log's table and drawer, live, and the prototype's quick filters:
 * search, provider chips, errors only, cache hits, flagged, clear all, export.
 */
export function RecentRequests({ providers, firstRun }: { providers: string[]; firstRun: boolean }) {
  const typeOf = useProviderType();
  const [quick, setQuick] = useState<Quick>(NONE);
  const [search, setSearch] = useState('');
  // The search applies as you type, after a pause; Enter applies it at once.
  const typing = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchFor = (text: string) => {
    setSearch(text);
    if (typing.current) clearTimeout(typing.current);
    typing.current = setTimeout(() => setQuick((q) => ({ ...q, q: text.trim() })), 350);
  };
  const filters = useMemo(() => toFilters(quick), [quick]);
  const query = useLogQuery({ filters });
  useLogData(query, false);
  const rangeKind = useTimeRange((s) => s.range.kind);
  const live = useLiveTail(query, rangeKind === 'preset');
  const logs = useLogs((s) => s.records);
  const loadLogs = useLogs((s) => s.load);
  const [selected, setSelected] = useQueryParam('request');
  const range = useResolvedRange();
  const setRange = useTimeRange((s) => s.setRange);
  const logHref = useScopedHref('/request-log/');
  const active =
    quick.q !== '' || quick.provider !== null || quick.errors || quick.cacheHits || quick.flagged;
  const filteredHref = useMemo(() => {
    const [path, search = ''] = logHref.split('?');
    const params = encodeFilters(
      new URLSearchParams(search),
      toUrlState(filters, 'started_at', 'desc'),
      LOG_URL_DEFAULTS,
    );
    const q = params.toString();
    return q ? `${path ?? ''}?${q}` : (path ?? '');
  }, [logHref, filters]);
  const window = range.label.startsWith('Last ') ? range.label.slice(5) : null;
  const set = (patch: Partial<Quick>) => setQuick({ ...quick, ...patch });
  const rows = logs.data?.slice(0, SHOWN) ?? [];

  return (
    <section aria-labelledby="recent-title" className="flex min-h-[320px] flex-1 flex-col bg-raised">
      <div className="flex min-h-11 shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-line px-gutter py-2 max-sm:px-gutter-sm">
        <h2 id="recent-title" className="text-ui font-semibold text-fg">
          Recent requests
        </h2>
        <form
          role="search"
          className="max-sm:order-last max-sm:w-full"
          onSubmit={(e) => {
            e.preventDefault();
            if (typing.current) clearTimeout(typing.current);
            set({ q: search.trim() });
          }}
        >
          <Input
            aria-label="Search recent requests"
            icon="search"
            value={search}
            onChange={(e) => searchFor(e.target.value)}
            placeholder="Search requests"
            className="h-7 w-48 max-sm:w-full"
          />
        </form>
        {providers.length > 0 ? (
          <div
            role="group"
            aria-label="Provider"
            className="flex flex-wrap items-center gap-1.5 max-md:hidden"
          >
            {providers.slice(0, 4).map((p) => (
              <ChipToggle
                key={p}
                pressed={quick.provider === p}
                onChange={(on) => set({ provider: on ? p : null })}
              >
                <ProviderMark type={typeOf(p)} size="xs" />
                {p}
              </ChipToggle>
            ))}
          </div>
        ) : null}
        <div role="group" aria-label="Quick filters" className="flex flex-wrap items-center gap-1.5">
          <ChipToggle pressed={quick.errors} onChange={(errors) => set({ errors })} tone="error">
            Errors only
          </ChipToggle>
          <ChipToggle pressed={quick.cacheHits} onChange={(cacheHits) => set({ cacheHits })} tone="info">
            Cache hits
          </ChipToggle>
          <ChipToggle pressed={quick.flagged} onChange={(flagged) => set({ flagged })} tone="warn">
            Flagged
          </ChipToggle>
          {active ? (
            <button
              type="button"
              onClick={() => {
                if (typing.current) clearTimeout(typing.current);
                setQuick(NONE);
                setSearch('');
              }}
              className="h-6 rounded-xs px-1 text-caption text-muted hover:text-fg"
            >
              Clear all
            </button>
          ) : null}
        </div>
        <span className="ml-auto flex items-center gap-3">
          <Tooltip
            content={
              live === 'stream'
                ? 'New requests stream in as the gateway serves them'
                : live === 'poll'
                  ? 'New requests are checked for every few seconds'
                  : 'This range ends in the past, so no new requests join it'
            }
          >
            <span className="flex items-center gap-1.5 text-caption text-muted tabular-nums">
              {live !== 'off' ? (
                <span aria-hidden="true" className="h-1.5 w-1.5 animate-pulse rounded-full bg-success" />
              ) : null}
              {logs.data ? `${formatCount(rows.length)} shown${live !== 'off' ? ' · live' : ''}` : 'Live'}
            </span>
          </Tooltip>
          <Tooltip content="Download the requests these filters select as CSV">
            <a href={logsApi.exportUrl(query)} className={buttonClass('ghost', 'sm')} download>
              <Icon name="download" size={14} />
              <span className="max-sm:sr-only">Export</span>
            </a>
          </Tooltip>
          <Link href={filteredHref} className="flex items-center gap-1 text-caption text-muted hover:text-fg">
            View all
            <Icon name="arrowRight" size={14} />
          </Link>
        </span>
      </div>
      {logs.error ? (
        <ErrorState
          error={logs.error}
          subject="The request log"
          size="sm"
          onRetry={() => void loadLogs(query)}
        />
      ) : !logs.data ? (
        <LoadingRows rows={4} label="Loading requests" />
      ) : rows.length === 0 ? (
        <>
          <div className="overflow-hidden" aria-hidden="true">
            <LogTable records={[]} selectedId={null} onSelect={() => undefined} />
          </div>
          {active ? (
            <NoMatch
              onClear={() => {
                if (typing.current) clearTimeout(typing.current);
                setQuick(NONE);
                setSearch('');
              }}
            />
          ) : firstRun ? (
            <EmptyState
              size="sm"
              title="No requests yet"
              description="Send a request through the gateway to see it here."
            />
          ) : (
            <EmptyState
              size="sm"
              title={`No requests in the ${(window ?? 'selected range').toLowerCase()}`}
              action={
                range.seconds < 7 * 86_400 ? (
                  <EmptyAction onClick={() => setRange({ kind: 'preset', preset: '7d' })}>
                    Show last 7 days
                  </EmptyAction>
                ) : undefined
              }
            />
          )}
        </>
      ) : (
        <LogTable records={rows} selectedId={selected} onSelect={setSelected} />
      )}
      <LogDrawer id={selected} onClose={() => setSelected(null)} onOpen={setSelected} />
    </section>
  );
}
