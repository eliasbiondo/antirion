'use client';

import { useState } from 'react';

import { Sparkline } from '@/components/ui/charts';
import { Segmented } from '@/components/ui/form';
import { SummaryStrip } from '@/components/ui/layout';
import type { Alert } from '@/lib/api/types';
import { formatCount, formatPercent, plural } from '@/lib/format';
import { formatClock } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { dayMonth, spanWords } from '../describe';
import {
  budgetBurn,
  DAY_MS,
  fireBuckets,
  meanTimeToResolve,
  noiseRatio,
  notificationsSince,
  SEVERITIES,
} from '../stats';
import { useAlerts } from '../store';

const FILL = { critical: 'fill-error', warning: 'fill-warn', info: 'fill-info' } as const;

/** Alerts fired per hour over a day, or per day over 30 days, stacked by severity. */
function FiredChart({ history, now }: { history: Alert[]; now: number }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const [span, setSpan] = useState<'24h' | '30d'>('24h');
  const buckets = fireBuckets(history, span, now);
  const totals = buckets.map((b) => b.counts.critical + b.counts.warning + b.counts.info);
  const peak = Math.max(1, ...totals);
  const total = totals.reduce((a, b) => a + b, 0);
  const width = 240;
  const step = width / buckets.length;
  const when = (t: number) =>
    span === '24h' ? formatClock(new Date(t).toISOString(), timeZone).slice(0, 5) : dayMonth(t, timeZone);
  return (
    <div className="flex min-w-0 flex-col bg-raised px-gutter py-2.5 max-sm:px-gutter-sm">
      <div className="flex items-center justify-between gap-2 text-caption">
        <span className="text-muted">
          Alerts fired <span className="mono text-fg tabular-nums">{formatCount(total)}</span>
        </span>
        <Segmented<'24h' | '30d'>
          label="Fires over"
          value={span}
          onChange={setSpan}
          options={[
            { value: '24h', label: '24 h' },
            { value: '30d', label: '30 days' },
          ]}
        />
      </div>
      <svg
        viewBox={`0 0 ${width} 40`}
        preserveAspectRatio="none"
        className="mt-1.5 block h-10 w-full"
        role="img"
        aria-label={`${plural(total, 'alert')} fired in the last ${span === '24h' ? '24 hours' : '30 days'}`}
      >
        <line
          x1="0"
          x2={width}
          y1="39.5"
          y2="39.5"
          className="stroke-line"
          vectorEffect="non-scaling-stroke"
        />
        {buckets.map((b, i) => {
          let base = 40;
          return (
            <g key={b.start}>
              <title>{`${when(b.start)}: ${plural(totals[i] ?? 0, 'alert')}`}</title>
              {SEVERITIES.map((sv) => {
                const h = (b.counts[sv] / peak) * 38;
                if (h <= 0) return null;
                base -= h;
                return (
                  <rect
                    key={sv}
                    x={i * step + step * 0.15}
                    y={base}
                    width={step * 0.7}
                    height={h}
                    className={FILL[sv]}
                  />
                );
              })}
            </g>
          );
        })}
      </svg>
      <div className="mono mt-1 flex justify-between text-caption text-dim">
        <span>{span === '24h' ? '24 h ago' : '30 days ago'}</span>
        <span>now</span>
      </div>
    </div>
  );
}

/**
 * The alerting figures over the last 30 days: what fires now, time to resolve against the 30 days
 * before, notifications of the last day by channel, noise, the error budget with its burn, and the
 * fires over time.
 */
