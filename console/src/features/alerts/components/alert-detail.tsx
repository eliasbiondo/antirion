'use client';

import { useEffect, useState, type ReactNode } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { TimeChart } from '@/components/ui/charts';
import { Field, Textarea } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { TileGrid } from '@/components/ui/layout';
import { MenuButton } from '@/components/ui/menu';
import { CopyLinkButton } from '@/components/ui/misc';
import { Dialog } from '@/components/ui/overlay';
import { EmptyState, ErrorState, isUnavailable, LoadingRows } from '@/components/ui/states';
import type { OperationId } from '@/lib/api/operations';
import type { Alert } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount, formatLatency, formatPercent, plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { formatClock, formatDateTime } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import {
  activityWords,
  agoWords,
  CHANNEL_TYPES,
  dayMonth,
  METRICS,
  severityWord,
  spanWords,
  STATUS_WORD,
} from '../describe';
import { useAlertEditors } from '../editors';
import { resolveSeconds, ruleStats } from '../stats';
import { severityTone, useAlerts } from '../store';

export function durationOf(a: Alert, now: number): number {
  const end = a.resolved_at ? new Date(a.resolved_at).getTime() : now;
  return (end - new Date(a.fired_at).getTime()) / 1000;
}

/** Whether an instance is snoozed now. */
export function snoozed(a: Alert, now: number): boolean {
  return !!a.snoozed_until && new Date(a.snoozed_until).getTime() > now && a.status !== 'resolved';
}

/** Why a workflow action is disabled for this principal, said in words. */
function useDenied(operation: OperationId, verb: string): string | undefined {
  const can = useCan(operation);
  return can ? undefined : `You don't have permission to ${verb} alerts.`;
}

function NoteDialog({ id, onClose }: { id: string; onClose: () => void }) {
  const act = useAlerts((s) => s.act);
  const [text, setText] = useState('');
  const [error, setError] = useState<string | undefined>();
  return (
    <Dialog
      open
      onClose={onClose}
      title="Add a note"
      size="sm"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={() => {
              if (!text.trim()) {
                setError('Write the note first');
                return;
              }
              void act(id, { note: text.trim() }).then(onClose);
            }}
          >
            Add note
          </Button>
        </>
      }
    >
      <Field label="Note" hint="Everyone who opens this alert sees it in its activity." error={error}>
        <Textarea
          rows={4}
          maxLength={2000}
          value={text}
          onChange={(e) => setText(e.target.value)}
          data-autofocus
        />
      </Field>
    </Dialog>
  );
}

function Facts({ items }: { items: { label: string; value: ReactNode; mono?: boolean }[] }) {
  return (
    <TileGrid as="dl" variant="flat" cols={{ base: 1, sm: 2, md: 3 }}>
      {items.map((item) => (
        <div key={item.label} className="min-w-0 bg-raised px-3.5 py-2.5">
          <dt className="text-caption text-muted">{item.label}</dt>
          <dd className={cn('mt-1 text-body break-words text-fg', item.mono && 'mono')}>{item.value}</dd>
        </div>
      ))}
    </TileGrid>
  );
}

function Section({ title, aside, children }: { title: string; aside?: ReactNode; children: ReactNode }) {
  return (
    <section className="border-b border-line px-7 py-5 max-sm:px-gutter-sm">
      <div className="mb-3 flex items-baseline justify-between gap-3">
        <h3 className="text-ui font-semibold text-fg">{title}</h3>
        {aside ? <span className="text-caption text-muted">{aside}</span> : null}
      </div>
      {children}
    </section>
  );
}

const SNOOZE = [
  { value: '15m', label: '15 minutes' },
  { value: '1h', label: '1 hour' },
  { value: '4h', label: '4 hours' },
  { value: '24h', label: '1 day' },
];

