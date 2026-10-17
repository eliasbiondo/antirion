'use client';

import { useRouter } from 'next/navigation';
import { useEffect, useMemo, useState } from 'react';

import { changeRequestHref } from '@/components/layout/change-control';
import { useQueryParam } from '@/components/layout/url-sync';
import { Button } from '@/components/ui/button';
import { Legend, TimeChart, type LegendItem } from '@/components/ui/charts';
import { useCan } from '@/components/ui/gate';
import { KpiStrip, KpiTile, type KpiState } from '@/components/ui/kpi';
import { PageHeader, Panel, Tabs } from '@/components/ui/layout';
import { UpdatedAt } from '@/components/ui/misc';
import { Banner, EmptyState, ErrorState, isUnavailable, UnavailableState } from '@/components/ui/states';
import { downloadCsv } from '@/lib/csv';
import { formatBytes, formatCount, formatDelta, formatMoney, formatPercent, plural } from '@/lib/format';
import { usePolling } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { cacheOf, keyStrategyOf, modesOf } from '../policy';
import { totalOf, useCache } from '../store';
import { ActivityTab } from './activity-tab';
import { ClearDialog } from './clear-dialog';
import { EntriesTab } from './entries-tab';
import { ModelsTab } from './models-tab';
import { RulesTab } from './rules-tab';
import { StatsTab } from './stats-tab';

type Tab = 'models' | 'entries' | 'rules' | 'stats' | 'activity';
const TABS: Tab[] = ['models', 'entries', 'rules', 'stats', 'activity'];
const LEGEND: LegendItem[] = [
  { label: 'Answered from the cache', tone: 'accent' },
  { label: 'Sent to a provider', tone: 'muted' },
  { label: 'Not sent to a provider', tone: 'warn' },
  { label: 'Saved', tone: 'success' },
];

