'use client';

import Link from 'next/link';
import { useMemo, type ReactNode } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Button } from '@/components/ui/button';
import { Legend, TimeChart } from '@/components/ui/charts';
import { KpiStrip, KpiTile, type KpiDelta, type KpiState } from '@/components/ui/kpi';
import { Panel, PageHeader, TileGrid } from '@/components/ui/layout';
import { UpdatedAt } from '@/components/ui/misc';
import { Tooltip } from '@/components/ui/tooltip';
import { Banner, isUnavailable } from '@/components/ui/states';
import type { UsageBucket } from '@/lib/api/types';
import { formatCount, formatDelta, formatMoney, formatTokens } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { compareTitle, formatClock, INTERVALS } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { scopeOrganization, useScope } from '@/stores/scope';
import { useResolvedRange, useTimeRange } from '@/stores/time-range';

import { useUsage, useUsageQueries } from '@/features/analytics/hooks';
import { groupTotals, totals, useAnalytics } from '@/features/analytics/store';
import { useLogs } from '@/features/request-log/store';

import { monthElapsed, useOverviewRuntime, useRuntimeReadings } from '../runtime';
import { useSetup } from '../setup';
import { GetStarted } from './get-started';
import { ProviderHeadroom, TeamSpend, TopModels } from './leaderboards';
import { RecentRequests } from './recent-requests';
import { monthProjection } from '@/features/budgets/projection';

function bucketLabel(b: UsageBucket, timeZone: string, long: boolean): string {
  return long
    ? new Date(b.start).toLocaleDateString('en-US', { month: 'short', day: 'numeric', timeZone })
    : formatClock(b.start, timeZone).slice(0, 5);
}

/** A change against the previous window, coloured by whether a rise is bad, good or neither. */
function delta(text: string | null, rise: 'bad' | 'good' | 'neutral', title: string): KpiDelta | null {
  if (!text || text === '—' || text === 'new') return null;
  const spaced = text.replace(/pp$/, ' pp');
  const zero = /^[+−]0(\.0+)?( pp|%)$/.test(spaced) || spaced === '0%';
  const up = spaced.startsWith('+');
  const tone: KpiDelta['tone'] =
    zero || rise === 'neutral'
      ? 'neutral'
      : (up && rise === 'bad') || (!up && rise === 'good')
        ? 'bad'
        : 'good';
  return { text: zero ? 'no change' : spaced, tone, title };
}

/** "$2.17" + "K": money with its thousands suffix set apart, as the prototype draws it. */
function moneyParts(value: number): { value: string; unit?: string } {
  if (value >= 1e6) return { value: `$${(value / 1e6).toFixed(2)}`, unit: 'M' };
  if (value >= 1e4) return { value: `$${(value / 1e3).toFixed(2)}`, unit: 'K' };
  return { value: formatMoney(value) };
}

/** Latency figures in one unit for a "p50 · p95 · p99" row: milliseconds, or seconds past 10 s. */
function latencyRow(
  p50: number | null,
  p95: number | null,
  p99: number | null,
): { parts: string[]; unit: string } {
  const seconds = (p99 ?? 0) >= 10_000;
  const f = (v: number | null) =>
    v === null ? '—' : seconds ? (v / 1000).toFixed(1) : String(Math.round(v));
  return { parts: [f(p50), f(p95), f(p99)], unit: seconds ? 's' : 'ms' };
}

function ChartPanel({
  title,
  subtitle,
  legend,
  className,
  children,
}: {
  title: string;
  subtitle?: string;
  legend?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <Panel
      title={title}
      subtitle={subtitle}
      actions={legend}
      bodyClassName="px-4 pb-3"
      {...(className ? { className } : {})}
    >
      {children}
    </Panel>
  );
}

/**
 * The Overview, in the prototype's layout: KPI tiles, the traffic and latency charts, spend and
 * model leaderboards, and the newest requests. A new gateway keeps that layout, with honest empty
 * tiles and a Get started checklist where the charts go; a gateway without usage analytics says so
 * once and still shows its requests.
 */