/** Past instances with the same grouping key over 90 days, newest first, this one left out. */
function SimilarFires({ alert, onOpen }: { alert: Alert; onOpen: (id: string) => void }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const slot = useAlerts((s) => s.similar[alert.id]);
  const loadSimilar = useAlerts((s) => s.loadSimilar);
  const known = useAlerts((s) => s.history.data);
  const [all, setAll] = useState(false);
  const now = useNow(60_000);
  useEffect(() => {
    void loadSimilar(alert.id);
  }, [alert.id, loadSimilar]);
  const past = (slot?.data ?? []).filter((a) => a.id !== alert.id);
  const shown = all ? past : past.slice(0, 4);
  const quick = past.filter((a) => {
    const s = resolveSeconds(a);
    return s !== null && s <= 60;
  }).length;
  return (
    <Section
      title="Similar fires, last 90 days"
      aside={slot?.data ? (past.length ? plural(past.length, 'fire') : undefined) : undefined}
    >
      {slot?.error ? (
        isUnavailable(slot.error) ? (
          <p className="text-ui text-muted">Past fires aren&apos;t available on this gateway yet.</p>
        ) : (
          <ErrorState error={slot.error} subject="Past fires" size="sm" />
        )
      ) : !slot?.data ? (
        <LoadingRows rows={2} label="Loading past fires" />
      ) : past.length === 0 ? (
        <p className="text-ui text-muted">This is the first time this grouping key has fired in 90 days.</p>
      ) : (
        <>
          {past.length > 1 ? (
            <p className="mb-2 text-caption text-muted">
              {quick ? `${plural(quick, 'fire')} cleared within a minute. ` : ''}
              Last one {agoWords(past[0]?.fired_at ?? alert.fired_at, now)}.
            </p>
          ) : null}
          <ul className="flex flex-col divide-y divide-line rounded-control border border-line">
            {shown.map((a) => {
              const note = (a.timeline ?? []).find((t) => t.kind === 'note');
              const span = resolveSeconds(a);
              const openable = known?.some((k) => k.id === a.id);
              const body = (
                <>
                  <span className="mono w-28 shrink-0 text-caption text-fg">
                    {dayMonth(a.fired_at, timeZone, true)}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block text-ui text-fg">
                      {span !== null ? `Lasted ${spanWords(span)}` : STATUS_WORD[a.status]}
                      {a.resolved_by ? (
                        <span className="text-muted"> · resolved by {a.resolved_by}</span>
                      ) : null}
                    </span>
                    {note?.detail && 'text' in note.detail ? (
                      <span className="block truncate text-caption text-muted">
                        {String(note.detail.text)}
                      </span>
                    ) : null}
                  </span>
                  <Dot tone={severityTone(a.severity)} className="mt-1.5 shrink-0" />
                </>
              );
              return (
                <li key={a.id}>
                  {openable ? (
                    <button
                      type="button"
                      onClick={() => onOpen(a.id)}
                      className="flex w-full items-start gap-3 px-3 py-2 text-left hover:bg-hover"
                    >
                      {body}
                    </button>
                  ) : (
                    <div className="flex items-start gap-3 px-3 py-2">{body}</div>
                  )}
                </li>
              );
            })}
          </ul>
          {past.length > 4 ? (
            <Button size="sm" variant="ghost" className="mt-2" onClick={() => setAll(!all)}>
              {all ? 'Show fewer' : `Show all ${formatCount(past.length)}`}
            </Button>
          ) : null}
        </>
      )}
    </Section>
  );
}

/** The rule behind the instance over 30 days: fires, notifications and time to resolve, with its runbook. */
function RuleFacts({ alert, now }: { alert: Alert; now: number }) {
  const history = useAlerts((s) => s.history.data);
  const rules = useAlerts((s) => s.rules.data);
  const show = useAlertEditors((s) => s.show);
  const rule = rules?.find((r) => r.name === alert.rule);
  const stats = history ? ruleStats(history, alert.rule, now) : null;
  return (
    <Section
      title="Rule, last 30 days"
      aside={
        rule ? (
          <button
            type="button"
            className="text-caption text-accent hover:underline"
            onClick={() => show({ kind: 'rule', name: rule.name })}
          >
            Edit rule
          </button>
        ) : rules ? (
          'Built into the gateway'
        ) : undefined
      }
    >
      <dl className="grid grid-cols-3 gap-3 max-sm:grid-cols-1">
        <div>
          <dt className="text-caption text-muted">Fires</dt>
          <dd className="mono mt-0.5 text-body text-fg tabular-nums">
            {stats ? formatCount(stats.fires) : '—'}
          </dd>
        </div>
        <div>
          <dt className="text-caption text-muted">Notifications</dt>
          <dd className="mono mt-0.5 text-body text-fg tabular-nums">
            {stats ? formatCount(stats.notifications) : '—'}
          </dd>
        </div>
        <div>
          <dt className="text-caption text-muted">Time to resolve</dt>
          <dd className="mono mt-0.5 text-body text-fg tabular-nums">
            {stats?.mttr != null ? spanWords(stats.mttr) : '—'}
          </dd>
        </div>
      </dl>
      {alert.runbook_url ? (
        <a
          href={alert.runbook_url}
          rel="noreferrer"
          target="_blank"
          className="mt-3 flex items-center gap-2 rounded-control border border-line bg-sunken px-3 py-2 text-ui text-fg hover:border-line-strong"
        >
          <Icon name="book" size={14} className="text-muted" />
          Open runbook
          <Icon name="external" size={14} className="ml-auto text-muted" />
        </a>
      ) : null}
    </Section>
  );
}

