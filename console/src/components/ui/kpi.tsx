'use client';

import type { ReactNode } from 'react';

import { cn } from '@/lib/cn';

import type { Tone } from './badge';
import { Sparkline, type ChartTone } from './charts';
import { Icon, type IconName } from './icon';
import { TileGrid } from './layout';
import { Skeleton } from './states';
import { Tooltip } from './tooltip';

export type KpiState = 'ready' | 'loading' | 'empty' | 'unavailable';

export interface KpiDelta {
  /** "+12.3%", "−0.4 pp". */
  text: string;
  /** `bad` is red, `good` green, `neutral` muted (no change, or a change that is neither). */
  tone: 'good' | 'bad' | 'neutral';
  /** The comparison, for the tooltip ("vs the previous 6 hours"). */
  title?: string;
}

export interface KpiTileProps {
  /** Sentence-case label ("Error rate"). */
  label: string;
  icon?: IconName;
  /** The figure. Never truncated: it shrinks a step in a narrow tile instead. */
  value?: ReactNode;
  /** A unit after the figure, smaller ("%", "ms", "K"). */
  unit?: ReactNode;
  valueTone?: Tone;
  delta?: KpiDelta | null | undefined;
  /** One line of context ("$0.0059 per request", "212 failed"). */
  context?: ReactNode;
  spark?: { values: (number | null | undefined)[]; tone?: ChartTone } | null | undefined;
  /**
   * `loading`: skeletons. `empty`: "—" and `emptyHint` (the source answered, there is nothing yet).
   * `unavailable`: "—" and "Not available" (the gateway does not serve the source; never a 0).
   */
  state?: KpiState;
  /** Under "—" while there is no data yet ("After the first request"). */
  emptyHint?: string;
  className?: string;
}

const VALUE_TONE: Record<Tone, string> = {
  neutral: 'text-fg',
  success: 'text-success',
  warn: 'text-warn',
  error: 'text-error',
  info: 'text-info',
  accent: 'text-accent',
};

const DELTA_TONE: Record<KpiDelta['tone'], string> = {
  good: 'text-success',
  bad: 'text-error',
  neutral: 'text-muted',
};

/**
 * A KPI tile as in the prototype: label with icon, a big tabular figure, context and the change
 * against the previous window, and a sparkline. It keeps its shape in every state.
 */
export function KpiTile({
  label,
  icon,
  value,
  unit,
  valueTone = 'neutral',
  delta,
  context,
  spark,
  state = 'ready',
  emptyHint,
  className,
}: KpiTileProps) {
  const blank = state === 'empty' || state === 'unavailable';
  return (
    <div className={cn('@container flex min-w-0 flex-col bg-raised px-4 pt-3.5 pb-3', className)}>
      <div className="flex items-center gap-1.5 text-caption font-medium text-muted">
        {icon ? <Icon name={icon} size={14} className="shrink-0 text-dim" /> : null}
        <span className="truncate">{label}</span>
      </div>
      {state === 'loading' ? (
        <div role="status" aria-label={`Loading ${label}`} className="flex flex-col gap-2 pt-2">
          <Skeleton className="h-6 w-24" />
          <Skeleton className="h-3 w-32" />
          <Skeleton className="mt-1 h-[22px] w-full" />
        </div>
      ) : (
        <>
          <div
            className={cn(
              'mono mt-1.5 text-[18px] leading-7 font-semibold tracking-[-0.01em] whitespace-nowrap tabular-nums @[180px]:text-kpi',
              blank ? 'text-dim' : VALUE_TONE[valueTone],
            )}
          >
            {blank ? '—' : value}
            {!blank && unit ? (
              <span className="ml-px font-sans text-ui font-normal text-muted">{unit}</span>
            ) : null}
          </div>
          <div className="mt-0.5 flex min-h-4 min-w-0 items-center gap-2 text-caption text-muted">
            {state === 'unavailable' ? (
              <Tooltip content="This gateway doesn't serve this figure yet">
                <span className="flex items-center gap-1 truncate">
                  <Icon name="info" size={12} className="shrink-0" />
                  Not available
                </span>
              </Tooltip>
            ) : state === 'empty' ? (
              <span className="truncate">{emptyHint ?? 'No data yet'}</span>
            ) : (
              <>
                {context ? <span className="min-w-0 truncate">{context}</span> : null}
                {delta ? (
                  <Tooltip content={delta.title}>
                    <span className={cn('mono shrink-0 tabular-nums', DELTA_TONE[delta.tone])}>
                      {delta.text}
                      {delta.title ? <span className="sr-only"> {delta.title}</span> : null}
                    </span>
                  </Tooltip>
                ) : null}
              </>
            )}
          </div>
          <div className="mt-2">
            {state === 'ready' && spark ? (
              <Sparkline values={spark.values} tone={spark.tone ?? 'accent'} label={`${label}, per bucket`} />
            ) : (
              <Sparkline values={[0, 0]} label={`${label}: no data`} />
            )}
          </div>
        </>
      )}
    </div>
  );
}

/**
 * The row of KPI tiles, separated by 1px lines: two columns on a phone, three on a tablet, all in
 * one row from 1280px.
 */
export function KpiStrip({
  children,
  label = 'Key figures',
  columns = 6,
  className,
}: {
  children: ReactNode;
  label?: string;
  columns?: 3 | 4 | 5 | 6;
  className?: string;
}) {
  return (
    <TileGrid label={label} cols={{ base: 2, md: 3, xl: columns }} className={className}>
      {children}
    </TileGrid>
  );
}