export function OverviewScreen() {
  const { current, previous, key } = useUsageQueries();
  const scope = useScope((s) => s.scope);
  const environment = useScope((s) => s.environment);
  const now = useNow(60_000);
  const rangeTo = useResolvedRange().to.getTime();
  const month = useMemo(() => {
    const d = new Date(now);
    return {
      from: new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1)).toISOString(),
      // Up to the range's end or the current minute, so the month includes the range's spend.
      to: new Date(Math.max(Math.floor(now / 60_000) * 60_000, rangeTo)).toISOString(),
      name: d.toLocaleDateString('en-US', { month: 'long', timeZone: 'UTC' }),
    };
  }, [now, rangeTo]);
  const range = useResolvedRange();
  const timeZone = usePreferences((s) => s.timeZone);
  const compactNumbers = usePreferences((s) => s.compactNumbers);
  const queries = useMemo(
    () => ({
      series: current,
      previous: { ...previous, interval: undefined },
      previousSeries: previous,
      byModel: { ...current, interval: undefined, groupBy: ['model' as const] },
      byProvider: { ...current, interval: undefined, groupBy: ['provider' as const] },
      monthByTeam: {
        organization: scopeOrganization(scope),
        tag: environment ? `env:${environment}` : null,
        from: month.from,
        to: month.to,
        groupBy: ['team' as const],
      },
    }),
    [current, previous, scope, environment, month],
  );
  useUsage(queries, `${key}|${month.from}|${month.to}`);
  useRuntimeReadings();
  const counters = useOverviewRuntime((s) => s.budgets.data);
  const providerStatus = useOverviewRuntime((s) => s.providers.data);
  const monthByTeam = useAnalytics((s) => s.usage.monthByTeam);
  const series = useAnalytics((s) => s.usage.series);
  const prev = useAnalytics((s) => s.usage.previous);
  const prevSeries = useAnalytics((s) => s.usage.previousSeries);
  const compareName = compareTitle(useTimeRange((s) => s.compare)).toLowerCase();
  const byModel = useAnalytics((s) => s.usage.byModel);
  const byProvider = useAnalytics((s) => s.usage.byProvider);
  const loadUsage = useAnalytics((s) => s.loadUsage);

  const logs = useLogs((s) => s.records);
  const logHref = useScopedHref('/request-log/');
  const setup = useSetup();

  const rows = series?.data ?? [];
  // The comparison's buckets, moved onto the current range's bucket starts.
  const shift = range.from.getTime() - range.previous.from.getTime();
  const step = INTERVALS[current.interval ?? '5m'] * 1000;
  const slot = (t: number) => Math.round(t / step) * step;
  const compared = new Map(
    (prevSeries?.data ?? []).map((b) => [slot(new Date(b.start).getTime() + shift), b.requests] as const),
  );
  const t = totals(series?.data);
  const p = totals(prev?.data);
  const minutes = range.seconds / 60;
  const long = range.seconds > 2 * 86_400;
  const labels = rows.map((b) => bucketLabel(b, timeZone, long));
  const n = (v: number) => formatCount(v, { compact: compactNumbers });
  const window = range.label.startsWith('Last ') ? range.label.slice(5) : null;
  const versus = `vs ${range.previous.label}`;

  const unavailable = isUnavailable(series?.error);
  const failed = Boolean(series?.error) && !unavailable;
  const analyticsLoading = !series || (series.status === 'loading' && !series.data);
  const inRange = (t?.requests ?? 0) > 0;
  const firstRun = setup.loaded && setup.hasRequest === false && !inRange && (logs.data?.length ?? 0) === 0;

  const kpiState: KpiState = unavailable
    ? 'unavailable'
    : analyticsLoading
      ? 'loading'
      : failed || !t || t.requests === 0
        ? 'empty'
        : 'ready';
  const emptyHint = failed
    ? 'Couldn’t load'
    : firstRun
      ? 'After the first request'
      : `No requests in the ${(window ?? 'range').toLowerCase()}`;
  // Month to date and month end, by the one rule Analytics and Budgets use: the counted budgets'
  // counters and forecasts when there are any, else the month's usage at its rate.
  const monthTotals = totals(monthByTeam?.data);
  const monthFigures = monthProjection(counters, scope, monthTotals?.cost ?? null, now);
  const budget = monthFigures?.source === 'counters';
  const monthSpend = monthFigures?.spent ?? null;
  const budgetAmount = monthFigures?.amount ?? null;
  const elapsed = monthElapsed(now);
  const projected = monthFigures?.projected ?? null;
  const monthState: KpiState =
    isUnavailable(monthByTeam?.error) && !budget
      ? 'unavailable'
      : budget || (monthTotals?.requests ?? 0) > 0
        ? 'ready'
        : monthByTeam?.data || monthByTeam?.error
          ? 'empty'
          : 'loading';
  const spend = moneyParts(monthSpend ?? 0);
  const savedPerMonth = monthTotals ? monthTotals.saved / elapsed : null;
  const latency = latencyRow(t?.p50 ?? null, t?.p95 ?? null, t?.p99 ?? null);
  const errorRate = t?.requests ? t.errors / t.requests : 0;
  const cacheRate = t?.requests ? t.cacheHits / t.requests : 0;
  const p95Delta = t && p ? delta(formatDelta(t.p95, p.p95), 'bad', versus) : null;

  const providers = groupTotals(byProvider?.data, 'provider').slice(0, 8);

  const retry = () => {
    for (const [name, q] of Object.entries(queries)) void loadUsage(name, q);
  };

  return (
    <div className="flex min-h-full flex-col">
      <PageHeader
        title="Overview"
        description="Live traffic, spend and health"
        context={range.label}
        actions={<UpdatedAt at={series?.at ?? logs.at ?? null} />}
      />
      {unavailable ? (
        <Banner
          tone="info"
          title="Usage analytics isn't available on this gateway yet"
          actions={
            <Link href={logHref} className="text-ui text-muted hover:text-fg">
              View requests
            </Link>
          }
        >
          This version doesn&apos;t report spend, traffic or latency. Upgrade the gateway to see them here;
          recent requests still show below.
        </Banner>
      ) : null}
      {failed ? (
        <Banner
          tone="error"
          title="Couldn't load usage analytics"
          actions={
            <Button size="sm" icon="refresh" onClick={retry}>
              Try again
            </Button>
          }
        >
          Something went wrong on the gateway. The figures below are blank until it answers.
        </Banner>
      ) : null}

      <KpiStrip>
        <KpiTile
          label="Spend this month"
          icon="dollar"
          state={monthState}
          emptyHint={firstRun ? 'After the first request' : 'No spend yet'}
          value={spend.value}
          unit={spend.unit}
          valueTone={budgetAmount && monthSpend !== null && monthSpend > budgetAmount ? 'error' : 'neutral'}
          context={
            budgetAmount ? (
              <Tooltip content="What the budget counted, adjustments included; charts count requests">
                <span>
                  of {formatMoney(budgetAmount, { compact: true })} budget ·{' '}
                  {Math.round(((monthSpend ?? 0) / budgetAmount) * 100)}%
                </span>
              </Tooltip>
            ) : t ? (
              `${formatMoney(t.cost, { compact: true })} in range`
            ) : (
              `${month.name} so far`
            )
          }
          delta={
            projected !== null && monthSpend
              ? {
                  text: `→ ${formatMoney(projected, { compact: true })}`,
                  tone: budgetAmount ? (projected > budgetAmount ? 'bad' : 'good') : 'neutral',
                  title: budgetAmount
                    ? `Projected month end, against a ${formatMoney(budgetAmount)} budget`
                    : 'Projected month end at the current rate',
                }
              : null
          }
          spark={{ values: rows.map((r) => Number(r.cost_usd)) }}
        />
        <KpiTile
          label="Requests"
          icon="activity"
          state={kpiState}
          emptyHint={emptyHint}
          value={formatCount(t?.requests ?? 0)}
          context={t ? `${(t.requests / minutes).toFixed(1)} per minute` : null}
          delta={t && p ? delta(formatDelta(t.requests, p.requests), 'neutral', versus) : null}
          spark={{ values: rows.map((r) => r.requests) }}
        />
        <KpiTile
          label="Tokens in → out"
          icon="chart"
          state={kpiState}
          emptyHint={emptyHint}
          value={
            <>
              {formatTokens(t?.input ?? 0)}
              <span className="mx-1 font-normal text-dim">/</span>
              {formatTokens(t?.output ?? 0)}
            </>
          }
          context={t?.input ? `Output is ${((t.output / t.input) * 100).toFixed(1)}% of input` : null}
          spark={{ values: rows.map((r) => (r.input ?? 0) + (r.output ?? 0)) }}
        />
        <KpiTile
          label="Latency"
          icon="clock"
          state={kpiState}
          emptyHint={emptyHint}
          value={
            <span className="text-[15px] @[200px]:text-[17px]">
              {latency.parts[0]}
              <span className="mx-1 font-normal text-dim">·</span>
              <span className="text-warn">{latency.parts[1]}</span>
              <span className="mx-1 font-normal text-dim">·</span>
              <span className="text-error">{latency.parts[2]}</span>
            </span>
          }
          unit={latency.unit}
          context={
            <>
              p50 <span className="text-dim">·</span> <span className="text-warn">p95</span>{' '}
              <span className="text-dim">·</span> <span className="text-error">p99</span>
            </>
          }
          delta={p95Delta ? { ...p95Delta, title: `p95 ${versus}` } : null}
          spark={{ values: rows.map((r) => r.latency_ms?.p95 ?? 0), tone: 'warn' }}
        />
        <KpiTile
          label="Error rate"
          icon="alert"
          state={kpiState}
          emptyHint={emptyHint}
          value={(errorRate * 100).toFixed(2)}
          unit="%"
          valueTone={errorRate > 0.03 ? 'error' : 'neutral'}
          context={t ? `${n(t.errors)} failed` : null}
          delta={
            p?.requests && t?.requests
              ? delta(formatDelta(errorRate, p.errors / p.requests, 'points'), 'bad', versus)
              : null
          }
          spark={{ values: rows.map((r) => (r.requests ? r.errors / r.requests : 0)), tone: 'error' }}
        />
        <KpiTile
          label="Cache hit rate"
          icon="cache"
          state={kpiState}
          emptyHint={emptyHint}
          value={(cacheRate * 100).toFixed(1)}
          unit="%"
          context={
            savedPerMonth !== null && savedPerMonth > 0
              ? `~${formatMoney(savedPerMonth, { compact: true })} saved a month`
              : t
                ? `${formatMoney(t.saved)} saved`
                : null
          }
          delta={
            p?.requests && t?.requests
              ? delta(formatDelta(cacheRate, p.cacheHits / p.requests, 'points'), 'good', versus)
              : null
          }
          spark={{
            values: rows.map((r) => (r.requests ? (r.cache_hits ?? 0) / r.requests : 0)),
            tone: 'info',
          }}
        />
      </KpiStrip>

      {firstRun ? <GetStarted setup={setup} /> : null}

      {!firstRun && !unavailable ? (
        <>
          <TileGrid label="Charts" className="md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr]">
            <ChartPanel
              title="Request volume"
              className="md:col-span-2 lg:col-span-1"
              subtitle={`per ${current.interval}`}
              legend={
                <Legend
                  items={[
                    { label: 'Successful', tone: 'accent' },
                    { label: 'Errors', tone: 'error' },
                    { label: `All, ${compareName}`, tone: 'muted', dashed: true },
                  ]}
                />
              }
            >
              <TimeChart
                kind="area"
                title="Requests per bucket, successful and errors"
                labels={labels}
                height={184}
                emptyLabel={analyticsLoading ? undefined : 'No requests in this range'}
                series={[
                  { name: 'Successful', values: rows.map((r) => r.requests - r.errors), tone: 'accent' },
                  { name: 'Errors', values: rows.map((r) => r.errors), tone: 'error' },
                  {
                    name: `All requests, ${compareName}`,
                    values: rows.map((r) => compared.get(slot(new Date(r.start).getTime())) ?? null),
                    tone: 'muted',
                    dashed: true,
                  },
                ]}
                gaps
                formatValue={(v) => formatCount(v, { compact: true })}
              />
            </ChartPanel>
            <ChartPanel
              title="Latency"
              legend={
                <Legend
                  items={[
                    { label: 'p50', tone: 'accent' },
                    { label: 'p95', tone: 'warn' },
                    { label: 'p99', tone: 'error' },
                  ]}
                />
              }
            >
              <TimeChart
                kind="line"
                title="Latency percentiles per bucket"
                labels={labels}
                height={184}
                emptyLabel={analyticsLoading ? undefined : 'No requests in this range'}
                series={[
                  { name: 'p50', values: rows.map((r) => r.latency_ms?.p50 ?? 0), tone: 'accent' },
                  { name: 'p95', values: rows.map((r) => r.latency_ms?.p95 ?? 0), tone: 'warn' },
                  { name: 'p99', values: rows.map((r) => r.latency_ms?.p99 ?? 0), tone: 'error' },
                ]}
                formatValue={(v) => (v >= 1000 ? `${(v / 1000).toFixed(1)} s` : `${Math.round(v)} ms`)}
              />
            </ChartPanel>
            <ChartPanel title="Volume and errors">
              <TimeChart
                kind="stacked-bars"
                title="Requests per bucket, stacked successful and errors"
                labels={labels}
                height={184}
                emptyLabel={analyticsLoading ? undefined : 'No requests in this range'}
                series={[
                  { name: 'Successful', values: rows.map((r) => r.requests - r.errors), tone: 'accent' },
                  { name: 'Errors', values: rows.map((r) => r.errors), tone: 'error' },
                ]}
                formatValue={(v) => formatCount(v, { compact: true })}
              />
            </ChartPanel>
          </TileGrid>

          <TileGrid label="Leaderboards" className="md:grid-cols-2 lg:grid-cols-[1.2fr_1fr_1fr]">
            <TeamSpend
              usage={monthByTeam?.data}
              counters={counters}
              className="md:col-span-2 lg:col-span-1"
            />
            <TopModels usage={byModel?.data} format={n} />
            <ProviderHeadroom usage={byProvider?.data} status={providerStatus} minutes={minutes} />
          </TileGrid>
        </>
      ) : null}

      <RecentRequests
        providers={providers.map((pr) => pr.name).filter((name) => name !== 'other')}
        firstRun={firstRun}
      />
    </div>
  );
}
