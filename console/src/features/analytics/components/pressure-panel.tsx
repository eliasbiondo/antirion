'use client';

import { Badge } from '@/components/ui/badge';
import { Legend, Meter, TimeChart, type ChartTone } from '@/components/ui/charts';
import { Panel } from '@/components/ui/layout';
import { ProviderName } from '@/components/ui/marks';
import { EmptyState, ErrorState, isUnavailable, LoadingRows, UnavailableState } from '@/components/ui/states';
import { formatCount, formatPercent, plural } from '@/lib/format';
import { formatClock } from '@/lib/time';
import { useProviderType } from '@/stores/provider-types';

import type { ProviderPressure } from '../api';

const TONES: ChartTone[] = ['accent', 'warn', 'info', 'success', 'error', 'muted'];

type Bucket = ProviderPressure['items'][number]['buckets'][number];

/** A bucket's pressure: the larger of the request and token utilizations. */
function peakOf(b: Bucket): number | null {
  const values = [b.rpm_utilization, b.tpm_utilization].filter((v): v is number => typeof v === 'number');
  return values.length ? Math.max(...values) : null;
}

export interface PressureRow {
  provider: string;
  /** The newest bucket with a reading. */
  current: number | null;
  /** Which quota the current figure is ("requests" or "tokens"). */
  unit: 'requests' | 'tokens' | null;
  used: number | null;
  limit: number | null;
  peak: number | null;
  hot: boolean;
}

/** Per provider: its newest reading, its peak over the window, and whether it reached the threshold. */
export function pressureRows(p: ProviderPressure): PressureRow[] {
  return p.items
    .map((item) => {
      const latest = [...item.buckets].reverse().find((b) => peakOf(b) !== null);
      const peaks = item.buckets.map(peakOf).filter((v): v is number => v !== null);
      const peak = peaks.length ? Math.max(...peaks) : null;
      let unit: PressureRow['unit'] = null;
      let used: number | null = null;
      let limit: number | null = null;
      if (latest) {
        const tokens = (latest.tpm_utilization ?? -1) > (latest.rpm_utilization ?? -1);
        unit = tokens ? 'tokens' : 'requests';
        limit = tokens ? latest.tokens_limit : latest.requests_limit;
        const share = tokens ? latest.tpm_utilization : latest.rpm_utilization;
        used = limit !== null && share !== null ? Math.round(share * limit) : null;
      }
      return {
        provider: item.provider,
        current: latest ? peakOf(latest) : null,
        unit,
        used,
        limit,
        peak,
        hot: peak !== null && peak >= p.threshold,
      };
    })
    .sort((a, b) => (b.current ?? -1) - (a.current ?? -1));
}

/**
 * Provider rate-limit pressure: each provider's use of its upstream quota over the window against
 * the 80 % line, the hot-provider count (against the comparison window), and the newest reading.
 */
export function PressurePanel({
  pressure,
  previous,
  error,
  timeZone,
  versus,
}: {
  pressure: ProviderPressure | null;
  previous: ProviderPressure | null;
  error: unknown;
  timeZone: string;
  versus: string;
}) {
  const typeOf = useProviderType();
  const rows = pressure ? pressureRows(pressure) : [];
  const hot = rows.filter((r) => r.hot).length;
  const before = previous ? (previous.hot[0]?.providers ?? 0) : null;
  const threshold = pressure?.threshold ?? 0.8;
  const starts = pressure?.items[0]?.buckets.map((b) => b.start) ?? pressure?.hot.map((h) => h.start) ?? [];
  return (
    <Panel
      title="Provider rate-limit pressure"
      actions={
        pressure && rows.length > 0 ? (
          <Badge tone={hot > 0 ? 'warn' : 'neutral'}>
            {hot} hot{before !== null ? ` · ${before} ${versus}` : ''}
          </Badge>
        ) : null
      }
    >
      {error && !pressure ? (
        isUnavailable(error) ? (
          <UnavailableState subject="Pressure history" size="sm" />
        ) : (
          <ErrorState error={error} subject="Pressure" size="sm" />
        )
      ) : !pressure ? (
        <LoadingRows rows={4} label="Loading pressure" />
      ) : rows.length === 0 ? (
        <EmptyState
          size="sm"
          title="No provider reported its rate limits in this range"
          description="Pressure shows once upstream answers carry quota headers."
        />
      ) : (
        <div className="flex flex-col gap-3">
          <TimeChart
            kind="line"
            gaps
            title="Upstream quota used per provider over time"
            labels={starts.map((s) => formatClock(s, timeZone).slice(0, 5))}
            series={pressure.items.map((item, i) => ({
              name: item.provider,
              values: item.buckets.map((b) => {
                const v = peakOf(b);
                return v === null ? null : v * 100;
              }),
              tone: TONES[i % TONES.length] ?? 'accent',
            }))}
            reference={{ value: threshold * 100, label: `${formatPercent(threshold, 0)} of quota` }}
            formatValue={(v) => `${Math.round(v)}%`}
            height={170}
          />
          <Legend
            items={pressure.items.map((item, i) => ({
              label: item.provider,
              tone: TONES[i % TONES.length] ?? 'accent',
            }))}
          />
          <ul aria-label="Pressure by provider" className="flex flex-col gap-2">
            {rows.map((r) => {
              const tone: ChartTone =
                r.current === null
                  ? 'muted'
                  : r.current >= 0.95
                    ? 'error'
                    : r.current >= threshold
                      ? 'warn'
                      : 'accent';
              return (
                <li
                  key={r.provider}
                  className="grid grid-cols-[minmax(0,140px)_minmax(60px,1fr)_auto] items-center gap-3 text-ui max-sm:grid-cols-[minmax(0,1fr)_auto] max-sm:gap-y-1"
                >
                  <ProviderName type={typeOf(r.provider)} name={r.provider} />
                  <span className="flex items-center gap-2 max-sm:order-last max-sm:col-span-2">
                    <Meter
                      value={r.current ?? 0}
                      max={1}
                      tick={threshold}
                      tone={tone}
                      label={`${r.provider}: ${r.current === null ? 'no reading' : formatPercent(r.current, 0)} of its quota`}
                    />
                    <span className="mono w-10 shrink-0 text-right text-caption text-muted tabular-nums">
                      {r.current === null ? '—' : formatPercent(r.current, 0)}
                    </span>
                  </span>
                  <span className="mono text-right text-caption text-muted tabular-nums">
                    {r.used !== null && r.limit !== null ? (
                      <>
                        <span className="text-fg">{formatCount(r.used, { compact: true })}</span> /{' '}
                        {formatCount(r.limit, { compact: true })} {r.unit === 'tokens' ? 'tpm' : 'rpm'}
                      </>
                    ) : (
                      '—'
                    )}
                    {r.peak !== null ? (
                      <span className="block text-dim">peak {formatPercent(r.peak, 0)}</span>
                    ) : null}
                  </span>
                </li>
              );
            })}
          </ul>
          <p className="text-caption text-muted">
            {plural(hot, 'provider')} reached {formatPercent(threshold, 0)} of a quota in this range.
          </p>
        </div>
      )}
    </Panel>
  );
}
