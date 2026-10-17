'use client';

import { useRouter } from 'next/navigation';
import { useEffect, useMemo, type ReactNode } from 'react';

import { Dot } from '@/components/ui/badge';
import {
  BarRow,
  Legend,
  Meter,
  TimeChart,
  type ChartAnnotation,
  type ChartTone,
} from '@/components/ui/charts';
import { usePermission } from '@/components/ui/gate';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { KpiStrip, KpiTile, type KpiDelta, type KpiState } from '@/components/ui/kpi';
import { PageHeader, Panel, TileGrid } from '@/components/ui/layout';
import { UpdatedAt } from '@/components/ui/misc';
import { EmptyState, ErrorState, isUnavailable, LoadingRows, UnavailableState } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { cn } from '@/lib/cn';
import { formatCount, formatLatency, formatMoney, formatPercent, formatTokens, plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { chartLabel } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { scopeOrganization, useScope } from '@/stores/scope';
import { useScopedHref } from '@/components/layout/url-sync';
import { ModelName, ProviderName } from '@/components/ui/marks';
import { useTokenPrice } from '@/lib/hooks';
import { useProviderType } from '@/stores/provider-types';
import { useResolvedRange } from '@/stores/time-range';

import { useAlerts } from '@/features/alerts/store';
import { teamBudgets, useOverviewRuntime, useRuntimeReadings } from '@/features/overview/runtime';

import { useUsage, useUsageQueries } from '../hooks';
import { groupTotals, totals, useAnalytics, type Totals } from '../store';
import type { ProviderPressure } from '../api';
import { useAnalyticsExtras, useExtrasLoader } from '../extras';
import { analyticsCsv } from '../export';
import { EfficiencyChart, Heatmap } from './analytics-charts';
import { DetailsStrip } from './details-strip';
import { PressurePanel } from './pressure-panel';
import { SignalsPanel } from './signals-panel';
import { teamBudgetRows, TeamBudgetsPanel, type TeamBudgetRow } from './team-budgets-panel';
import { monthProjection } from '@/features/budgets/projection';

const PROVIDER_TONES: ChartTone[] = ['accent', 'warn', 'info', 'success', 'error', 'muted'];

/** A change against the comparison window, coloured by whether up is good, bad or neither. */
function deltaAgainst(
  title: string,
  current: number,
  previous: number | undefined,
  up: 'good' | 'bad' | 'neutral',
  mode: 'relative' | 'points' = 'relative',
): KpiDelta | null {
  if (previous === undefined) return null;
  if (mode === 'points') {
    const d = (current - previous) * 100;
    if (Math.abs(d) < 0.05) return { text: 'no change', tone: 'neutral', title };
    return {
      text: `${d > 0 ? '+' : '−'}${Math.abs(d).toFixed(1)} pp`,
      tone: up === 'neutral' ? 'neutral' : d > 0 === (up === 'good') ? 'good' : 'bad',
      title,
    };
  }
  if (previous === 0) return current === 0 ? { text: 'no change', tone: 'neutral', title } : null;
  const d = ((current - previous) / Math.abs(previous)) * 100;
  if (Math.abs(d) < 0.5) return { text: 'no change', tone: 'neutral', title };
  return {
    text: `${d > 0 ? '+' : '−'}${Math.abs(d).toFixed(Math.abs(d) >= 10 ? 0 : 1)}%`,
    tone: up === 'neutral' ? 'neutral' : d > 0 === (up === 'good') ? 'good' : 'bad',
    title,
  };
}

function ratio(a: number, b: number): number {
  return b > 0 ? a / b : 0;
}

/** A section heading between bands ("This month", "Last 6 hours"). */
function SectionTitle({ children, aside }: { children: ReactNode; aside?: ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-3 border-b border-line bg-canvas px-gutter pt-5 pb-2 max-sm:px-gutter-sm">
      <h2 className="text-ui font-semibold text-fg">{children}</h2>
      {aside ? <span className="text-caption text-muted">{aside}</span> : null}
    </div>
  );
}

/** The empty pill in place of a panel's rows, keeping the panel's place. */
function Quiet({ children }: { children: string }) {
  return <EmptyState size="sm" title={children} />;
}

/**
 * Analytics: spend against the month with a projection, the range's KPIs, spend by provider,
 * open alerts, the busiest hours, top models, model efficiency, provider health, cache impact
 * by team and tokens by model.
 */
/** Requests for names no Model has (a typo, a retired Model): the usage reports group them as one. */
const UNKNOWN_MODELS = 'Names not in the catalog';
const UNKNOWN_MODELS_HINT = 'Requests that named no Model of this gateway; the Request log lists each name';

export function AnalyticsScreen() {
  const { current, previous, key } = useUsageQueries();
  const range = useResolvedRange();
  const scope = useScope((s) => s.scope);
  const environment = useScope((s) => s.environment);
  const timeZone = usePreferences((s) => s.timeZone);
  const now = useNow(60_000);
  const month = useMemo(() => {
    const d = new Date(now);
    const start = new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1));
    return {
      from: start.toISOString(),
      // Up to the current minute, so today's spend counts as soon as it happens.
      to: new Date(Math.floor(now / 60_000) * 60_000).toISOString(),
      start: start.getTime(),
      days: new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 0)).getUTCDate(),
      day: d.getUTCDate(),
      name: d.toLocaleDateString('en-US', { month: 'long', timeZone: 'UTC' }),
    };
  }, [now]);
  const base = useMemo(
    () => ({ organization: scopeOrganization(scope), tag: environment ? `env:${environment}` : null }),
    [scope, environment],
  );
  const queries = useMemo(
    () => ({
      a_series: current,
      a_previous: { ...previous, interval: undefined },
      a_byProvider: { ...current, groupBy: ['provider' as const] },
      a_byModel: { ...current, interval: undefined, groupBy: ['model' as const] },
      a_byTeam: { ...current, interval: undefined, groupBy: ['team' as const] },
      a_byDeployment: { ...current, interval: undefined, groupBy: ['deployment' as const] },
      a_byFlag: { ...current, interval: undefined, groupBy: ['guardrail_category' as const] },
      a_month: { ...base, from: month.from, to: month.to, interval: '1d' as const },
      a_week: {
        ...base,
        from: new Date(now - 7 * 86_400_000).toISOString(),
        to: new Date(now).toISOString(),
        interval: '1h' as const,
      },
    }),
    [current, previous, base, month, now],
  );
  useUsage(queries, `${key}|${month.from}`);
  const usage = useAnalytics((s) => s.usage);
  const alerts = useAlerts((s) => s.alerts);
  const loadAlerts = useAlerts((s) => s.load);
  const canAlerts = usePermission('alerts:read');
  useEffect(() => {
    if (canAlerts) void loadAlerts();
  }, [loadAlerts, canAlerts]);
  useRuntimeReadings();
  const canBudgets = usePermission('policies:read');
  const canConfig = usePermission('config:read');
  const budgets = useOverviewRuntime((s) => s.budgets);
  useExtrasLoader({
    from: current.from,
    to: current.to,
    interval: current.interval ?? '5m',
    previousFrom: previous.from,
    previousTo: previous.to,
    key,
  });
  const extras = useAnalyticsExtras();
  const byDeployment = usage.a_byDeployment?.data;
  useEffect(() => {
    if (byDeployment)
      void useAnalyticsExtras.getState().loadTransition(byDeployment, current.from, current.to);
  }, [byDeployment, current.from, current.to]);

  const series = usage.a_series;
  const t = totals(series?.data);
  const p = totals(usage.a_previous?.data) ?? undefined;
  const monthRows = usage.a_month?.data ?? [];
  const mtd = monthRows.reduce((a, r) => a + Number(r.cost_usd), 0);
  const perDay = month.day > 0 ? mtd / month.day : 0;
  const linear = mtd + perDay * (month.days - month.day);
  const rows = series?.data ?? [];
  const providerRows = usage.a_byProvider?.data ?? [];
  // Requests refused before routing reach no provider; the usage report groups them as "other".
  const providerNames = groupTotals(providerRows, 'provider')
    .map((g) => g.name)
    .filter((name) => name !== 'other');
  const starts = [...new Set(providerRows.map((r) => r.start))].sort();
  const allModels = groupTotals(usage.a_byModel?.data, 'model');
  const tokenPrice = useTokenPrice();
  const typeOf = useProviderType();
  const router = useRouter();
  const modelsHref = useScopedHref('/models/');
  const models = [...allModels].sort((a, b) => b.totals.cost - a.totals.cost).slice(0, 8);
  const teams = groupTotals(usage.a_byTeam?.data, 'team');
  const providerGroups = groupTotals(usage.a_byProvider?.data, 'provider');
  const providers = providerGroups.filter((g) => g.name !== 'other');
  const refused = providerGroups.find((g) => g.name === 'other')?.totals.requests ?? 0;
  const modelSpend = allModels.reduce((a, m) => a + m.totals.cost, 0);
  const teamRows = teamBudgetRows(teamBudgets(budgets.data), teams);
  const weekSaved = usage.a_week?.data
    ? usage.a_week.data.reduce((a, r) => a + Number(r.cache_saved_usd ?? 0), 0) / 7
    : null;
  const rangeDays = Math.max(1 / 24, (range.to.getTime() - range.from.getTime()) / 86_400_000);
  const flagRows = usage.a_byFlag?.data;
  const flagSplit = flagRows
    ? {
        categories: groupTotals(flagRows, 'guardrail_category')
          .filter((g) => g.name !== 'other' && g.totals.flags > 0)
          .map((g) => ({ name: g.name, count: g.totals.flags }))
          .sort((a, b) => b.count - a.count),
        errors: t?.errors ?? 0,
      }
    : null;
  const empty = t !== null && t.requests === 0;
  const kpi: KpiState = !t ? 'loading' : empty ? 'empty' : 'ready';
  const emptyHint = 'No requests in this range';
  const windowLabel = range.label;
  const versus = `vs ${range.previous.label}`;
  const delta = (
    current: number,
    previous: number | undefined,
    up: 'good' | 'bad' | 'neutral',
    mode: 'relative' | 'points' = 'relative',
  ) => deltaAgainst(versus, current, previous, up, mode);

  if (series?.error && !series.data) {
    return (
      <>
        <PageHeader title="Analytics" description="Spend, performance and use across the gateway" />
        <div className="flex min-h-[calc(100vh-48px-57px)] items-center justify-center bg-raised">
          {isUnavailable(series.error) ? (
            <UnavailableState
              subject="Analytics"
              description="Upgrade the gateway to use it. The request log still lists every request."
              operation={(series.error as { operation?: string }).operation}
            />
          ) : (
            <ErrorState error={series.error} subject="Analytics" />
          )}
        </div>
      </>
    );
  }

  const monthLabels = Array.from({ length: month.days }, (_, i) =>
    new Date(month.start + i * 86_400_000).toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
      timeZone: 'UTC',
    }),
  );
  const dayOf = (iso: string) => new Date(iso).getUTCDate() - 1;
  const actual: (number | null)[] = monthLabels.map((_, i) => (i < month.day ? 0 : null));
  for (const r of monthRows) {
    const i = dayOf(r.start);
    if (i >= 0 && i < month.day) actual[i] = (actual[i] ?? 0) + Number(r.cost_usd);
  }
  // The one month-end rule Overview and Budgets use: the counted budgets' forecasts, else usage.
  const figures = monthProjection(budgets.data, scope, mtd, now);
  const budget =
    figures?.source === 'counters'
      ? { spent: figures.spent, amount: figures.amount ?? 0, projected: figures.projected }
      : null;
  const projected = figures ? figures.projected : linear;
  const spentSoFar = budget ? budget.spent : mtd;
  const overage = budget ? budget.projected - budget.amount : 0;
  const projection: (number | null)[] = monthLabels.map((_, i) =>
    i === month.day - 1 ? (actual[i] ?? 0) : i >= month.day ? perDay : null,
  );

  const topDay = actual
    .map((cost, i) => ({ cost: cost ?? 0, label: monthLabels[i] ?? '' }))
    .sort((a, b) => b.cost - a.cost)[0];
  const sparkOf = (f: (r: (typeof rows)[number]) => number) => ({ values: rows.map(f) });

  const events = timelineEvents(extras.generations.data, alerts.data);
  const monthNotes: ChartAnnotation[] = events
    .filter((e) => e.at >= month.start && e.at < month.start + month.days * 86_400_000)
    .map((e) => ({ index: Math.floor((e.at - month.start) / 86_400_000), label: e.label, tone: e.tone }));
  const startTimes = starts.map((x) => new Date(x).getTime());
  const rangeNotes: ChartAnnotation[] = events
    .filter((e) => e.at >= range.from.getTime() && e.at < range.to.getTime())
    .map((e) => ({ index: startTimes.filter((t) => t <= e.at).length - 1, label: e.label, tone: e.tone }))
    .filter((a) => a.index >= 0);
  // One flag per bucket on the provider chart, its events in one line of the hover card, so no bare
  // count is drawn beside the flag.
  const providerNotes: ChartAnnotation[] = [];
  for (const a of rangeNotes) {
    const same = providerNotes.find((p) => p.index === a.index);
    if (same) same.label = `${same.label} · ${a.label}`;
    else providerNotes.push({ ...a });
  }
  const noteLegend = [
    ...(events.some((e) => e.kind === 'config')
      ? [{ label: 'Configuration change', tone: 'info' as const }]
      : []),
    ...(events.some((e) => e.kind === 'alert') ? [{ label: 'Alert', tone: 'warn' as const }] : []),
  ];
  const csv = {
    models: allModels,
    providers,
    teams,
    teamRows,
    pressure: extras.pressure.data,
    month: monthLabels.map((label, i) => ({ label, spent: actual[i] ?? null })),
  };

  return (
    <>
      <PageHeader
        title="Analytics"
        description="Spend, performance and use across the gateway"
        context={windowLabel}
        actions={
          <div className="flex items-center gap-2">
            <UpdatedAt at={series?.at ?? null} />
            <MenuButton
              label="Export CSV"
              icon="download"
              variant="ghost"
              items={exportItems(csv, windowLabel, current.from.slice(0, 16).replace(/[:T]/g, '-'))}
            />
          </div>
        }
      />
      <SectionTitle aside={`${month.name}, UTC`}>This month</SectionTitle>
      <TileGrid label="This month" className="grid-cols-1 xl:grid-cols-[minmax(0,1fr)_minmax(0,1.7fr)]">
        <div className="flex min-w-0 flex-col gap-5 bg-raised bg-[linear-gradient(135deg,color-mix(in_oklab,var(--ant-accent)_9%,transparent),transparent_60%)] px-gutter py-5 max-sm:px-gutter-sm">
          <div>
            <div className="flex items-center gap-2 text-caption font-medium text-muted">
              <Dot tone="accent" /> Projected spend this month
            </div>
            {usage.a_month?.data || budget ? (
              <>
                <div
                  className={cn(
                    'mono mt-3 text-[40px] leading-none font-semibold tracking-tight tabular-nums',
                    spentSoFar > 0 ? 'text-fg' : 'text-dim',
                  )}
                >
                  {spentSoFar > 0 ? formatMoney(projected, { compact: true }) : '—'}
                </div>
                <p className="mt-3 text-ui text-muted">
                  {spentSoFar > 0 ? (
                    <>
                      <span className="mono font-semibold text-fg tabular-nums">
                        {formatMoney(spentSoFar, { compact: true })}
                      </span>{' '}
                      spent so far · {plural(month.days - month.day, 'day')} left
                      {budget ? (
                        <>
                          {' · '}
                          <span className={cn('font-medium', overage > 0 ? 'text-error' : 'text-success')}>
                            {overage > 0
                              ? `${formatMoney(overage, { compact: true })} over`
                              : `${formatMoney(-overage, { compact: true })} under`}
                          </span>{' '}
                          the {formatMoney(budget.amount, { compact: true })} budget
                        </>
                      ) : null}
                    </>
                  ) : (
                    'No spend this month yet. The projection starts with the first paid request.'
                  )}
                </p>
              </>
            ) : (
              <LoadingRows rows={2} label="Loading this month's spend" />
            )}
          </div>
          {budget && budget.spent > 0 ? (
            <div className="flex flex-col gap-4">
              <div>
                <Meter
                  value={budget.spent}
                  max={budget.amount}
                  projected={budget.projected}
                  tone={overage > 0 ? 'warn' : 'accent'}
                  tick={month.day / month.days}
                  label={`Spent ${formatMoney(budget.spent)} of the ${formatMoney(budget.amount)} budget, projected ${formatMoney(budget.projected)}`}
                />
                <div className="mono mt-1.5 flex justify-between gap-2 text-caption text-dim tabular-nums">
                  <span>$0</span>
                  <span>{formatMoney(budget.spent, { compact: true })} used</span>
                  <span className={overage > 0 ? 'text-warn' : undefined}>
                    projected {formatMoney(budget.projected, { compact: true })}
                  </span>
                  <span>budget {formatMoney(budget.amount, { compact: true })}</span>
                </div>
              </div>
              <MonthFacts perDay={perDay} topDay={topDay} />
            </div>
          ) : usage.a_month?.data && mtd > 0 ? (
            <div className="flex flex-col gap-4">
              <div>
                <Meter
                  value={mtd}
                  max={Math.max(projected, 0.0001)}
                  tick={month.day / month.days}
                  label="Spent so far against the projection"
                />
                <div className="mono mt-1.5 flex justify-between text-caption text-dim tabular-nums">
                  <span>$0</span>
                  <span>
                    Day {month.day} of {month.days}
                  </span>
                  <span>{formatMoney(projected, { compact: true })}</span>
                </div>
              </div>
              <MonthFacts perDay={perDay} topDay={topDay} />
            </div>
          ) : null}
        </div>
        <Panel
          title="Daily spend this month"
          subtitle={perDay > 0 ? `${formatMoney(perDay)} a day on average` : undefined}
          actions={
            <Legend
              items={[
                { label: 'Spent', tone: 'accent' },
                { label: 'Projected', tone: 'accent', dashed: true },
                ...noteLegend,
              ]}
            />
          }
        >
          <TimeChart
            kind="area"
            gaps
            title="Daily spend this month, with the projection to month end"
            labels={monthLabels}
            series={[
              { name: 'Spent', values: actual, tone: 'accent' },
              { name: 'Projected', values: projection, tone: 'accent', dashed: true },
            ]}
            marker={{ index: month.day - 1, label: 'Today' }}
            annotations={monthNotes}
            formatValue={(v) => formatMoney(v, { compact: true })}
            emptyLabel={usage.a_month?.data ? 'No spend this month yet' : undefined}
            height={210}
          />
        </Panel>
      </TileGrid>
      <SectionTitle aside={`Compared with ${range.previous.label}`}>{windowLabel}</SectionTitle>
      <KpiStrip columns={4} label={`Key figures, ${windowLabel.toLowerCase()}`}>
        <KpiTile
          label="Requests"
          icon="activity"
          state={kpi}
          emptyHint={emptyHint}
          value={t ? formatCount(t.requests) : undefined}
          context={t ? `${formatPercent(ratio(t.errors, t.requests), 2)} failed` : undefined}
          delta={t ? delta(t.requests, p?.requests, 'neutral') : null}
          spark={sparkOf((r) => r.requests)}
        />
        <KpiTile
          label="Cost per request"
          icon="dollar"
          state={kpi}
          emptyHint={emptyHint}
          value={t ? formatMoney(ratio(t.cost, t.requests)) : undefined}
          context={t ? `${formatMoney(t.cost)} in total` : undefined}
          delta={t && p ? delta(ratio(t.cost, t.requests), ratio(p.cost, p.requests), 'bad') : null}
          spark={sparkOf((r) => ratio(Number(r.cost_usd), r.requests))}
        />
        <KpiTile
          label="p95 latency"
          icon="clock"
          state={kpi}
          emptyHint={emptyHint}
          value={t ? formatLatency(t.p95) : undefined}
          context={t ? `p50 ${formatLatency(t.p50)} · p99 ${formatLatency(t.p99)}` : undefined}
          delta={t && p?.p95 != null && t.p95 != null ? delta(t.p95, p.p95, 'bad') : null}
          spark={{ values: rows.map((r) => r.latency_ms?.p95 ?? null), tone: 'warn' }}
        />
        <KpiTile
          label="Cache hit rate"
          icon="cache"
          state={kpi}
          emptyHint={emptyHint}
          value={t ? formatPercent(ratio(t.cacheHits, t.requests), 1) : undefined}
          context={t ? `${formatCount(t.cacheHits)} cached responses` : undefined}
          delta={
            t && p
              ? delta(ratio(t.cacheHits, t.requests), ratio(p.cacheHits, p.requests), 'good', 'points')
              : null
          }
          spark={{ values: rows.map((r) => ratio(r.cache_hits ?? 0, r.requests)), tone: 'success' }}
        />
        <KpiTile
          label="Saved by caching"
          icon="dollar"
          state={kpi}
          emptyHint={emptyHint}
          valueTone="success"
          value={t ? formatMoney(t.saved) : undefined}
          context={
            t ? `${formatPercent(ratio(t.saved, t.cost + t.saved), 1)} of what it would cost` : undefined
          }
          delta={t ? delta(t.saved, p?.saved, 'good') : null}
          spark={{ values: rows.map((r) => Number(r.cache_saved_usd ?? 0)), tone: 'success' }}
        />
        <KpiTile
          label="Fallbacks"
          icon="route"
          state={kpi}
          emptyHint={emptyHint}
          value={t ? formatCount(t.fallbacks) : undefined}
          context={t ? `${formatPercent(ratio(t.fallbacks, t.requests), 1)} of requests` : undefined}
          delta={t ? delta(t.fallbacks, p?.fallbacks, 'bad') : null}
          spark={{ values: rows.map((r) => r.fallbacks ?? 0), tone: 'warn' }}
        />
        <KpiTile
          label="Tokens"
          icon="code"
          state={kpi}
          emptyHint={emptyHint}
          value={t ? formatTokens(t.input + t.output) : undefined}
          context={t ? `${formatTokens(t.input)} in · ${formatTokens(t.output)} out` : undefined}
          delta={t ? delta(t.input + t.output, p ? p.input + p.output : undefined, 'neutral') : null}
          spark={sparkOf((r) => (r.input ?? 0) + (r.output ?? 0))}
        />
        <KpiTile
          label="Guardrail flags"
          icon="shield"
          state={kpi}
          emptyHint={emptyHint}
          value={t ? formatCount(t.flags) : undefined}
          context={t ? `of ${plural(t.requests, 'request')}` : undefined}
          delta={t ? delta(t.flags, p?.flags, 'neutral') : null}
          spark={{
            values: rows.map(
              (r) => (r.guardrail_flags ?? 0) + (r.guardrail_blocks ?? 0) + (r.guardrail_redactions ?? 0),
            ),
            tone: 'error',
          }}
        />
      </KpiStrip>
      <DetailsStrip
        savings={
          t
            ? { perDay: t.saved / rangeDays, perMonth: (t.saved / rangeDays) * 30, weekMean: weekSaved }
            : null
        }
        transition={extras.transition.data}
        transitionLoading={extras.transition.status === 'loading'}
        fallbacks={t?.fallbacks ?? 0}
        flags={flagSplit}
        p95={t?.p95 ?? null}
        slo={extras.slo.data}
        sloKnown={canConfig}
      />
      <TileGrid
        label="Spend and alerts"
        className={canAlerts ? 'grid-cols-1 xl:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]' : 'grid-cols-1'}
      >
        <Panel
          title="Spend by provider"
          subtitle={
            t && !empty
              ? `${formatMoney(t.cost)} across ${plural(providerNames.length, 'provider')}`
              : undefined
          }
        >
          {usage.a_byProvider?.data ? (
            <>
              <TimeChart
                kind="stacked-area"
                title="Spend by provider over time"
                labels={starts.map((s) => chartLabel(s, timeZone, range.seconds))}
                series={providerNames.map((name, i) => ({
                  name,
                  values: starts.map((s) =>
                    Number(
                      providerRows.find((r) => r.start === s && r.group.provider === name)?.cost_usd ?? 0,
                    ),
                  ),
                  tone: PROVIDER_TONES[i % PROVIDER_TONES.length] ?? 'accent',
                }))}
                formatValue={(v) => formatMoney(v, { compact: true })}
                emptyLabel="No spend in this range"
                annotations={providerNotes}
                height={300}
                labelWidth={range.seconds > 2 * 86_400 ? 100 : 76}
              />
              {providerNames.length > 0 ? (
                <Legend
                  className="mt-2"
                  items={[
                    ...providers.map((pr) => ({
                      label: pr.name,
                      tone:
                        PROVIDER_TONES[providerNames.indexOf(pr.name) % PROVIDER_TONES.length] ?? 'accent',
                      value: formatMoney(pr.totals.cost),
                    })),
                    ...(providerNotes.length > 0 ? noteLegend : []),
                  ]}
                />
              ) : null}
            </>
          ) : (
            <LoadingRows rows={4} label="Loading spend by provider" />
          )}
        </Panel>
        {canAlerts ? <SignalsPanel /> : null}
      </TileGrid>
      <TileGrid
        label="Traffic and models"
        className="grid-cols-1 xl:grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)]"
      >
        <Panel title="Busiest hours" subtitle="Last 7 days">
          {usage.a_week?.data ? (
            <Heatmap rows={usage.a_week.data} timeZone={timeZone} />
          ) : (
            <LoadingRows rows={3} label="Loading the busiest hours" />
          )}
        </Panel>
        <Panel title="Top models by spend">
          {models.length === 0 ? (
            usage.a_byModel?.data ? (
              <Quiet>No requests in this range</Quiet>
            ) : (
              <LoadingRows rows={4} label="Loading models" />
            )
          ) : (
            <Table label="Top models by spend">
              <thead>
                <tr>
                  <Th>Model</Th>
                  <Th numeric>Requests</Th>
                  <Th numeric>p95</Th>
                  <Th numeric>Spend</Th>
                  <Th className="w-[28%]">Share</Th>
                </tr>
              </thead>
              <tbody>
                {models.map((m) => (
                  <Tr key={m.name}>
                    <Td className="max-w-40" title={m.name === 'other' ? UNKNOWN_MODELS_HINT : m.name}>
                      {m.name === 'other' ? (
                        <span className="text-muted">{UNKNOWN_MODELS}</span>
                      ) : (
                        <ModelName model={m.name} className="max-w-full text-fg" />
                      )}
                    </Td>
                    <Td numeric>{formatCount(m.totals.requests)}</Td>
                    <Td numeric>{formatLatency(m.totals.p95)}</Td>
                    <Td numeric>{formatMoney(m.totals.cost)}</Td>
                    <Td>
                      <span className="flex items-center gap-2">
                        <Meter
                          className="w-auto min-w-0 flex-1"
                          value={m.totals.cost}
                          max={modelSpend || 1}
                          label={`${m.name}: ${formatPercent(ratio(m.totals.cost, modelSpend), 1)} of spend`}
                        />
                        <span className="mono w-11 shrink-0 text-right text-caption text-muted tabular-nums">
                          {formatPercent(ratio(m.totals.cost, modelSpend), 0)}
                        </span>
                      </span>
                    </Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          )}
        </Panel>
      </TileGrid>
      <TileGrid label="Efficiency and providers" className="grid-cols-1 xl:grid-cols-2">
        <Panel
          title="Model efficiency"
          subtitle="Cost per request against p95 latency; bubble size is requests"
        >
          {allModels.length > 0 ? (
            <EfficiencyChart
              onOpen={(name) => {
                const [path, query = ''] = modelsHref.split('?');
                const q = new URLSearchParams(query);
                q.set('model', name);
                router.push(`${path ?? ''}?${q.toString()}`);
              }}
              points={allModels.map((m) => ({
                name: m.name,
                requests: m.totals.requests,
                costPerRequest: ratio(m.totals.cost, m.totals.requests),
                p95: m.totals.p95,
              }))}
            />
          ) : usage.a_byModel?.data ? (
            <Quiet>No requests in this range</Quiet>
          ) : (
            <LoadingRows rows={4} label="Loading models" />
          )}
        </Panel>
        <Panel
          title="Provider health"
          subtitle={
            refused > 0
              ? `Success rate and latency; ${formatCount(refused)} requests were refused before reaching a provider`
              : 'Success rate and latency'
          }
        >
          {providers.length === 0 ? (
            usage.a_byProvider?.data ? (
              <Quiet>No requests in this range</Quiet>
            ) : (
              <LoadingRows rows={4} label="Loading providers" />
            )
          ) : (
            <Table label="Provider health">
              <thead>
                <tr>
                  <Th>Provider</Th>
                  <Th className="w-[26%] min-w-40">Success</Th>
                  <Th numeric>p95</Th>
                  <Th numeric className="max-sm:hidden">
                    p99
                  </Th>
                  <Th numeric>Requests</Th>
                </tr>
              </thead>
              <tbody>
                {providers.map((pr) => {
                  const success = 1 - ratio(pr.totals.errors, pr.totals.requests);
                  const tone: ChartTone = success < 0.95 ? 'error' : success < 0.99 ? 'warn' : 'success';
                  return (
                    <Tr key={pr.name}>
                      <Td>
                        <span className="flex items-center gap-2">
                          <Dot tone={tone === 'success' ? 'success' : tone === 'warn' ? 'warn' : 'error'} />
                          <ProviderName type={typeOf(pr.name)} name={pr.name} />
                        </span>
                      </Td>
                      <Td>
                        <span className="flex items-center gap-2">
                          <span className="min-w-16 flex-1">
                            <Meter value={success} max={1} tone={tone} label={`${pr.name} success rate`} />
                          </span>
                          <span
                            className={cn(
                              'mono w-14 shrink-0 text-right text-caption tabular-nums',
                              tone === 'error' ? 'text-error' : tone === 'warn' ? 'text-warn' : 'text-fg',
                            )}
                          >
                            {formatPercent(success, 2)}
                          </span>
                        </span>
                      </Td>
                      <Td numeric>{formatLatency(pr.totals.p95)}</Td>
                      <Td numeric className="max-sm:hidden">
                        {formatLatency(pr.totals.p99)}
                      </Td>
                      <Td numeric>{formatCount(pr.totals.requests)}</Td>
                    </Tr>
                  );
                })}
              </tbody>
            </Table>
          )}
        </Panel>
      </TileGrid>
      <TileGrid label="Capacity and budgets" className="grid-cols-1 xl:grid-cols-2">
        <PressurePanel
          pressure={extras.pressure.data}
          previous={extras.pressurePrevious.data}
          error={extras.pressure.error}
          timeZone={timeZone}
          versus={`vs ${range.previous.label}`}
        />
        <TeamBudgetsPanel
          rows={teamRows}
          loading={canBudgets && !budgets.data && !budgets.error}
          allowed={canBudgets}
        />
      </TileGrid>
      <TileGrid label="Teams and tokens" className="grid-cols-1 xl:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]">
        <Panel
          title="Cache impact by team"
          subtitle={t && t.saved > 0 ? `${formatMoney(t.saved)} saved` : undefined}
        >
          {teams.length === 0 ? (
            usage.a_byTeam?.data ? (
              <Quiet>No requests in this range</Quiet>
            ) : (
              <LoadingRows rows={4} label="Loading teams" />
            )
          ) : (
            <ul aria-label="Cache hit rate by team" className="flex flex-col gap-2">
              {teams.map((tm) => (
                <TeamCacheRow key={tm.name} name={tm.name} totals={tm.totals} />
              ))}
            </ul>
          )}
        </Panel>
        <Panel title="Tokens by model" subtitle="Averages per request">
          {allModels.length === 0 ? (
            usage.a_byModel?.data ? (
              <Quiet>No requests in this range</Quiet>
            ) : (
              <LoadingRows rows={4} label="Loading models" />
            )
          ) : (
            <Table label="Tokens by model">
              <thead>
                <tr>
                  <Th>Model</Th>
                  <Th numeric>Input</Th>
                  <Th numeric>Output</Th>
                  <Th numeric>Total tokens</Th>
                  <Th numeric>Cost per {tokenPrice.unit} tokens</Th>
                </tr>
              </thead>
              <tbody>
                {[...allModels]
                  .sort((a, b) => b.totals.input + b.totals.output - (a.totals.input + a.totals.output))
                  .slice(0, 8)
                  .map((m) => {
                    const tokens = m.totals.input + m.totals.output;
                    return (
                      <Tr key={m.name}>
                        <Td className="max-w-48" title={m.name}>
                          <ModelName model={m.name} />
                        </Td>
                        <Td numeric>{formatCount(ratio(m.totals.input, m.totals.requests))}</Td>
                        <Td numeric>{formatCount(ratio(m.totals.output, m.totals.requests))}</Td>
                        <Td numeric>{formatTokens(tokens)}</Td>
                        <Td numeric>{tokens > 0 ? tokenPrice.perTokens(m.totals.cost / tokens) : '—'}</Td>
                      </Tr>
                    );
                  })}
              </tbody>
            </Table>
          )}
        </Panel>
      </TileGrid>
    </>
  );
}

function TeamCacheRow({ name, totals: tm }: { name: string; totals: Totals }) {
  const rate = ratio(tm.cacheHits, tm.requests);
  return (
    <BarRow
      className="min-h-9 grid-cols-[minmax(0,1fr)_minmax(60px,1fr)_168px]"
      name={<span className="truncate">{name}</span>}
      value={rate}
      max={1}
      tone="success"
      label={`${name}: ${formatPercent(rate, 1)} of requests served from the cache`}
      display={
        <span className="flex flex-col items-end leading-4">
          <span>
            {formatPercent(rate, 0)} · <span className="text-success">{formatMoney(tm.saved)}</span>
          </span>
          <span className="text-dim">
            {formatCount(tm.cacheHits)} of {plural(tm.requests, 'request')}
          </span>
        </span>
      }
    />
  );
}

function MonthFacts({
  perDay,
  topDay,
}: {
  perDay: number;
  topDay: { cost: number; label: string } | undefined;
}) {
  return (
    <dl className="grid grid-cols-2 gap-4 border-t border-line pt-3">
      <div>
        <dt className="text-caption text-muted">Average a day</dt>
        <dd className="mono mt-0.5 text-body font-semibold text-fg tabular-nums">{formatMoney(perDay)}</dd>
      </div>
      <div>
        <dt className="text-caption text-muted">Most expensive day</dt>
        <dd className="mono mt-0.5 text-body font-semibold text-fg tabular-nums">
          {topDay && topDay.cost > 0 ? (
            <>
              {formatMoney(topDay.cost)}{' '}
              <span className="font-sans text-caption font-normal text-muted">{topDay.label}</span>
            </>
          ) : (
            '—'
          )}
        </dd>
      </div>
    </dl>
  );
}

interface TimelineEvent {
  at: number;
  label: string;
  tone: ChartTone;
  kind: 'config' | 'alert';
}

/** Configuration generations and alert instances, as events for the time axes. */
function timelineEvents(
  generations: { generation: number; created_at: string; summary: string; source: string }[] | null,
  alerts: { fired_at: string; summary: string; severity: string }[] | null,
): TimelineEvent[] {
  return [
    ...(generations ?? []).map((g) => ({
      at: new Date(g.created_at).getTime(),
      label: `Configuration ${g.generation}${g.source === 'rollback' ? ' (rollback)' : ''}: ${g.summary}`,
      tone: 'info' as const,
      kind: 'config' as const,
    })),
    ...(alerts ?? []).map((a) => ({
      at: new Date(a.fired_at).getTime(),
      label: `Alert: ${a.summary}`,
      tone: a.severity === 'critical' ? ('error' as const) : ('warn' as const),
      kind: 'alert' as const,
    })),
  ].sort((a, b) => a.at - b.at);
}

function exportItems(
  csv: {
    models: { name: string; totals: Totals }[];
    providers: { name: string; totals: Totals }[];
    teams: { name: string; totals: Totals }[];
    teamRows: TeamBudgetRow[];
    pressure: ProviderPressure | null;
    month: { label: string; spent: number | null }[];
  },
  windowLabel: string,
  suffix: string,
): MenuItem[] {
  const range = windowLabel.toLowerCase();
  const pressure = csv.pressure;
  return [
    {
      label: `Models, ${range}`,
      onSelect: () => analyticsCsv.models(csv.models, suffix),
      disabled: csv.models.length === 0,
      disabledReason: 'No requests in this range.',
    },
    {
      label: `Providers, ${range}`,
      onSelect: () => analyticsCsv.providers(csv.providers, suffix),
      disabled: csv.providers.length === 0,
      disabledReason: 'No requests in this range.',
    },
    {
      label: `Teams, ${range}`,
      onSelect: () => analyticsCsv.teams(csv.teams, suffix),
      disabled: csv.teams.length === 0,
      disabledReason: 'No requests in this range.',
    },
    {
      label: 'Team budgets, this month',
      onSelect: () => analyticsCsv.teamBudgets(csv.teamRows),
      disabled: csv.teamRows.length === 0,
      disabledReason: 'No team has a monthly budget you can see.',
    },
    {
      label: `Provider pressure, ${range}`,
      onSelect: () => (pressure ? analyticsCsv.pressure(pressure, suffix) : undefined),
      disabled: !pressure?.items.length,
      disabledReason: 'No provider reported its rate limits in this range.',
    },
    { label: 'Daily spend, this month', onSelect: () => analyticsCsv.dailySpend(csv.month) },
  ];
}
