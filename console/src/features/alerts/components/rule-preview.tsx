'use client';

import { useEffect, useMemo, useState } from 'react';

import { TimeChart } from '@/components/ui/charts';
import { LoadingRows } from '@/components/ui/states';
import { isUnavailable } from '@/components/ui/states';
import { describeError } from '@/lib/api/client';
import { plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { usePreferences } from '@/stores/preferences';

import { alertsApi, type RuleSpec } from '../api';
import { METRICS, spanWords, weekdayClock } from '../describe';
import {
  foldBuckets,
  needsBaseline,
  PREVIEW_DAYS,
  previewable,
  previewInterval,
  simulate,
  type PreviewBucket,
} from '../preview';
import { ruleStats } from '../stats';
import { useAlerts } from '../store';

type State =
  | { status: 'idle' | 'loading' }
  | { status: 'ready'; rows: PreviewBucket[]; at: number }
  | { status: 'error'; error: unknown };

/**
 * How often the rule would have fired: the metric over the last 7 days replayed against the
 * condition, with each fire marked; for what a replay cannot compute, the rule's own fires.
 */
export function RulePreview({ spec, existing }: { spec: RuleSpec; existing: string | null }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const history = useAlerts((s) => s.history.data);
  const [state, setState] = useState<State>({ status: 'idle' });
  const now = useNow(60_000);
  const metric = spec.kind === 'event' ? undefined : spec.metric;
  const can = previewable(metric);
  const { interval, ms } = previewInterval(spec.window);
  const scopeKey = JSON.stringify(spec.scope ?? {});

  useEffect(() => {
    if (!can) return;
    const controller = new AbortController();
    const timer = window.setTimeout(() => {
      const to = Date.now();
      const days = PREVIEW_DAYS + (needsBaseline(metric) ? 8 : 1);
      setState({ status: 'loading' });
      alertsApi
        .previewUsage({
          from: to - days * 86_400_000,
          to,
          interval,
          scope: JSON.parse(scopeKey) as RuleSpec['scope'],
          byStatus: metric === 'upstream_5xx_rate',
          signal: controller.signal,
        })
        .then((items) => setState({ status: 'ready', rows: foldBuckets(items), at: to }))
        .catch((error: unknown) => {
          if (!controller.signal.aborted) setState({ status: 'error', error });
        });
    }, 450);
    return () => {
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [can, metric, interval, scopeKey]);

  const result = useMemo(
    () => (state.status === 'ready' ? simulate(spec, state.rows, ms, state.at) : null),
    [state, spec, ms],
  );
  const own = existing && history ? ruleStats(history, existing, now) : null;
  const ownLine = own ? `It fired ${plural(own.fires, 'time')} in the last 30 days.` : null;

  if (!can) {
    return (
      <Frame>
        <p className="text-ui text-fg">
          {spec.kind === 'event'
            ? 'An event rule fires once for each event it subscribes to.'
            : `A replay can't compute ${METRICS[spec.metric ?? 'requests'].label.toLowerCase()}: it is read live from the gateway's limits and budgets.`}
        </p>
        {ownLine ? <p className="mt-1 text-ui text-muted">{ownLine}</p> : null}
      </Frame>
    );
  }
  if (state.status === 'error') {
    return (
      <Frame>
        <p className="text-ui text-muted">
          {isUnavailable(state.error)
            ? "This gateway doesn't serve the usage history a preview needs."
            : `The preview couldn't read usage: ${describeError(state.error)}`}
        </p>
        {ownLine ? <p className="mt-1 text-ui text-muted">{ownLine}</p> : null}
      </Frame>
    );
  }
  if (!result) {
    return (
      <Frame>
        <LoadingRows rows={2} label="Replaying the last 7 days" />
      </Frame>
    );
  }
  const m = METRICS[spec.metric ?? 'requests'];
  const known = result.values.filter((v) => v !== null).length;
  // Axis ticks read alike: "30%" beside "40%", not "30.0%".
  const formatValue = (v: number) =>
    m.value(m.unit === 'ratio' ? Number(v.toFixed(2)) : v).replace(/\.0(?=\D*$)/, '');
  const label = (t: number) => weekdayClock(t, timeZone);
  return (
    <Frame>
      <p className="text-ui text-fg">
        {known === 0 ? (
          result.measured > 0 && m.baseline ? (
            'No baseline yet: it compares each hour with the same hour a week earlier, and this scope had no traffic then.'
          ) : (
            'No traffic in the last 7 days matches this scope, so it would not have fired.'
          )
        ) : (
          <>
            Would have fired <strong className="font-semibold">{plural(result.fires.length, 'time')}</strong>{' '}
            in the last 7 days
            {result.held > 0 ? (
              <span className="text-muted">
                {' '}
                · the condition held for {spanWords((result.held * ms) / 1000)} in all
              </span>
            ) : null}
            .
          </>
        )}
      </p>
      {known > 0 ? (
        <TimeChart
          kind="line"
          title={`${m.label} over the last 7 days`}
          labels={result.starts.map(label)}
          series={[
            {
              name: m.label + (m.baseline ? ' against its baseline' : ''),
              values: result.values,
              tone: 'accent',
            },
          ]}
          formatValue={formatValue}
          reference={spec.threshold !== undefined ? { value: spec.threshold, label: 'Threshold' } : undefined}
          annotations={
            // Past a few dozen, flags hide the line; the sentence above has the count.
            result.fires.length <= 40
              ? result.fires.map((index) => ({ index, label: 'Would fire', tone: 'error' as const }))
              : undefined
          }
          gaps
          labelWidth={100}
          height={130}
          className="mt-3"
        />
      ) : null}
      <p className="mt-2 text-caption text-muted">
        Replayed on {interval === '1h' ? 'hourly' : interval === '15m' ? '15-minute' : '5-minute'} usage
        {spec.scope?.route ? '; the route is not part of usage history, so it is left out' : ''}.{' '}
        {ownLine ?? ''}
      </p>
    </Frame>
  );
}

function Frame({ children }: { children: React.ReactNode }) {
  return (
    <section aria-label="Preview" className="rounded-control border border-line bg-sunken px-3.5 py-3">
      <h3 className="mb-1.5 text-caption font-medium text-muted">Preview</h3>
      {children}
    </section>
  );
}
