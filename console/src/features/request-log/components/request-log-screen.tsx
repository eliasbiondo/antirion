'use client';

import { useEffect, useMemo, useRef, useState } from 'react';

import { useQueryParam, useScopedHref } from '@/components/layout/url-sync';
import { Button, buttonClass } from '@/components/ui/button';
import { Input } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { PageHeader, SummaryStrip } from '@/components/ui/layout';
import { MenuButton } from '@/components/ui/menu';
import { UpdatedAt } from '@/components/ui/misc';
import { Drawer } from '@/components/ui/overlay';
import {
  EmptyAction,
  EmptyState,
  ErrorState,
  isUnavailable,
  LoadingRows,
  NoMatch,
} from '@/components/ui/states';
import { describeError } from '@/lib/api/client';
import { cn } from '@/lib/cn';
import { exportFileName } from '@/lib/csv';
import { formatCount, formatLatency, formatMoney, formatPercent, plural } from '@/lib/format';
import { useMediaQuery, useNow } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { formatDate } from '@/lib/time';
import { useShell } from '@/stores/shell';
import { notify } from '@/stores/ui';
import { KeyboardHints } from '@/components/ui/keyboard-hints';
import { usePreferences } from '@/stores/preferences';
import { useResolvedRange, useTimeRange } from '@/stores/time-range';

import { curlSnippet, useSetup } from '@/features/overview/setup';

import { type LogQuery, logsApi, unsupportedParams } from '../api';
import { resolveColumns } from '../columns';
import {
  type LiveMode,
  useLiveTail,
  useLogColumnsPreference,
  useLogData,
  useLogQuery,
  useLogUrlSync,
  useRetention,
} from '../hooks';
import { useLogs } from '../store';
import { activeFilterCount, applyLocally, EMPTY_LOG_FILTERS, filterChips } from '../types';
import { ColumnChooser } from './column-chooser';
import { FiltersRail } from './filters-rail';
import { LogDrawer } from './log-drawer';
import { RequestLogMoreMenu } from './erase-end-user';
import { LogTable } from './log-table';
import { Tooltip } from '@/components/ui/tooltip';

/**
 * The figures over the table, for the whole filtered range (from usage analytics): hidden while
 * they load, when the gateway does not serve them, and when the range has no requests.
 */
function Summary() {
  const agg = useLogs((s) => s.aggregates);
  const a = agg.data;
  if (!a || a.requests === 0 || agg.error) return null;
  return (
    <SummaryStrip
      label="Requests in the range"
      items={[
        { label: 'Requests', value: formatCount(a.requests) },
        {
          label: 'Error rate',
          value: formatPercent(a.errors / a.requests, 2),
          tone: a.errors / a.requests > 0.05 ? 'error' : 'neutral',
          title: `${formatCount(a.errors)} failed`,
        },
        { label: 'p95 latency', value: formatLatency(a.p95), title: `p50 ${formatLatency(a.p50)}` },
        {
          label: 'Spend',
          value: formatMoney(a.cost),
          title: `${formatMoney(a.cost / a.requests)} per request`,
        },
        {
          label: 'Cache hit rate',
          value: formatPercent(a.cacheHits / a.requests, 1),
          title: `${formatCount(a.cacheHits)} cached responses`,
        },
      ]}
    />
  );
}

/**
 * The live-updates switch: a labelled toggle with its state in words. Live needs the newest
 * requests first and a range that ends now; otherwise it says why it is off.
 */
function LiveToggle({ mode, blocked }: { mode: LiveMode; blocked: string | null }) {
  const live = useLogs((s) => s.live);
  const setLive = useLogs((s) => s.setLive);
  const on = live && !blocked;
  const tip = blocked
    ? blocked
    : !live
      ? 'Resume live updates'
      : mode === 'stream'
        ? 'New requests stream in as the gateway serves them. Pause live updates'
        : 'New requests are checked for every few seconds. Pause live updates';
  return (
    <Tooltip content={tip} describe={false}>
      <span className="inline-flex [&>button:disabled]:pointer-events-none">
        <button
          type="button"
          aria-pressed={on}
          aria-label={
            blocked ? `Live updates are off. ${blocked}` : live ? 'Pause live updates' : 'Resume live updates'
          }
          disabled={Boolean(blocked)}
          onClick={() => setLive(!live)}
          className={cn(buttonClass('secondary', 'md'), on && 'text-success', 'disabled:opacity-60')}
        >
          {on ? (
            <span aria-hidden="true" className="h-2 w-2 animate-pulse rounded-full bg-success" />
          ) : (
            <Icon name="pause" size={14} className="text-muted" />
          )}
          <span className={on ? 'text-fg' : 'text-muted'}>{on ? 'Live' : 'Paused'}</span>
        </button>
      </span>
    </Tooltip>
  );
}

