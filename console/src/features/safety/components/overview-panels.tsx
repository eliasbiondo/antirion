'use client';

import { useEffect, useMemo } from 'react';

import { Badge } from '@/components/ui/badge';
import { type ChartTone, Legend, Sparkline, TimeChart } from '@/components/ui/charts';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { Panel } from '@/components/ui/layout';
import { EmptyState, isUnavailable, LoadingRows, UnavailableState } from '@/components/ui/states';
import { ProviderName } from '@/components/ui/marks';
import { formatCount, formatPercent, formatPerMinute, plural } from '@/lib/format';
import { usePreferences } from '@/stores/preferences';
import { useProviderType } from '@/stores/provider-types';

import { bucketLabels } from '../chart';
import { CATEGORY_LABEL, useSafety } from '../store';

/** The posture score, its 30-day series and each check with its state. */
export function PosturePanel() {
  const posture = useSafety((s) => s.posture);
  const load = useSafety((s) => s.loadPosture);
  const canConfig = useCan('exportConfig');
  const canUsers = useCan('listUsers');
  const canKeys = useCan('listKeys');
  useEffect(() => {
    if (canConfig) void load({ users: canUsers, keys: canKeys });
  }, [canConfig, canUsers, canKeys, load]);
  if (!canConfig) return null;
  const data = posture.data;
  const pct = data?.score ?? null;
  return (
    <Panel
      title="Posture"
      subtitle="Safety checks passing, last 30 days"
      actions={
        data ? (
          <span className="w-32">
            <Sparkline values={data.series} tone="success" label="Posture over the last 30 days" />
          </span>
        ) : null
      }
    >
      {posture.status === 'loading' && !data ? <LoadingRows rows={3} /> : null}
      {posture.error ? (
        isUnavailable(posture.error) ? (
          <UnavailableState subject="Posture" size="sm" />
        ) : (
          <p className="text-ui text-muted">The configuration couldn&apos;t be read.</p>
        )
      ) : null}
      {data ? (
        <div className="flex flex-col gap-3">
          <p className="flex items-baseline gap-2">
            <span
              className={`mono text-kpi leading-7 font-semibold tracking-[-0.01em] tabular-nums ${pct === null ? 'text-muted' : pct >= 0.99 ? 'text-success' : pct >= 0.5 ? 'text-warn' : 'text-error'}`}
            >
              {pct === null ? '—' : formatPercent(pct, 0)}
            </span>
            <span className="text-ui text-muted">
              {data.checks.filter((c) => c.pass).length} of{' '}
              {data.checks.filter((c) => c.pass !== null).length} checks pass
            </span>
          </p>
          <ul className="flex flex-col gap-1.5" aria-label="Safety checks">
            {data.checks.map((c) => (
              <li key={c.id} className="flex items-start gap-2 text-ui">
                <span
                  className={`mt-0.5 shrink-0 ${c.pass === null ? 'text-dim' : c.pass ? 'text-success' : 'text-error'}`}
                >
                  <Icon
                    name={
                      c.notApplicable
                        ? 'minus'
                        : c.pass === null
                          ? 'circleDashed'
                          : c.pass
                            ? 'checkCircle'
                            : 'alertCircle'
                    }
                    size={14}
                  />
                </span>
                <span className="min-w-0">
                  <span className="text-fg">{c.label}</span>
                  <span className="sr-only">
                    {c.notApplicable
                      ? ' (not applicable)'
                      : c.pass === null
                        ? ' (unknown)'
                        : c.pass
                          ? ' (passes)'
                          : ' (fails)'}
                  </span>
                  <span className="block text-caption text-muted">{c.detail}</span>
                </span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </Panel>
  );
}

/** Requests in and out over the last hour, and what the guardrails took out on the way. */
export function DataFlowPanel() {
  const lastHour = useSafety((s) => s.lastHour);
  const runtime = useSafety((s) => s.runtime.data);
  const canUsage = useCan('getUsageAnalytics');
  const typeOf = useProviderType();
  const flow = useMemo(() => {
    const rows = lastHour.data ?? [];
    const sum = (f: (r: (typeof rows)[number]) => number) => rows.reduce((a, r) => a + f(r), 0);
    const providers = rows
      .filter((r) => r.group.provider)
      .map((r) => ({
        name: r.group.provider ?? '',
        perMin: Math.max(0, r.requests - (r.guardrail_blocks ?? 0)) / 60,
      }))
      .filter((p) => p.perMin > 0)
      .sort((a, b) => b.perMin - a.perMin);
    return {
      inbound: sum((r) => r.requests) / 60,
      blocked: sum((r) => r.guardrail_blocks ?? 0) / 60,
      redacted: sum((r) => r.guardrail_redactions ?? 0) / 60,
      outbound: providers.reduce((a, p) => a + p.perMin, 0),
      providers,
    };
  }, [lastHour.data]);
  if (!canUsage) return null;
  const active = (runtime ?? []).filter(
    (g) => g.enabled && (g.action === 'redact' || g.action === 'block'),
  ).length;
  const rate = (n: number) => formatPerMinute(n);
  return (
    <Panel title="Data flow" subtitle="Last hour, per minute">
      {lastHour.status === 'loading' && !lastHour.data ? <LoadingRows rows={3} /> : null}
      {lastHour.error && isUnavailable(lastHour.error) ? (
        <UnavailableState subject="Traffic" size="sm" />
      ) : null}
      {lastHour.data ? (
        <div className="flex flex-col gap-4">
          <div
            className="grid grid-cols-1 items-center gap-3 border-y border-line py-3 sm:grid-cols-[1fr_auto_1fr_auto_1fr]"
            aria-label="Requests from your apps through the guardrails to the providers"
          >
            <div className="flex flex-col gap-1">
              <span className="text-caption text-muted">Your apps</span>
              <span className="mono text-title text-fg tabular-nums">{rate(flow.inbound)}</span>
              <span className="text-caption text-muted">in</span>
            </div>
            <span className="hidden text-dim sm:block" aria-hidden="true">
              <Icon name="arrowRight" size={16} />
            </span>
            <div className="flex flex-col gap-1 border-l-2 border-accent pl-3">
              <span className="text-caption text-muted">Guardrails</span>
              <span className="mono text-title text-fg tabular-nums">
                {formatPerMinute(flow.blocked + flow.redacted, { taken: true })}
              </span>
              <span className="text-caption text-muted">{plural(active, 'rule')} blocking or redacting</span>
            </div>
            <span className="hidden text-dim sm:block" aria-hidden="true">
              <Icon name="arrowRight" size={16} />
            </span>
            <div className="flex flex-col gap-1">
              <span className="text-caption text-muted">Providers</span>
              <span className="mono text-title text-fg tabular-nums">{rate(flow.outbound)}</span>
              <span className="text-caption text-muted">out</span>
            </div>
          </div>
          {flow.providers.length > 0 ? (
            <ul className="flex flex-col gap-1" aria-label="Out per provider">
              {flow.providers.slice(0, 5).map((p) => (
                <li key={p.name} className="flex items-center justify-between gap-3 text-ui">
                  <ProviderName type={typeOf(p.name)} name={p.name} />
                  <span className="mono text-muted tabular-nums">{rate(p.perMin)}</span>
                </li>
              ))}
            </ul>
          ) : (
            <EmptyState size="sm" title="No requests reached a provider in the last hour" />
          )}
          <Legend
            items={[
              { label: 'Blocked', tone: 'error', value: rate(flow.blocked) },
              { label: 'Redacted', tone: 'warn', value: rate(flow.redacted) },
            ]}
          />
        </div>
      ) : null}
    </Panel>
  );
}

const CATEGORY_TONE: Record<string, ChartTone> = {
  pii: 'warn',
  secret: 'error',
  prompt_injection: 'info',
  keyword: 'accent',
  deny_pattern: 'success',
};
const TONES: ChartTone[] = ['accent', 'info', 'success', 'warn', 'error'];

const STEP: Record<string, number> = {
  '1m': 60_000,
  '5m': 300_000,
  '15m': 900_000,
  '1h': 3_600_000,
  hour: 3_600_000,
  '6h': 21_600_000,
  '1d': 86_400_000,
  day: 86_400_000,
};

/** Every bucket start of the window, so quiet buckets show as empty bars instead of disappearing. */
export function bucketStarts(from: string, to: string, interval: string, seen: string[]): string[] {
  const step = STEP[interval];
  const start = Date.parse(from);
  const end = Date.parse(to);
  if (!step || !Number.isFinite(start) || !Number.isFinite(end) || (end - start) / step > 400)
    return [...new Set(seen)].sort();
  const out = new Set<number>();
  for (let t = Math.floor(start / step) * step; t < end; t += step) out.add(t);
  for (const s of seen) out.add(Date.parse(s));
  return [...out].sort((a, b) => a - b).map((t) => new Date(t).toISOString());
}

/** Safety events per bucket, stacked by category. */
export function ActivityChart({ from, to, interval }: { from: string; to: string; interval: string }) {
  const byCategory = useSafety((s) => s.byCategory);
  const timeZone = usePreferences((s) => s.timeZone);
  const canSummary = useCan('getSafetySummary');
  const chart = useMemo(() => {
    const rows = (byCategory.data ?? []).map((r) => ({
      ...r,
      start: r.start ? new Date(r.start).toISOString() : null,
    }));
    const starts = bucketStarts(from, to, interval, rows.map((r) => r.start ?? '').filter(Boolean));
    const totals = new Map<string, number>();
    for (const r of rows) {
      const c = r.group.category ?? 'custom';
      totals.set(c, (totals.get(c) ?? 0) + r.events);
    }
    const top = [...totals.entries()].sort((a, b) => b[1] - a[1]).map(([c]) => c);
    const shown = top.slice(0, 5);
    const used = new Set<ChartTone>();
    const tone = (c: string, i: number): ChartTone => {
      const t =
        CATEGORY_TONE[c] && !used.has(CATEGORY_TONE[c])
          ? CATEGORY_TONE[c]
          : (TONES.find((x) => !used.has(x)) ?? TONES[i % TONES.length] ?? 'accent');
      used.add(t);
      return t;
    };
    const series = shown.map((c, i) => ({
      name: CATEGORY_LABEL[c] ?? c,
      tone: tone(c, i),
      values: starts.map((s) =>
        rows
          .filter((r) => r.start === s && (r.group.category ?? 'custom') === c)
          .reduce((a, r) => a + r.events, 0),
      ),
    }));
    if (top.length > shown.length)
      series.push({
        name: 'Other',
        tone: 'muted',
        values: starts.map((s) =>
          rows
            .filter((r) => r.start === s && !shown.includes(r.group.category ?? 'custom'))
            .reduce((a, r) => a + r.events, 0),
        ),
      });
    const labels = bucketLabels(starts, timeZone);
    return { series, labels, total: [...totals.values()].reduce((a, n) => a + n, 0) };
  }, [byCategory.data, timeZone, from, to, interval]);
  if (!canSummary) return null;
  return (
    <Panel
      title="Events over time"
      subtitle={byCategory.data ? `${plural(chart.total, 'event')} by category` : undefined}
      actions={
        chart.series.length > 0 ? (
          <Legend
            className="max-sm:hidden"
            items={chart.series.map((s) => ({ label: s.name, tone: s.tone }))}
          />
        ) : null
      }
    >
      {byCategory.status === 'loading' && !byCategory.data ? <LoadingRows rows={3} /> : null}
      {byCategory.error && isUnavailable(byCategory.error) ? (
        <UnavailableState subject="Event counts" size="sm" />
      ) : null}
      {byCategory.data ? (
        <TimeChart
          kind="stacked-bars"
          title="Safety events per bucket, stacked by category"
          labels={chart.labels}
          labelWidth={96}
          height={150}
          emptyLabel="No safety events in this range"
          series={
            chart.series.length > 0
              ? chart.series
              : [{ name: 'Events', tone: 'accent', values: chart.labels.map(() => 0) }]
          }
          formatValue={(v) => formatCount(v)}
        />
      ) : null}
    </Panel>
  );
}

export function CountBadge({ n }: { n: number }) {
  return <Badge tone={n > 0 ? 'warn' : 'neutral'}>{formatCount(n)}</Badge>;
}