/** The response cache: what it saves, caching per model, cached responses, rules, statistics, activity. */
export function CacheScreen() {
  const router = useRouter();
  const status = useCache((s) => s.status);
  const models = useCache((s) => s.models);
  const series = useCache((s) => s.series);
  const byModel = useCache((s) => s.byModel);
  const rules = useCache((s) => s.rules);
  const flush = useCache((s) => s.flush);
  const load = useCache((s) => s.load);
  const loadSeries = useCache((s) => s.loadSeries);
  const loadModelFacts = useCache((s) => s.loadModelFacts);
  const loadStats = useCache((s) => s.loadStats);
  const loadActivity = useCache((s) => s.loadActivity);
  const loadFlush = useCache((s) => s.loadFlush);
  const canClear = useCan('invalidateCache');
  const timeZone = usePreferences((s) => s.timeZone);
  const [tabParam, setTabParam] = useQueryParam('tab');
  const tab: Tab = TABS.includes(tabParam as Tab) ? (tabParam as Tab) : 'models';
  const [clearing, setClearing] = useState(false);

  useEffect(() => {
    void load();
    void loadSeries();
    void loadModelFacts();
    void loadFlush();
  }, [load, loadSeries, loadModelFacts, loadFlush]);
  useEffect(() => {
    if (tab === 'stats') void loadStats();
    if (tab === 'activity') void loadActivity();
  }, [tab, loadStats, loadActivity]);
  usePolling(() => void load({ quiet: true }));

  const list = useMemo(() => models.data ?? [], [models.data]);
  const enabled = list.filter((m) => modesOf(cacheOf(m)).exact);
  const prompt = list.filter((m) => modesOf(cacheOf(m)).prompt);
  const s = status.data;
  const unhealthy = (status.error?.status === 503 || (s && !s.healthy)) === true;

  // The last 24 hours and the 24 before, from 48 hourly buckets.
  const all = series.data ?? [];
  const buckets = all.slice(-24);
  const before = all.slice(0, Math.max(0, all.length - 24));
  const cur = totalOf(buckets);
  const prev = totalOf(before);
  const rate = cur.requests > 0 ? cur.hits / cur.requests : 0;
  const prevRate = prev.requests > 0 ? prev.hits / prev.requests : null;

  const seriesState: KpiState = series.error
    ? isUnavailable(series.error)
      ? 'unavailable'
      : 'empty'
    : !series.data
      ? 'loading'
      : cur.requests === 0
        ? 'empty'
        : 'ready';
  const statusState: KpiState = unhealthy
    ? 'ready'
    : status.error
      ? isUnavailable(status.error)
        ? 'unavailable'
        : 'empty'
      : !s
        ? 'loading'
        : 'ready';

  const exportModels = () =>
    downloadCsv(
      'cache-per-model.csv',
      [
        'model',
        'enabled',
        'semantic',
        'prompt_caching',
        'ttl',
        'key_strategy',
        'max_entry_size',
        'max_entries',
        'read_only',
        'fail_mode',
        'requests_30d',
        'hits_30d',
        'saved_usd_30d',
        'cost_usd_30d',
      ],
      list.map((m) => {
        const c = cacheOf(m);
        const md = modesOf(c);
        const u = byModel.data?.get(m.name);
        return [
          m.name,
          String(md.exact),
          String(md.semantic),
          String(md.prompt),
          c.ttl ?? '',
          keyStrategyOf(c).label,
          c.max_entry_size ?? '',
          c.max_entries ?? '',
          String(c.read_only === true),
          c.fail_mode ?? 'open',
          u?.requests,
          u?.hits,
          u?.saved.toFixed(2),
          u?.cost.toFixed(2),
        ];
      }),
    );
  usePageShortcut('e', exportModels);

  const pending = flush.data?.pending ?? [];

  return (
    <>
      <PageHeader
        title="Cache"
        description="Answer repeated requests without calling a provider"
        actions={
          <div className="flex flex-wrap items-center gap-2 max-sm:min-w-[calc(100vw-32px)]">
            <UpdatedAt at={status.at} />
            {list.length > 0 ? (
              <Button variant="ghost" icon="download" onClick={exportModels}>
                Export CSV
              </Button>
            ) : null}
          </div>
        }
      />
      <KpiStrip columns={5} label="Cache figures">
        <KpiTile
          label="Saved, last 24 h"
          icon="dollar"
          state={seriesState}
          emptyHint="After the first cached answer"
          value={formatMoney(cur.saved)}
          valueTone="success"
          delta={
            prev.saved > 0
              ? {
                  text: formatDelta(cur.saved, prev.saved),
                  tone: cur.saved >= prev.saved ? 'good' : 'bad',
                  title: 'Against the 24 hours before',
                }
              : null
          }
          context={`About ${formatMoney(cur.saved * 30, { compact: true })} a month at this rate`}
          spark={{ values: buckets.map((b) => b.saved), tone: 'success' }}
        />
        <KpiTile
          label="Hit rate, last 24 h"
          icon="cache"
          state={seriesState}
          emptyHint="No requests yet"
          value={formatPercent(rate, 1)}
          delta={
            prevRate !== null
              ? {
                  text: formatDelta(rate, prevRate, 'points'),
                  tone: Math.abs(rate - prevRate) < 0.0005 ? 'neutral' : rate > prevRate ? 'good' : 'bad',
                  title: 'Against the 24 hours before',
                }
              : null
          }
          context={`${formatCount(cur.hits, { compact: true })} of ${formatCount(cur.requests, { compact: true })} cached`}
          spark={{
            values: buckets.map((b) => (b.requests > 0 ? b.hits / b.requests : null)),
            tone: 'accent',
          }}
        />
        <KpiTile
          label="Paid to providers, last 24 h"
          icon="wallet"
          state={seriesState}
          emptyHint="No requests yet"
          value={formatMoney(cur.cost)}
          context={`${formatMoney(cur.cost + cur.saved)} without the cache`}
          spark={{ values: buckets.map((b) => b.cost), tone: 'muted' }}
        />
        <KpiTile
          label="Cached responses"
          icon="list"
          state={statusState}
          value={
            unhealthy
              ? 'Unhealthy'
              : s?.entries === null || s?.entries === undefined
                ? 'Not reported'
                : formatCount(s.entries, { compact: true })
          }
          valueTone={unhealthy ? 'error' : 'neutral'}
          context={
            unhealthy
              ? 'The storage isn’t answering'
              : s
                ? `${s.bytes !== null ? `${formatBytes(s.bytes)} · ` : ''}${s.backend === 'memory' ? 'In memory' : s.backend}`
                : undefined
          }
          spark={null}
        />
        <KpiTile
          className="max-md:col-span-2"
          label="Models with caching on"
          icon="cube"
          state={!models.data ? 'loading' : list.length === 0 ? 'empty' : 'ready'}
          emptyHint="No models yet"
          value={`${enabled.length} of ${list.length}`}
          context={
            prompt.length > 0
              ? `${plural(prompt.length, 'model')} with provider prompt caching`
              : enabled.length === list.length
                ? 'Every model'
                : `${plural(list.length - enabled.length, 'model')} off`
          }
          spark={null}
        />
      </KpiStrip>
      {unhealthy ? (
        <Banner tone="error" title="The cache storage isn’t answering">
          Requests go to providers without the cache, and models set to refuse requests when the cache is down
          refuse them, until it’s back.
        </Banner>
      ) : null}
      {pending.map((r) => (
        <Banner
          key={r.id}
          tone="info"
          title="Clearing the whole cache is waiting for approval"
          actions={
            <Button size="sm" variant="secondary" onClick={() => router.push(changeRequestHref(r.id))}>
              View request
            </Button>
          }
        >
          Requested by {r.author} {formatRelative(r.created_at)}. The cache is cleared once a second person
          approves it.
        </Banner>
      ))}
      <Panel
        title="Requests and cache hits"
        subtitle="Last 24 hours, per hour"
        className="border-b border-line"
        actions={seriesState === 'ready' ? <Legend className="max-sm:hidden" items={LEGEND} /> : null}
      >
        {seriesState === 'unavailable' ? (
          <UnavailableState subject="Cache history" size="sm" />
        ) : series.error ? (
          <ErrorState
            error={series.error}
            subject="Cache history"
            size="sm"
            onRetry={() => void loadSeries()}
          />
        ) : seriesState === 'empty' ? (
          <EmptyState size="sm" title="No requests in the last 24 hours" />
        ) : (
          <>
            <Legend className="mb-2 sm:hidden" items={LEGEND} />
            <TimeChart
              kind="stacked-bars"
              title="Requests answered from the cache and sent to a provider, and dollars saved, per hour, over 24 hours"
              labels={buckets.map((b) =>
                new Date(b.start).toLocaleTimeString('en-GB', {
                  hour: '2-digit',
                  minute: '2-digit',
                  timeZone,
                }),
              )}
              series={[
                { name: 'Answered from the cache', values: buckets.map((b) => b.hits), tone: 'accent' },
                {
                  name: 'Sent to a provider',
                  values: buckets.map((b) => b.sent),
                  tone: 'muted',
                },
                {
                  name: 'Not sent to a provider',
                  values: buckets.map((b) => Math.max(0, b.requests - b.hits - b.sent)),
                  tone: 'warn',
                },
              ]}
              overlay={{
                name: 'Saved',
                values: buckets.map((b) => b.saved),
                tone: 'success',
                format: (v) => formatMoney(v, { compact: v >= 100 }),
              }}
              emptyLabel="No requests in the last 24 hours"
              height={170}
            />
          </>
        )}
      </Panel>
      <Tabs<Tab>
        label="Cache views"
        value={tab}
        onChange={(t) => setTabParam(t === 'models' ? null : t)}
        items={[
          { value: 'models', label: 'Models', count: list.length, icon: 'cube' },
          {
            value: 'entries',
            label: 'Cached responses',
            icon: 'list',
            ...(s?.entries != null ? { count: s.entries } : {}),
          },
          {
            value: 'rules',
            label: 'Clearing rules',
            icon: 'refresh',
            ...(rules.data ? { count: rules.data.length } : {}),
          },
          { value: 'stats', label: 'Statistics', icon: 'chart' },
          { value: 'activity', label: 'Activity', icon: 'history' },
        ]}
      />
      {tab === 'models' ? <ModelsTab /> : null}
      {tab === 'entries' ? <EntriesTab /> : null}
      {tab === 'rules' ? <RulesTab /> : null}
      {tab === 'stats' ? <StatsTab /> : null}
      {tab === 'activity' ? <ActivityTab /> : null}
      {canClear ? (
        <section aria-labelledby="cache-danger" className="border-b border-line bg-raised">
          <div className="flex flex-wrap items-center gap-4 px-5 py-4 max-sm:px-gutter-sm">
            <div className="min-w-0 flex-1">
              <h2 id="cache-danger" className="text-ui font-semibold text-error">
                Clear cache
              </h2>
              <p className="mt-0.5 text-ui text-muted">
                Remove every cached response for the gateway, an organization, a team, a project, a route or a
                model. Requests go to providers until the cache fills again.
                {flush.data?.dualControl
                  ? ' Clearing the whole gateway needs a second person’s approval.'
                  : ''}
              </p>
            </div>
            <Button variant="danger-solid" onClick={() => setClearing(true)}>
              Clear cache…
            </Button>
          </div>
        </section>
      ) : null}
      <ClearDialog open={clearing} onClose={() => setClearing(false)} />
    </>
  );
}