/**
 * `requests-4xx-gpt-4o-2026-10-09-to-2026-10-16.csv`: the export's name says which filters and
 * days it holds.
 */
export function exportName(query: Pick<LogQuery, 'from' | 'to'>, format: 'csv' | 'ndjson'): string {
  return exportFileName('requests', format, { from: query.from, to: query.to });
}

/**
 * Downloads the requests the filters select, as CSV or NDJSON. It is read here and saved under the
 * console's name, since the gateway's own file name says nothing about the filters.
 */
function downloadLog(query: LogQuery, format: 'csv' | 'ndjson') {
  const name = exportName(query, format);
  void fetch(logsApi.exportUrl(query, format), { credentials: 'same-origin' })
    .then(async (response) => {
      if (!response.ok) throw new Error(`The export failed (${String(response.status)})`);
      const url = URL.createObjectURL(await response.blob());
      const a = document.createElement('a');
      a.href = url;
      a.download = name;
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    })
    .catch((e: unknown) => notify('error', "Couldn't export the requests", describeError(e)));
}

function ExportMenu({ query }: { query: LogQuery }) {
  const download = (format: 'csv' | 'ndjson') => downloadLog(query, format);
  return (
    <MenuButton
      label="Export"
      items={[
        { label: 'CSV, for spreadsheets', icon: 'download', onSelect: () => download('csv') },
        { label: 'NDJSON, one record per line', icon: 'code', onSelect: () => download('ndjson') },
      ]}
    />
  );
}

/** A pill over the table while new requests wait for the pointer to leave it. */
function NewRequests({ onShow }: { onShow: () => void }) {
  const count = useLogs((s) => s.incoming.length);
  if (count === 0) return null;
  return (
    <div className="pointer-events-none sticky top-2 z-10 flex h-0 justify-center">
      <button
        type="button"
        onClick={onShow}
        className="pointer-events-auto inline-flex h-7 items-center gap-1.5 rounded-full border border-[color-mix(in_oklab,var(--ant-accent)_50%,transparent)] bg-raised px-3 text-caption text-fg shadow-pop hover:border-accent"
      >
        <Icon name="arrowUp" size={12} className="text-accent" />
        {plural(count, 'new request')}
      </button>
    </div>
  );
}

/** Copies the first request's curl command, and says so for a moment. */
function CopySampleAction({ snippet }: { snippet: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <EmptyAction
      onClick={() => {
        void navigator.clipboard.writeText(snippet).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        });
      }}
    >
      {copied ? 'Copied' : 'Copy a sample request'}
    </EmptyAction>
  );
}

/** The empty log: first run teaches the first request; otherwise it offers a wider window. */
function NoRequests() {
  const setup = useSetup();
  const range = useResolvedRange();
  const setRange = useTimeRange((s) => s.setRange);
  const dataPlaneUrl = useShell((s) => s.status?.data_plane_url ?? null);
  const keysHref = useScopedHref('/keys/');
  if (!setup.loaded) return <LoadingRows rows={3} label="Loading requests" />;
  if (setup.hasRequest === false) {
    const snippet = curlSnippet(dataPlaneUrl, setup.modelName);
    return (
      <EmptyState
        title="No requests yet"
        description="Send a request through the gateway to see it here."
        action={
          setup.hasKey === false ? <EmptyAction href={keysHref}>Create an API key</EmptyAction> : undefined
        }
        secondaryAction={<CopySampleAction snippet={snippet} />}
      />
    );
  }
  const wider = range.seconds < 86_400 ? '24h' : range.seconds < 7 * 86_400 ? '7d' : null;
  return (
    <EmptyState
      title={
        /^last /i.test(range.label)
          ? `No requests in the ${range.label.toLowerCase()}`
          : 'No requests in this range'
      }
      action={
        wider ? (
          <EmptyAction onClick={() => setRange({ kind: 'preset', preset: wider })}>
            {wider === '24h' ? 'Show last 24 hours' : 'Show last 7 days'}
          </EmptyAction>
        ) : undefined
      }
    />
  );
}

/**
 * The request log: a filter rail with starred views, the range's figures, active filters as
 * chips, the table with live updates and paging, and the request drawer.
 */
