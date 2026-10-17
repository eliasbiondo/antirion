'use client';

import { useEffect } from 'react';

import { BarRow, TimeChart } from '@/components/ui/charts';
import { useCan } from '@/components/ui/gate';
import { DetailSection } from '@/components/ui/list-detail';
import { Tooltip } from '@/components/ui/tooltip';
import type { LatencyHistogram } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount, formatLatency, formatMoney, formatPercent, formatTokens } from '@/lib/format';
import { formatDate } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { byGroup, outputSpeed, sumBuckets } from '../catalog';
import { useCatalog, useModelInsight } from '../store';
import type { ModelView } from '../types';
import { errorRateThreshold } from './models-hero';

/** A histogram's counts merged into `n` buckets (the gateway's may have more), with their bounds. */
export function rebin(h: LatencyHistogram, n = 12): { counts: number[]; bounds: number[] } {
  if (h.counts.length <= n) return { counts: h.counts, bounds: h.bounds };
  const size = h.counts.length / n;
  const counts: number[] = [];
  const bounds: number[] = [h.bounds[0] ?? 0];
  for (let i = 0; i < n; i += 1) {
    const from = Math.round(i * size);
    const to = Math.round((i + 1) * size);
    counts.push(h.counts.slice(from, to).reduce((a, b) => a + b, 0));
    bounds.push(h.bounds[to] ?? h.bounds.at(-1) ?? 0);
  }
  return { counts, bounds };
}

/** The bucket a latency falls in, or -1. */
export function bucketOf(bounds: number[], value: number | null | undefined): number {
  if (value === null || value === undefined) return -1;
  for (let i = 0; i < bounds.length - 1; i += 1) if (value <= (bounds[i + 1] ?? Infinity)) return i;
  return bounds.length - 2;
}

/** Twelve latency buckets with the p50 and p95 buckets marked. */
function LatencyBars({ histogram }: { histogram: LatencyHistogram }) {
  const { counts, bounds } = rebin(histogram);
  const peak = Math.max(...counts, 1);
  const p50 = bucketOf(bounds, histogram.p50);
  const p95 = bucketOf(bounds, histogram.p95);
  const w = 240 / Math.max(1, counts.length);
  return (
    <figure className="m-0">
      <svg
        viewBox="0 0 240 56"
        className="block h-16 w-full"
        role="img"
        aria-label={`Latency over the last 30 days: p50 ${formatLatency(histogram.p50)}, p95 ${formatLatency(histogram.p95)}`}
      >
        {counts.map((c, i) => {
          const h = Math.max(1.5, (c / peak) * 52);
          return (
            <rect
              key={i}
              x={i * w + 1}
              y={56 - h}
              width={Math.max(1, w - 2)}
              height={h}
              rx={1.5}
              className={i === p95 ? 'fill-warn' : i === p50 ? 'fill-accent' : 'fill-dim'}
              fillOpacity={i === p50 || i === p95 ? 0.9 : 0.4}
            >
              <title>{`${formatLatency(bounds[i])} to ${formatLatency(bounds[i + 1])}: ${formatCount(c)} requests`}</title>
            </rect>
          );
        })}
      </svg>
      <figcaption className="mono mt-1 flex justify-between text-caption text-dim tabular-nums">
        <span>{formatLatency(bounds[0] ?? 0)}</span>
        <span className="flex gap-3">
          <span className="text-accent">p50 {formatLatency(histogram.p50)}</span>
          <span className="text-warn">p95 {formatLatency(histogram.p95)}</span>
        </span>
        <span>{formatLatency(bounds.at(-1) ?? 0)}</span>
      </figcaption>
    </figure>
  );
}

function Cell({
  label,
  value,
  sub,
  tone,
  hint,
}: {
  label: string;
  value: string;
  sub?: string | undefined;
  tone?: 'warn' | 'error' | undefined;
  hint?: string | undefined;
}) {
  const body = (
    <div className="min-w-0 bg-raised px-3.5 py-2.5">
      <div className="text-caption text-muted">{label}</div>
      <div
        className={cn(
          'mono mt-1 truncate text-body font-semibold tabular-nums',
          tone === 'warn' ? 'text-warn' : tone === 'error' ? 'text-error' : 'text-fg',
        )}
      >
        {value}
      </div>
      {sub ? <div className="mt-0.5 truncate text-caption text-muted">{sub}</div> : null}
    </div>
  );
  return hint ? (
    <Tooltip content={hint}>
      <div className="min-w-0">{body}</div>
    </Tooltip>
  ) : (
    body
  );
}

/**
 * A Model's last 30 days: requests and their share of all traffic, spend, tokens, errors against the
 * error-rate rule, cancels, cache hits and prompt-cache reads, output speed, time to first token,
 * latency percentiles and their spread, requests per day, and the teams that use it most.
 */