/** One alert instance: what fired, its metric, its activity, who it notified, past fires and its rule. */
export function AlertDetail({ alert, onOpen }: { alert: Alert; onOpen: (id: string) => void }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const signal = useAlerts((s) => s.signals[alert.id]);
  const loadSignal = useAlerts((s) => s.loadSignal);
  const channels = useAlerts((s) => s.channels.data);
  const rules = useAlerts((s) => s.rules.data);
  const act = useAlerts((s) => s.act);
  const now = useNow(10_000);
  const [note, setNote] = useState(false);
  const ackDenied = useDenied('acknowledgeAlert', 'acknowledge');
  const resolveDenied = useDenied('resolveAlert', 'resolve');
  const snoozeDenied = useDenied('snoozeAlert', 'snooze');
  const unsnoozeDenied = useDenied('unsnoozeAlert', 'end the snooze of');
  const escalateDenied = useDenied('escalateAlert', 'escalate');
  const noteDenied = useDenied('noteAlert', 'add notes to');
  useEffect(() => {
    void loadSignal(alert.id);
  }, [alert.id, loadSignal]);
  const run = (action: Parameters<typeof act>[1], message: string) =>
    void act(alert.id, action).then(() => notify('success', message));
  const points = signal?.data ?? [];
  // A minute without traffic has no latency or error rate: a gap, never a zero.
  const metricOf = (b: (typeof points)[number]): number | null =>
    alert.category === 'latency'
      ? (b.latency_ms?.p95 ?? null)
      : alert.category === 'errors'
        ? b.requests
          ? b.errors / b.requests
          : null
        : b.requests;
  const traffic = points.some((b) => b.requests > 0);
  const metricName =
    alert.category === 'latency' ? 'p95 latency' : alert.category === 'errors' ? 'Error rate' : 'Requests';
  const formatMetric = (v: number) =>
    alert.category === 'latency'
      ? formatLatency(v)
      : alert.category === 'errors'
        ? formatPercent(v, 1)
        : formatCount(v, { compact: true });
  const rule = rules?.find((r) => r.name === alert.rule);
  const routed = rule?.spec.channels ?? [];
  const isSnoozed = snoozed(alert, now);
  const impact =
    alert.impact && !alert.impact.requests && !alert.impact.errors && !alert.impact.throttled
      ? 'None counted for this alert'
      : alert.impact
        ? [
            plural(alert.impact.requests ?? 0, 'request'),
            plural(alert.impact.errors ?? 0, 'error'),
            alert.impact.throttled ? `${formatCount(alert.impact.throttled)} throttled` : null,
          ]
            .filter(Boolean)
            .join(', ')
        : '—';
  const snoozeItems = [
    ...(isSnoozed
      ? [
          {
            label: 'End snooze now',
            icon: 'bell' as const,
            disabled: unsnoozeDenied !== undefined,
            disabledReason: unsnoozeDenied,
            onSelect: () => run('unsnooze', 'Snooze ended'),
          },
        ]
      : []),
    ...SNOOZE.map((d) => ({
      label: isSnoozed ? `Snooze ${d.label} from now` : d.label,
      disabled: snoozeDenied !== undefined || alert.status === 'resolved',
      disabledReason: snoozeDenied,
      onSelect: () => run({ snooze: d.value }, `Snoozed for ${d.label}`),
    })),
  ];
  return (
    <div>
      <div className="sticky top-0 z-[5] border-b border-line bg-raised px-7 pt-6 pb-4 max-sm:px-gutter-sm">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-caption text-muted">
          <span className="inline-flex items-center gap-1.5">
            <Dot tone={severityTone(alert.severity)} />
            <span className="font-medium text-fg">{severityWord(alert.severity)}</span>
          </span>
          <span aria-hidden="true">·</span>
          <span>
            {alert.status === 'acknowledged' && alert.acknowledged_by
              ? `Acknowledged by ${alert.acknowledged_by}`
              : alert.status === 'resolved' && alert.resolved_by
                ? `Resolved by ${alert.resolved_by}`
                : STATUS_WORD[alert.status]}
          </span>
          {isSnoozed && alert.snoozed_until ? (
            <>
              <span aria-hidden="true">·</span>
              <span className="inline-flex items-center gap-1.5">
                <Icon name="clock" size={12} />
                Snoozed until {formatClock(alert.snoozed_until, timeZone).slice(0, 5)}
              </span>
            </>
          ) : null}
        </div>
        <h2 className="mt-2 text-[22px] leading-7 font-semibold tracking-tight text-fg">{alert.summary}</h2>
        {rule?.spec.description ? <p className="mt-1.5 text-ui text-muted">{rule.spec.description}</p> : null}
        {alert.scope && Object.keys(alert.scope).length > 0 ? (
          <div className="mt-3 flex flex-wrap gap-1.5">
            {Object.entries(alert.scope)
              .filter(([k]) => k !== 'anomaly')
              .map(([k, v]) => (
                <Badge key={k} mono>
                  {k}: {v}
                </Badge>
              ))}
          </div>
        ) : null}
        <div className="mt-4 flex flex-wrap items-center gap-2">
          <Button
            variant="primary"
            disabled={alert.status !== 'firing' || ackDenied !== undefined}
            disabledReason={ackDenied}
            onClick={() => run('acknowledge', 'Acknowledged')}
          >
            Acknowledge
          </Button>
          <Button
            disabled={alert.status === 'resolved' || resolveDenied !== undefined}
            disabledReason={resolveDenied}
            onClick={() => run('resolve', 'Resolved')}
          >
            Resolve
          </Button>
          {isSnoozed ? (
            <Button
              icon="bell"
              disabled={unsnoozeDenied !== undefined}
              disabledReason={unsnoozeDenied}
              onClick={() => run('unsnooze', 'Snooze ended')}
            >
              Unsnooze
            </Button>
          ) : null}
          <MenuButton
            label={isSnoozed ? 'Snooze again' : 'Snooze'}
            icon="clock"
            align="left"
            items={snoozeItems}
          />
          <MenuButton
            label="More actions"
            iconOnly
            items={[
              {
                label: 'Add a note',
                icon: 'edit',
                disabled: noteDenied !== undefined,
                disabledReason: noteDenied,
                onSelect: () => setNote(true),
              },
              {
                label: 'Escalate',
                icon: 'arrowUp',
                disabled: escalateDenied !== undefined || alert.status === 'resolved',
                disabledReason: escalateDenied,
                onSelect: () => run('escalate', 'Escalated'),
              },
            ]}
          />
          <span className="ml-auto">
            <CopyLinkButton />
          </span>
        </div>
      </div>
      <Section title="Details">
        <Facts
          items={[
            {
              label: 'Fired',
              value: (
                <>
                  <span className="mono">{formatDateTime(alert.fired_at, timeZone)}</span>
                  <span className="block text-caption text-muted">{agoWords(alert.fired_at, now)}</span>
                </>
              ),
            },
            {
              label: alert.status === 'resolved' ? 'Lasted' : 'Lasting',
              value: spanWords(durationOf(alert, now)),
            },
            { label: 'Rule', value: alert.rule, mono: true },
            { label: 'Owner', value: alert.owner_team ?? '—' },
            { label: 'Impact', value: impact },
          ]}
        />
      </Section>
      <Section
        title="Metric, last 40 min"
        aside={
          alert.threshold != null
            ? `Threshold: ${
                rule?.spec.metric && rule.spec.threshold === alert.threshold
                  ? METRICS[rule.spec.metric].value(alert.threshold)
                  : alert.category === 'latency'
                    ? `${formatCount(alert.threshold)} ms`
                    : formatCount(alert.threshold)
              }`
            : undefined
        }
      >
        {signal?.error ? (
          isUnavailable(signal.error) ? (
            <p className="text-ui text-muted">
              The metric behind this alert isn&apos;t available on this gateway yet.
            </p>
          ) : (
            <ErrorState error={signal.error} subject="The metric" size="sm" />
          )
        ) : null}
        {!signal || (signal.status === 'loading' && !signal.data) ? (
          <LoadingRows rows={2} label="Loading the metric" />
        ) : null}
        {points.length > 0 && traffic ? (
          <TimeChart
            kind="area"
            title={`${metricName} around ${alert.summary}`}
            labels={points.map((b) => formatClock(b.start, timeZone).slice(0, 5))}
            series={[{ name: metricName, values: points.map(metricOf), tone: severityTone(alert.severity) }]}
            formatValue={formatMetric}
            reference={
              alert.threshold != null &&
              (rule?.spec.metric
                ? (alert.category === 'latency' && rule.spec.metric === 'p95_latency_ms') ||
                  (alert.category === 'errors' && rule.spec.metric === 'error_rate')
                : alert.category === 'latency')
                ? { value: alert.threshold, label: 'Threshold' }
                : undefined
            }
            gaps
            height={170}
          />
        ) : signal?.data ? (
          <EmptyState
            size="sm"
            title={
              points.length > 0
                ? 'No traffic recorded for this scope in these 40 minutes'
                : 'No data points in this window'
            }
          />
        ) : null}
      </Section>
      <div className="grid grid-cols-1 xl:grid-cols-[minmax(0,1.3fr)_minmax(0,1fr)]">
        <Section title="Activity" aside={plural((alert.timeline ?? []).length, 'event')}>
          <ol className="flex flex-col">
            {(alert.timeline ?? []).map((t, i, all) => (
              <li key={`${t.time}-${i}`} className="relative flex gap-3 pb-3 last:pb-0">
                {i < all.length - 1 ? (
                  <span aria-hidden="true" className="absolute top-3 bottom-0 left-[3px] w-px bg-line" />
                ) : null}
                <Dot
                  tone={
                    t.kind === 'fired' || t.kind === 'refired' || t.kind === 'notification_failed'
                      ? 'error'
                      : t.kind === 'resolved'
                        ? 'success'
                        : t.kind === 'acknowledged'
                          ? 'warn'
                          : 'info'
                  }
                  className="relative mt-1.5"
                />
                <div className="min-w-0">
                  <p className="text-ui text-fg">
                    {activityWords(t.kind, t.detail, t.time)}
                    {t.actor ? <span className="text-muted"> · {t.actor}</span> : null}
                  </p>
                  {t.detail && 'text' in t.detail ? (
                    <p className="text-ui text-muted">{String(t.detail.text)}</p>
                  ) : null}
                  <p className="mono text-caption text-dim">{formatClock(t.time, timeZone)}</p>
                </div>
              </li>
            ))}
          </ol>
        </Section>
        <div className="xl:border-l xl:border-line">
          <Section title="Notifies">
            {routed.length === 0 ? (
              <p className="text-ui text-muted">This rule sends no notifications.</p>
            ) : (
              <ul className="flex flex-col gap-1.5">
                {routed.map((name) => {
                  const ch = channels?.find((c) => c.name === name);
                  return (
                    <li
                      key={name}
                      className="flex items-center gap-2 rounded-control border border-line bg-sunken px-3 py-2 text-ui"
                    >
                      <Icon name="bell" size={14} className="text-muted" />
                      <span className="mono truncate text-fg">{name}</span>
                      {ch ? (
                        <span className="ml-auto text-caption text-muted">{CHANNEL_TYPES[ch.spec.type]}</span>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            )}
          </Section>
          <RuleFacts alert={alert} now={now} />
        </div>
      </div>
      <SimilarFires alert={alert} onOpen={onOpen} />
      {note ? <NoteDialog id={alert.id} onClose={() => setNote(false)} /> : null}
    </div>
  );
}