export function RequestLogScreen() {
  useLogUrlSync();
  const query = useLogQuery();
  useLogData(query);
  const retention = useRetention();
  const [columnChoice, setColumns] = useLogColumnsPreference();
  const columns = resolveColumns(columnChoice);
  const live = useLogs((s) => s.live);
  const setHeld = useLogs((s) => s.setHeld);
  const release = useLogs((s) => s.release);
  const rangeKind = useTimeRange((s) => s.range.kind);
  const timeZone = usePreferences((s) => s.timeZone);
  const records = useLogs((s) => s.records);
  const cursor = useLogs((s) => s.cursor);
  const loadingMore = useLogs((s) => s.loadingMore);
  const loadMore = useLogs((s) => s.loadMore);
  const load = useLogs((s) => s.load);
  const filters = useLogs((s) => s.filters);
  const setFilters = useLogs((s) => s.setFilters);
  const sort = useLogs((s) => s.sort);
  const order = useLogs((s) => s.order);
  const setSort = useLogs((s) => s.setSort);
  const aggregates = useLogs((s) => s.aggregates.data);
  const range = useResolvedRange();
  const [selected, setSelected] = useQueryParam('request');
  const liveBlocked =
    rangeKind === 'custom'
      ? 'Live updates follow the newest requests; pick a range that ends now.'
      : sort !== 'started_at' || order !== 'desc'
        ? 'Live updates need the newest requests first; sort by time to turn them on.'
        : null;
  const liveMode = useLiveTail(query, live && !liveBlocked);
  const tableRef = useRef<HTMLDivElement>(null);
  /** The rail sits beside the table from 1440px; narrower, it opens on demand. */
  const wide = !useMediaQuery('(max-width: 1439px)');
  const inline = !useMediaQuery('(max-width: 1023px)');
  const [railChoice, setRailChoiceOnly] = useState<boolean | null>(null);
  const railPreference = usePreferences((s) => s.filterRail);
  const setPreferences = usePreferences((s) => s.set);
  const showRail = railChoice ?? (wide && railPreference);
  /** Where the rail sits beside the table, opening or closing it is remembered for next time. */
  const setRailChoice = (open: boolean) => {
    setRailChoiceOnly(open);
    if (wide) setPreferences({ filterRail: open });
  };
  const [search, setSearch] = useState(filters.q);
  // A link or a view that changes the search shows it in the box.
  const [shownQ, setShownQ] = useState(filters.q);
  if (shownQ !== filters.q) {
    setShownQ(filters.q);
    setSearch(filters.q);
  }

  usePageShortcut('f', () => setRailChoice(!showRail));
  usePageShortcut('e', () => downloadLog(query, 'csv'));

  useEffect(() => () => useLogs.getState().setHeld(false), []);
  const showNew = () => {
    release();
    tableRef.current?.scrollIntoView({ block: 'start', behavior: 'smooth' });
  };
  const now = useNow(60_000);
  const kept = retention ? { label: retention.label, since: new Date(now - retention.seconds * 1000) } : null;
  const beyondRetention = kept !== null && range.from.getTime() < kept.since.getTime() - 60_000;

  const rows = useMemo(
    () => applyLocally(records.data ?? [], filters, unsupportedParams(), sort, order),
    [records.data, filters, sort, order],
  );
  const chips = filterChips(filters);
  const active = activeFilterCount(filters);
  const clearAll = () => {
    setFilters(EMPTY_LOG_FILTERS);
    setSearch('');
  };
  const total = aggregates?.requests ?? null;

  const rail = <FiltersRail />;

  return (
    <div className="flex min-h-[calc(100vh-48px)]">
      {showRail && inline ? (
        <aside
          aria-label="Filters"
          className="sticky top-0 flex h-(--pane-h) w-[248px] shrink-0 flex-col overflow-y-auto border-r border-line bg-raised"
        >
          <div className="flex h-11 shrink-0 items-center justify-between border-b border-line px-4">
            <h2 className="text-ui font-semibold text-fg">Filters</h2>
            <button
              type="button"
              className="rounded-xs text-caption text-muted hover:text-fg"
              onClick={() => setRailChoice(false)}
            >
              Hide filters
            </button>
          </div>
          {rail}
        </aside>
      ) : null}
      {!inline ? (
        <Drawer open={showRail} onClose={() => setRailChoice(false)} title="Filters">
          <div className="-mx-5 -my-4">{rail}</div>
        </Drawer>
      ) : null}
      <div className="flex min-w-0 flex-1 flex-col">
        <PageHeader
          title="Request log"
          description="Every request through the gateway, newest first"
          context={range.label}
          actions={
            <>
              <span className="max-sm:hidden">
                <UpdatedAt at={records.at} />
              </span>
              <LiveToggle mode={liveMode} blocked={liveBlocked} />
              <ExportMenu query={query} />
              <RequestLogMoreMenu />
            </>
          }
        />
        <Summary />
        <div className="flex flex-wrap items-center gap-2 border-b border-line bg-canvas px-gutter py-2 max-sm:px-gutter-sm">
          {showRail && inline ? null : (
            <Button icon="filter" onClick={() => setRailChoice(true)}>
              Filters
              {active > 0 ? <span className="text-accent tabular-nums">{active}</span> : null}
            </Button>
          )}
          <form
            role="search"
            className="max-sm:flex-1"
            onSubmit={(e) => {
              e.preventDefault();
              setFilters({ ...filters, q: search.trim() });
            }}
          >
            <Input
              data-page-search
              aria-label="Search requests"
              icon="search"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Search requests"
              className="w-64 max-sm:w-full"
            />
          </form>
          {chips.length > 0 ? (
            <div role="group" aria-label="Active filters" className="flex flex-wrap items-center gap-2">
              {chips.map((c) => (
                <button
                  key={c.label}
                  type="button"
                  onClick={() => {
                    setFilters(c.without);
                    if (c.label === 'Search') setSearch('');
                  }}
                  aria-label={`Remove filter ${c.label}: ${c.value}`}
                  className="inline-flex h-7 max-w-80 items-center gap-1.5 rounded-full border border-[color-mix(in_oklab,var(--ant-accent)_40%,transparent)] bg-accent-dim px-2.5 text-caption text-fg hover:border-accent"
                >
                  <span className="text-muted">{c.label}:</span>
                  <span className="truncate">{c.value}</span>
                  <Icon name="x" size={12} className="shrink-0 text-muted" />
                </button>
              ))}
              <button
                type="button"
                onClick={clearAll}
                className="h-7 rounded-xs px-1 text-caption text-muted hover:text-fg"
              >
                Clear all
              </button>
            </div>
          ) : null}
          <span className="ml-auto flex items-center gap-3">
            {records.data && chips.length > 0 ? (
              <span className="text-caption text-muted tabular-nums" aria-live="polite">
                {total !== null
                  ? plural(total, 'matching request')
                  : `${formatCount(rows.length)} of ${plural(records.data.length, 'loaded request')}`}
              </span>
            ) : null}
            <ColumnChooser columns={columns} onChange={setColumns} />
          </span>
        </div>
        {beyondRetention ? (
          <p className="border-b border-line bg-canvas px-gutter py-2 text-caption text-muted max-sm:px-gutter-sm">
            <Icon name="info" size={12} className="mr-1.5 inline align-[-1px]" />
            Requests are kept for {kept.label}, so nothing before {formatDate(kept.since, timeZone)} is left
            to show.
          </p>
        ) : null}
        <div className="flex min-h-0 flex-1 flex-col bg-raised">
          {records.status === 'loading' && !records.data ? (
            <LoadingRows rows={10} label="Loading requests" />
          ) : null}
          {records.error && !records.data ? (
            isUnavailable(records.error) ? (
              <ErrorState error={records.error} subject="The request log" />
            ) : (
              <ErrorState error={records.error} subject="The request log" onRetry={() => void load(query)} />
            )
          ) : null}
          {records.data && rows.length === 0 ? (
            <>
              <div aria-hidden="true" className="overflow-hidden max-sm:hidden">
                <LogTable records={[]} selectedId={null} onSelect={() => undefined} />
              </div>
              {active > 0 ? <NoMatch onClear={clearAll} /> : <NoRequests />}
            </>
          ) : null}
          {rows.length > 0 ? (
            <>
              <div
                ref={tableRef}
                className="relative scroll-mt-12"
                onMouseEnter={() => {
                  if (live && !liveBlocked) setHeld(true);
                }}
                onMouseLeave={() => setHeld(false)}
              >
                <NewRequests onShow={showNew} />
                <LogTable
                  records={rows}
                  selectedId={selected}
                  onSelect={setSelected}
                  sort={sort}
                  order={order}
                  onSort={setSort}
                  columns={columns}
                />
              </div>
              <div className="flex items-center justify-center gap-3 px-gutter py-4">
                {cursor ? (
                  <>
                    <span className="text-caption text-muted tabular-nums">
                      Showing {formatCount(rows.length)}
                      {total !== null ? ` of ${formatCount(total)}` : ''}
                    </span>
                    <Button busy={loadingMore} onClick={() => void loadMore(query)}>
                      Load more
                    </Button>
                  </>
                ) : (
                  <span className="text-caption text-muted tabular-nums">
                    {rows.length === 1
                      ? 'Showing the only request'
                      : `Showing all ${formatCount(rows.length)} requests`}
                  </span>
                )}
                {retention ? (
                  <span className="text-caption text-dim">· Requests are kept for {retention.label}</span>
                ) : null}
              </div>
              <KeyboardHints keys={['search', 'move', 'open', 'close', 'filters', 'export']} />
            </>
          ) : null}
        </div>
      </div>
      <LogDrawer id={selected} onClose={() => setSelected(null)} onOpen={setSelected} />
    </div>
  );
}