export function AlertsHero({ inbox, now, onFiring }: { inbox: Alert[]; now: number; onFiring: () => void }) {
  const history = useAlerts((s) => s.history);
  const slos = useAlerts((s) => s.slos.data);
  const availability = useAlerts((s) => s.availability.data);
  const firing = inbox.filter((a) => a.status === 'firing');
  const bySeverity = SEVERITIES.map((sv) => ({
    sv,
    n: firing.filter((a) => a.severity === sv).length,
  })).filter((x) => x.n > 0);
  const list = history.data ?? inbox;
  const monthAgo = now - 30 * DAY_MS;
  const mttr = meanTimeToResolve(list, monthAgo, now + 1);
  const before = history.data ? meanTimeToResolve(list, now - 60 * DAY_MS, monthAgo) : null;
  const trend = mttr !== null && before !== null && before > 0 ? (mttr - before) / before : null;
  const noise = noiseRatio(list, monthAgo, now + 1);
  const sent = notificationsSince(list, now - DAY_MS);
  const slo = slos?.find((s) => s.kind === 'availability') ?? slos?.[0];
  const burn = slo?.kind === 'availability' && availability ? budgetBurn(availability, slo.objective) : [];
  const left = slo?.error_budget_remaining;
  const ready = history.data !== null;
  return (
    <div className="grid gap-px border-b border-line bg-line min-[1400px]:grid-cols-[minmax(0,1fr)_300px]">
      <div className="bg-raised [&>section]:h-full [&>section]:border-b-0">
        <SummaryStrip
          label="Alert summary"
          items={[
            {
              label: 'Firing now',
              value: formatCount(firing.length),
              sub: bySeverity.length ? bySeverity.map((x) => `${x.n} ${x.sv}`).join(' · ') : 'Nothing firing',
              tone: firing.length > 0 ? 'error' : 'neutral',
              title: 'Show the alerts that are firing',
              onClick: onFiring,
            },
            {
              label: 'Time to resolve',
              value: mttr === null ? '—' : spanWords(mttr),
              sub:
                trend === null
                  ? ready
                    ? 'Last 30 days'
                    : 'Reading 30 days…'
                  : `30 days · ${trend <= 0 ? '↓' : '↑'} ${formatPercent(Math.abs(trend), 0)}`,
              tone: trend !== null && trend > 0.1 ? 'warn' : 'neutral',
              title:
                'Average time from firing to resolution of the alerts resolved in the last 30 days, against the 30 days before',
            },
            {
              label: 'Notifications, 24 h',
              value: formatCount(sent.total),
              sub: sent.byChannel.length
                ? sent.byChannel
                    .slice(0, 2)
                    .map((c) => `${c.channel} ${formatCount(c.n)}`)
                    .join(' · ') + (sent.failed ? ` · ${formatCount(sent.failed)} failed` : '')
                : sent.failed
                  ? `${formatCount(sent.failed)} failed`
                  : 'None sent',
              tone: sent.failed > 0 ? 'warn' : 'neutral',
              title: 'Notifications the alerts sent in the last 24 hours, by channel',
            },
            {
              label: 'Noise, 30 days',
              value: noise === null ? '—' : formatPercent(noise, 0),
              sub: 'Resolved within a minute',
              tone: noise !== null && noise > 0.2 ? 'warn' : 'neutral',
              title: 'Share of the alerts fired in the last 30 days that resolved within a minute',
            },
            {
              label: 'Error budget left',
              value: left == null ? 'Not set' : left < 0 ? 'Used up' : formatPercent(left, 0),
              sub:
                burn.length >= 2 ? (
                  <Sparkline
                    values={burn}
                    tone={left != null && left < 0.25 ? 'warn' : 'success'}
                    label={`Error budget left over the ${slo?.window ?? ''} window`}
                    className="mt-0.5"
                  />
                ) : slo?.burn_rate_1h != null ? (
                  `Burn ${slo.burn_rate_1h.toFixed(1)}×, last hour`
                ) : slo ? (
                  `${formatPercent(slo.objective, 2)} over ${slo.window}`
                ) : (
                  'No target set'
                ),
              tone: left == null ? 'neutral' : left < 0 ? 'error' : left < 0.25 ? 'warn' : 'neutral',
              title: slo
                ? `${slo.name}: ${formatPercent(slo.objective, 2)} ${slo.kind} target over ${slo.window}${slo.burn_rate_1h != null ? `, burning ${slo.burn_rate_1h.toFixed(1)}× in the last hour` : ''}`
                : 'No availability target is set',
            },
          ]}
        />
      </div>
      <FiredChart history={list} now={now} />
    </div>
  );
}