export function ModelUsage({ view }: { view: ModelView }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const usage = useCatalog((s) => s.usage);
  const rules = useCatalog((s) => s.rules.data);
  const insight = useModelInsight((s) => s.slots[view.name]);
  const load = useModelInsight((s) => s.load);
  const canUsage = useCan('getUsageAnalytics');
  const canAlerts = useCan('listAlerts');
  useEffect(() => {
    if (canUsage) void load(view.name, { alerts: canAlerts });
  }, [canUsage, canAlerts, load, view.name]);

  if (!canUsage) return null;
  const row = usage.data?.get(view.name);
  const buckets = row?.buckets ?? [];
  const t = sumBuckets(buckets);
  const all = [...(usage.data?.values() ?? [])].reduce((a, u) => a + u.requests, 0);
  const threshold = errorRateThreshold(rules, view.name);
  const errorRate = t.requests > 0 ? t.errors / t.requests : 0;
  const teams = insight?.data ? byGroup(insight.data.teams, 'team') : [];
  const histogram = insight?.data?.histogram;
  const promptShare = t.input > 0 ? t.cacheRead / t.input : null;

  if (usage.data && t.requests === 0)
    return (
      <DetailSection title="Usage, last 30 days">
        <p className="text-ui text-muted">No requests in the last 30 days.</p>
      </DetailSection>
    );
  if (!usage.data) return null;

  return (
    <DetailSection title="Usage, last 30 days">
      <div className="grid grid-cols-2 gap-px border-y border-line bg-line sm:grid-cols-4">
        <Cell
          label="Requests"
          value={formatCount(t.requests)}
          sub={all > 0 ? `${formatPercent(t.requests / all, 1)} of all traffic` : undefined}
        />
        <Cell
          label="Spend"
          value={formatMoney(t.cost)}
          sub={t.requests > 0 ? `${formatMoney(t.cost / t.requests)} a request` : undefined}
        />
        <Cell
          label="Tokens"
          value={formatTokens(t.input + t.output)}
          sub={`${formatTokens(t.input)} in · ${formatTokens(t.output)} out`}
        />
        <Cell
          label="Error rate"
          value={formatPercent(errorRate, 2)}
          tone={threshold !== null && errorRate > threshold ? 'warn' : undefined}
          sub={`${formatCount(t.errors)} failed${threshold !== null ? ` · rule ${formatPercent(threshold, 1)}` : ''}`}
          hint={threshold !== null ? 'Against the threshold of the error-rate alert rule' : undefined}
        />
        <Cell
          label="Cancelled"
          value={formatPercent(t.cancelled / Math.max(1, t.requests), 2)}
          sub="apps that hung up"
        />
        <Cell
          label="Cache hits"
          value={formatPercent(t.cacheHits / Math.max(1, t.requests), 1)}
          sub={
            promptShare !== null
              ? `prompt cache ${formatPercent(promptShare, 1)} of input`
              : 'gateway response cache'
          }
          hint="Answers served from the gateway's response cache; the prompt-cache share is input tokens the provider read from its cache"
        />
        <Cell
          label="Output speed"
          value={outputSpeed(t.tokensPerSecond) !== null ? `${outputSpeed(t.tokensPerSecond)} tok/s` : '—'}
          sub={
            outputSpeed(t.tokensPerSecond) === null && t.tokensPerSecond !== null
              ? 'not measured reliably'
              : t.ttftP50 !== null
                ? `first token ${formatLatency(t.ttftP50)}`
                : 'streamed requests only'
          }
        />
        <Cell
          label="Latency p50"
          value={
            histogram?.p50 != null
              ? formatLatency(histogram.p50)
              : t.latencyP50 !== null
                ? formatLatency(t.latencyP50)
                : '—'
          }
          sub={`p95 ${formatLatency(histogram?.p95 ?? t.latencyP95)} · p99 ${formatLatency(histogram?.p99 ?? t.latencyP99)}`}
        />
      </div>
      <div className="mt-4 grid gap-5 lg:grid-cols-2">
        <div className="min-w-0">
          <div className="mb-1 text-caption text-muted">Requests per day</div>
          {row && row.days.length > 0 ? (
            <TimeChart
              title={`Requests per day for ${view.name}`}
              kind="bars"
              height={120}
              labels={row.days.map((d) => formatDate(d.start, timeZone))}
              series={[{ name: 'Requests', values: row.days.map((d) => d.requests), tone: 'accent' }]}
              formatValue={(v) => formatCount(v, { compact: true })}
            />
          ) : null}
        </div>
        <div className="min-w-0">
          <div className="mb-1 flex justify-between text-caption text-muted">
            <span>Latency spread</span>
            {t.ttftP95 !== null ? (
              <span>
                Time to first token p50 {formatLatency(t.ttftP50)} · p95 {formatLatency(t.ttftP95)}
              </span>
            ) : null}
          </div>
          {histogram?.counts.some((c) => c > 0) ? (
            <LatencyBars histogram={histogram} />
          ) : (
            <p className="text-caption text-dim">
              {insight?.status === 'loading' ? 'Loading…' : 'No latency recorded.'}
            </p>
          )}
        </div>
      </div>
      {teams.length > 0 ? (
        <div className="mt-4">
          <div className="mb-1.5 text-caption text-muted">Top teams</div>
          <ul aria-label={`Teams using ${view.name}`} className="flex flex-col gap-0.5">
            {teams.slice(0, 5).map((team) => (
              <BarRow
                key={team.name}
                name={<span className="truncate">{team.name}</span>}
                value={team.totals.requests}
                max={t.requests}
                label={`${team.name}: ${formatPercent(team.totals.requests / Math.max(1, t.requests), 0)} of requests`}
                display={
                  <>
                    {formatPercent(team.totals.requests / Math.max(1, t.requests), 0)}
                    <span className="text-dim"> · {formatMoney(team.totals.cost, { compact: true })}</span>
                  </>
                }
              />
            ))}
          </ul>
        </div>
      ) : null}
    </DetailSection>
  );
}
