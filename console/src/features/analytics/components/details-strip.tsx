'use client';

import type { ReactNode } from 'react';

import type { KpiDelta } from '@/components/ui/kpi';
import { cn } from '@/lib/cn';
import { formatCount, formatLatency, formatMoney, formatPercent, plural } from '@/lib/format';

import type { LatencySlo, Transition } from '../extras';

function Cell({ label, children, foot }: { label: string; children: ReactNode; foot?: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-1 bg-raised px-gutter py-3 max-sm:px-gutter-sm">
      <span className="text-caption text-muted">{label}</span>
      <div className="min-w-0 text-ui text-fg">{children}</div>
      {foot ? <div className="text-caption text-muted">{foot}</div> : null}
    </div>
  );
}

const DELTA_TEXT: Record<KpiDelta['tone'], string> = {
  good: 'text-success',
  bad: 'text-error',
  neutral: 'text-muted',
};

export interface Savings {
  perDay: number;
  perMonth: number;
  /** The mean saved per day over the last 7 days, or null while it loads. */
  weekMean: number | null;
}

export interface FlagSplit {
  categories: { name: string; count: number }[];
  errors: number;
}

/**
 * The KPIs' details: savings a day and a month against the 7-day mean, the most common fallback,
 * flags by category with errors, and p95 against the latency SLO.
 */
export function DetailsStrip({
  savings,
  transition,
  transitionLoading,
  fallbacks,
  flags,
  p95,
  slo,
  sloKnown,
}: {
  savings: Savings | null;
  transition: Transition | null;
  transitionLoading: boolean;
  fallbacks: number;
  flags: FlagSplit | null;
  p95: number | null;
  slo: LatencySlo | null;
  /** Whether the reader may read the SLOs (configuration readers only). */
  sloKnown: boolean;
}) {
  const vsWeek =
    savings && savings.weekMean !== null && savings.weekMean > 0
      ? (savings.perDay - savings.weekMean) / savings.weekMean
      : null;
  const weekDelta: KpiDelta | null =
    vsWeek === null
      ? null
      : Math.abs(vsWeek) < 0.005
        ? { text: 'no change', tone: 'neutral', title: '' }
        : {
            text: `${vsWeek > 0 ? '+' : '−'}${formatPercent(Math.abs(vsWeek), Math.abs(vsWeek) >= 0.1 ? 0 : 1)}`,
            tone: vsWeek > 0 ? 'good' : 'bad',
            title: '',
          };
  const over = slo && p95 !== null ? p95 > slo.thresholdMs : false;
  return (
    <section
      aria-label="Details of the key figures"
      className="grid grid-cols-1 gap-px border-b border-line bg-line sm:grid-cols-2 xl:grid-cols-4"
    >
      <Cell
        label="Cache savings"
        foot={
          weekDelta ? (
            <>
              <span className={DELTA_TEXT[weekDelta.tone]}>{weekDelta.text}</span> against the 7-day mean of{' '}
              {formatMoney(savings?.weekMean ?? 0)} a day
            </>
          ) : savings ? (
            'Nothing saved in the last 7 days to compare with'
          ) : null
        }
      >
        {savings ? (
          <span className="mono tabular-nums">
            <span className="font-semibold text-success">{formatMoney(savings.perDay)}</span> a day · ~
            {formatMoney(savings.perMonth, { compact: true })} a month
          </span>
        ) : (
          <span className="text-muted">—</span>
        )}
      </Cell>
      <Cell
        label="Most common fallback"
        foot={
          transition && transition.count > 0
            ? `${plural(transition.count, 'recent request')} took this path; ${formatCount(transition.served)} served after a fallback`
            : fallbacks > 0
              ? plural(fallbacks, 'request') + ' served after a fallback'
              : null
        }
      >
        {fallbacks === 0 ? (
          <span className="text-muted">No fallbacks in this range</span>
        ) : transition?.from ? (
          <span className="mono flex min-w-0 items-center gap-1.5">
            <span className="truncate">{transition.from}</span>
            <span className="text-muted">→</span>
            <span className="truncate font-semibold">{transition.to}</span>
          </span>
        ) : transition ? (
          <span className="mono truncate">
            to <span className="font-semibold">{transition.to}</span>
          </span>
        ) : (
          <span className="text-muted">{transitionLoading ? 'Reading the attempts…' : '—'}</span>
        )}
      </Cell>
      <Cell
        label="Guardrail flags by category"
        foot={flags ? `${plural(flags.errors, 'request')} failed` : null}
      >
        {flags && flags.categories.length > 0 ? (
          <span className="flex flex-wrap gap-x-3 gap-y-0.5">
            {flags.categories.slice(0, 4).map((c) => (
              <span key={c.name}>
                <span className="mono font-semibold tabular-nums">{formatCount(c.count)}</span>{' '}
                <span className="text-muted">{c.name}</span>
              </span>
            ))}
          </span>
        ) : (
          <span className="text-muted">{flags ? 'No flags in this range' : '—'}</span>
        )}
      </Cell>
      <Cell
        label="p95 against the latency objective"
        foot={
          slo
            ? `${slo.name}: ${formatPercent(slo.objective, 0)} of requests under ${formatLatency(slo.thresholdMs)}`
            : sloKnown
              ? 'No gateway-wide latency objective is set'
              : null
        }
      >
        {p95 === null ? (
          <span className="text-muted">—</span>
        ) : (
          <span className="mono tabular-nums">
            <span className={cn('font-semibold', over ? 'text-warn' : slo ? 'text-success' : 'text-fg')}>
              {formatLatency(p95)}
            </span>
            {slo ? (
              <span className="text-muted">
                {' '}
                {over ? 'over' : 'under'} {formatLatency(slo.thresholdMs)}
              </span>
            ) : null}
          </span>
        )}
      </Cell>
    </section>
  );
}
