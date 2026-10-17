'use client';

import Link from 'next/link';
import { useEffect, useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge, Dot } from '@/components/ui/badge';
import { buttonClass, Button } from '@/components/ui/button';
import { Sparkline, type ChartTone } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { ChipToggle, Field, Textarea } from '@/components/ui/form';
import { usePermission } from '@/components/ui/gate';
import { Panel } from '@/components/ui/layout';
import { EmptyState, ErrorState, isUnavailable, UnavailableState } from '@/components/ui/states';
import type { Alert, UsageBucket } from '@/lib/api/types';
import { capitalize, plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';

import { severityTone, useAlerts } from '@/features/alerts/store';

import { analyticsExtrasApi } from '../api';

/** The signal categories, in the order the filter offers them. */
const CATEGORIES = ['anomaly', 'capacity', 'budget', 'cache', 'latency'];

/** The figure a signal's mini-chart follows, by category. */
function metricOf(category: string, b: UsageBucket): number {
  if (category === 'latency') return b.latency_ms?.p95 ?? 0;
  if (category === 'budget') return Number(b.cost_usd);
  if (category === 'cache') return b.requests ? (b.cache_hits ?? 0) / b.requests : 0;
  if (category === 'errors' || category === 'availability') return b.requests ? b.errors / b.requests : 0;
  return b.requests;
}

/** The request log's filters for an instance's scope, and the window around it. */
export function reviewParams(
  alert: Alert,
  signal: UsageBucket[] | null,
  now: number,
): Record<string, string> {
  const params: Record<string, string> = {};
  for (const key of ['provider', 'model', 'team', 'project', 'key'] as const) {
    const v = alert.scope?.[key];
    if (v) params[key] = v;
  }
  // A budget's scope is its owner: `team/search/monthly` reviews team search's requests.
  const [level, owner] = (alert.scope?.budget ?? '').split('/');
  if (owner && (level === 'team' || level === 'project' || level === 'key')) params[level] ??= owner;
  const fired = new Date(alert.fired_at).getTime();
  const first = signal?.[0] ? new Date(signal[0].start).getTime() : fired - 30 * 60_000;
  const from = Math.min(first, fired - 15 * 60_000);
  const to = alert.resolved_at ? new Date(alert.resolved_at).getTime() + 5 * 60_000 : now;
  params.range = `${Math.floor(from / 60_000) * 60_000}-${Math.ceil(to / 60_000) * 60_000}`;
  return params;
}

/** The scope in words; internal ids (an anomaly's) are left out, a budget reads by its parts. */
export function scopeText(scope: Record<string, string>): string {
  const parts = Object.entries(scope).flatMap(([k, v]) => {
    if (k === 'anomaly') return [];
    if (k === 'budget') {
      const [level, owner, name] = v.split('/');
      return [
        level === 'gateway' ? `gateway budget ${name ?? ''}` : `${level} ${owner} · budget ${name ?? ''}`,
      ];
    }
    return [`${k} ${v}`];
  });
  return parts.length ? parts.join(' · ') : 'Gateway';
}

function SignalItem({ alert, onNotify }: { alert: Alert; onNotify: ((alert: Alert) => void) | null }) {
  const signal = useAlerts((s) => s.signals[alert.id]);
  const loadSignal = useAlerts((s) => s.loadSignal);
  const now = useNow(60_000);
  useEffect(() => {
    void loadSignal(alert.id);
  }, [alert.id, loadSignal]);
  const tone = severityTone(alert.severity);
  const points = signal?.data ?? null;
  const alertHref = useScopedHref('/alerts/', { alert: alert.id });
  const reviewHref = useScopedHref('/request-log/', reviewParams(alert, points, now));
  const routingHref = useScopedHref('/routing/');
  const budgetsHref = useScopedHref('/budgets/', alert.scope?.budget ? { budget: alert.scope.budget } : {});
  const cacheHref = useScopedHref('/cache/');
  const action = buttonClass('ghost', 'sm');
  const chartTone: ChartTone = tone === 'error' ? 'error' : tone === 'warn' ? 'warn' : 'info';
  const routed = ['capacity', 'latency', 'anomaly', 'availability', 'errors'].includes(alert.category);
  return (
    <li
      className={`border-l-2 pl-3 ${tone === 'error' ? 'border-error' : tone === 'warn' ? 'border-warn' : 'border-info'}`}
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-caption text-muted">
        <Dot tone={tone} />
        <span className="text-fg">{capitalize(alert.severity)}</span>
        <Badge>{alert.category}</Badge>
        <span className="truncate">{scopeText(alert.scope ?? {})}</span>
        <span>· {alert.status === 'acknowledged' ? 'Acknowledged' : 'Firing'}</span>
        <span>· {formatRelative(alert.fired_at)}</span>
      </div>
      <p className="mt-1 text-ui text-fg">{alert.summary}</p>
      {points && points.length > 1 ? (
        <Sparkline
          className="mt-1.5"
          height={24}
          tone={chartTone}
          values={points.map((b) => metricOf(alert.category, b))}
          label={`The signal behind "${alert.summary}"`}
        />
      ) : null}
      <div className="mt-1.5 flex flex-wrap items-center gap-1">
        <Link href={alertHref} className={action}>
          View alert
        </Link>
        <Link href={reviewHref} className={action}>
          Review requests
        </Link>
        {routed ? (
          <Link href={routingHref} className={action}>
            Adjust routing
          </Link>
        ) : null}
        {alert.category === 'budget' ? (
          <Link href={budgetsHref} className={action}>
            Raise budget or approve overage
          </Link>
        ) : null}
        {alert.category === 'cache' ? (
          <Link href={cacheHref} className={action}>
            Review the cache
          </Link>
        ) : null}
        {onNotify && (alert.owner_team ?? alert.scope?.team) ? (
          <Button size="sm" variant="ghost" icon="mail" onClick={() => onNotify(alert)}>
            Notify owner
          </Button>
        ) : null}
      </div>
    </li>
  );
}

/**
 * Signals: open alert instances (anomaly, capacity, budget, cache, latency and others) with
 * severity, scope, time and a mini-chart, filtered by category, each with the actions that fix it.
 */
export function SignalsPanel() {
  const alerts = useAlerts((s) => s.alerts);
  const [category, setCategory] = useState<string | null>(null);
  const [notifying, setNotifying] = useState<Alert | null>(null);
  const [message, setMessage] = useState('');
  const canNotify = usePermission('notifications:send');
  const alertsHref = useScopedHref('/alerts/');
  const open = (alerts.data ?? []).filter((a) => a.status !== 'resolved');
  const present = [
    ...new Set([
      ...CATEGORIES.filter((c) => open.some((a) => a.category === c)),
      ...open.map((a) => a.category),
    ]),
  ];
  const shown = category ? open.filter((a) => a.category === category) : open;
  const owner = notifying ? (notifying.owner_team ?? notifying.scope?.team ?? '') : '';
  return (
    <Panel
      title="Signals"
      subtitle={alerts.data && open.length > 0 ? `${open.length} open` : undefined}
      actions={
        <Link href={alertsHref} className="text-caption text-muted hover:text-fg">
          View all
        </Link>
      }
    >
      {alerts.error && !alerts.data ? (
        isUnavailable(alerts.error) ? (
          <UnavailableState subject="Alerting" size="sm" />
        ) : (
          <ErrorState error={alerts.error} subject="Alerts" size="sm" />
        )
      ) : null}
      {alerts.data && open.length === 0 ? (
        <EmptyState
          size="sm"
          title="No open signals"
          description="Anomalies, capacity, budget and latency alerts show here while open."
        />
      ) : null}
      {present.length > 1 ? (
        <div role="group" aria-label="Signal category" className="mb-3 flex flex-wrap gap-1.5">
          <ChipToggle pressed={category === null} onChange={() => setCategory(null)} count={open.length}>
            All
          </ChipToggle>
          {present.map((c) => (
            <ChipToggle
              key={c}
              pressed={category === c}
              onChange={(on) => setCategory(on ? c : null)}
              count={open.filter((a) => a.category === c).length}
            >
              {capitalize(c)}
            </ChipToggle>
          ))}
        </div>
      ) : null}
      <ul className="flex max-h-[340px] flex-col gap-4 overflow-auto">
        {shown.slice(0, 4).map((a) => (
          <SignalItem
            key={a.id}
            alert={a}
            onNotify={
              canNotify
                ? (al) => {
                    setMessage(
                      `${al.summary}\n\nScope: ${scopeText(al.scope ?? {})}. Fired ${new Date(al.fired_at).toUTCString()}.`,
                    );
                    setNotifying(al);
                  }
                : null
            }
          />
        ))}
      </ul>
      {shown.length > 4 ? (
        <p className="mt-3 text-caption text-muted">
          {plural(shown.length - 4, 'more signal')} in{' '}
          <Link href={alertsHref} className="text-accent hover:underline">
            Alerts
          </Link>
        </p>
      ) : null}
      <ConfirmDialog
        open={notifying !== null}
        onClose={() => setNotifying(null)}
        title={`Notify team ${owner}?`}
        effect={`Every member of team ${owner} gets this by email, with a link to the alert.`}
        confirmLabel="Send notification"
        tone="primary"
        onConfirm={async () => {
          if (!notifying) return;
          const link = `${window.location.origin}${window.location.pathname.replace(/analytics\/?$/, '')}alerts/?alert=${encodeURIComponent(notifying.id)}`;
          const sent = await analyticsExtrasApi.notify(owner, notifying.summary, message, link);
          notify('success', `Team ${owner} notified`, plural(sent.recipients, 'recipient'));
          setNotifying(null);
        }}
      >
        <Field label="Message">
          <Textarea rows={5} value={message} onChange={(e) => setMessage(e.target.value)} />
        </Field>
      </ConfirmDialog>
    </Panel>
  );
}
