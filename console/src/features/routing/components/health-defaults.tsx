'use client';

import { type ReactNode, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Meter } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, NumberInput } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { Panel } from '@/components/ui/layout';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import type { DeploymentHealth } from '@/lib/api/types';
import { formatCount, formatPercent, plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';

import { healthName } from '@/features/models/types';

import type { RouteStateEntry } from '../api';
import {
  type HedgingDraft,
  hedgingFrom,
  hedgingTo,
  type PredicateDraft,
  predicatesFrom,
  predicatesTo,
  predicateText,
  validateExtras,
} from '../resilience';
import { useRouting } from '../store';
import { isRecord, type Json } from '../types';
import { HedgingFields, ResponseChecks } from './resilience-fields';
import { useEdited } from '@/lib/hooks';

const STATE_LABEL: Record<DeploymentHealth['state'], string> = {
  closed: 'Healthy',
  open: 'Paused',
  half_open: 'Testing',
  disabled: 'Turned off',
};

function cooldownLeft(h: DeploymentHealth, now: Date): string {
  const until = h.open_until ?? h.cooldown_until ?? h.disabled_until;
  if (!until) return h.state === 'half_open' ? 'testing with a trial request' : '—';
  const left = new Date(until).getTime() - now.getTime();
  if (left <= 0) return 'about to be tested again';
  return `retried ${formatRelative(until, now)}`;
}

/** Open circuits with what tripped them, in-flight requests against the cap, and route sessions. */
export function HealthPanel({
  route,
  state,
  onReset,
}: {
  route: string | null;
  state: RouteStateEntry[] | null;
  onReset: () => void;
}) {
  const health = useRouting((s) => s.health);
  const gateway = useRouting((s) => s.gateway);
  const canReset = useCan('deleteRouteAffinity');
  const now = new Date();
  const list = health.data ?? [];
  const open = list.filter((h) => h.state !== 'closed');
  const admission = gateway.data?.admission;
  // Every entry reports the route's bindings on this replica; older gateways report none.
  const bindings = state?.[0]?.affinity_bindings ?? null;
  return (
    <Panel title="Health" subtitle="Deployments paused after errors, and load">
      {health.error ? <ErrorState error={health.error} subject="Deployment health" size="sm" /> : null}
      {!health.data && !health.error ? <LoadingRows rows={2} /> : null}
      {health.data ? (
        list.length === 0 ? (
          <p className="text-ui text-muted">No deployment has served a request yet.</p>
        ) : open.length === 0 ? (
          <p className="flex items-center gap-2 text-ui text-fg">
            <Icon name="checkCircle" size={14} className="text-success" />
            {list.length === 1
              ? 'The one deployment is taking traffic: none is paused after errors.'
              : `All ${plural(list.length, 'deployment')} are taking traffic: none is paused after errors.`}
          </p>
        ) : (
          <ul aria-label="Paused deployments" className="flex flex-col">
            {open.map((h) => (
              <li
                key={healthName(h)}
                className="flex flex-col gap-0.5 border-b border-line py-2 text-ui last:border-0"
              >
                <div className="flex items-center gap-2">
                  <span className="mono font-semibold text-fg">{healthName(h)}</span>
                  <Badge tone={h.state === 'half_open' ? 'warn' : 'error'}>{STATE_LABEL[h.state]}</Badge>
                </div>
                <span className="text-caption text-muted">
                  {h.trip_error_rate != null
                    ? `Tripped at ${formatPercent(h.trip_error_rate, 1)} errors`
                    : 'Tripped'}
                  {` · ${plural(h.window.failures, 'failure')} in ${plural(h.window.requests, 'request')}`}
                  {` · ${cooldownLeft(h, now)}`}
                  {h.trips > 1 ? ` · paused ${plural(h.trips, 'time')}` : ''}
                </span>
              </li>
            ))}
          </ul>
        )
      ) : null}
      <div className="mt-4 flex flex-col gap-1.5">
        <div className="flex items-baseline justify-between text-ui">
          <span className="text-muted">Requests in flight on this node</span>
          <span className="mono text-fg tabular-nums">
            {admission
              ? `${formatCount(admission.in_flight)} / ${formatCount(admission.max_in_flight)}`
              : gateway.error
                ? 'Not available'
                : '—'}
          </span>
        </div>
        {admission ? (
          <>
            <Meter
              value={admission.in_flight}
              max={Math.max(1, admission.max_in_flight)}
              tone={admission.in_flight / Math.max(1, admission.max_in_flight) > 0.9 ? 'warn' : 'accent'}
              label="Requests in flight against the admission cap"
            />
            <span className="text-caption text-muted">
              {formatCount(admission.queued)} waiting of {formatCount(admission.max_queue)} allowed. The cap
              is per node.
            </span>
          </>
        ) : null}
      </div>
      {route ? (
        <div className="mt-4 flex flex-wrap items-center justify-between gap-2 border-t border-line pt-3 text-ui">
          <span className="text-muted">
            Sessions kept on a deployment by route {route}:{' '}
            <span className="mono text-fg tabular-nums">
              {bindings === null ? '—' : formatCount(bindings)}
            </span>
          </span>
          {canReset ? (
            <Button size="sm" icon="refresh" onClick={onReset}>
              Reset sessions
            </Button>
          ) : null}
        </div>
      ) : null}
    </Panel>
  );
}

function text(value: Json): string {
  return typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean'
    ? String(value)
    : JSON.stringify(value);
}

function rec(value: Json): Record<string, Json> {
  return isRecord(value) ? value : {};
}

/** The gateway-wide defaults as read: configured values, or the built-in default marked so. */
function useDefaults() {
  const policies = useRouting((s) => s.policies);
  const resilience = useRouting((s) => s.resilience);
  const server = useRouting((s) => s.server);
  const res = rec(rec(policies.data?.spec).resilience);
  const timeouts = rec(res.timeouts);
  const retry = rec(res.retry);
  const backoff = rec(retry.backoff);
  const breaker = rec(rec(rec(resilience.data?.spec).health).breaker);
  const admission = rec(rec(server.data?.spec).admission);
  return { policies, resilience, server, res, timeouts, retry, backoff, breaker, admission };
}

interface Item {
  group: 'timeouts' | 'retry' | 'backoff' | 'breaker' | 'admission';
  key: string;
  label: string;
  fallback: string;
  kind: 'duration' | 'int' | 'number' | 'ratio';
  hint?: string;
}

const ITEMS: Item[] = [
  { group: 'timeouts', key: 'connect', label: 'Connect', fallback: '10s', kind: 'duration' },
  { group: 'timeouts', key: 'ttft', label: 'First token', fallback: '300s', kind: 'duration' },
  { group: 'timeouts', key: 'stream_idle', label: 'Idle stream', fallback: '120s', kind: 'duration' },
  { group: 'timeouts', key: 'attempt', label: 'Each attempt', fallback: '600s', kind: 'duration' },
  { group: 'timeouts', key: 'deadline', label: 'Whole request', fallback: '900s', kind: 'duration' },
  {
    group: 'retry',
    key: 'max_attempts',
    label: 'Attempts at most',
    fallback: '5',
    kind: 'int',
    hint: '1 to 10',
  },
  {
    group: 'retry',
    key: 'tier_retries',
    label: 'Retries per step',
    fallback: '2',
    kind: 'int',
    hint: '0 to 9',
  },
  { group: 'backoff', key: 'initial', label: 'First wait', fallback: '500ms', kind: 'duration' },
  { group: 'backoff', key: 'max', label: 'Longest wait', fallback: '8s', kind: 'duration' },
  { group: 'backoff', key: 'multiplier', label: 'Growth', fallback: '2', kind: 'number', hint: '1 to 10' },
  { group: 'backoff', key: 'jitter', label: 'Jitter', fallback: '50', kind: 'ratio', hint: 'Percent' },
  {
    group: 'breaker',
    key: 'failure_rate',
    label: 'Pause above',
    fallback: '50',
    kind: 'ratio',
    hint: 'Percent errors',
  },
  {
    group: 'breaker',
    key: 'min_requests',
    label: 'Out of at least',
    fallback: '20',
    kind: 'int',
    hint: 'Requests',
  },
  { group: 'breaker', key: 'window', label: 'Over', fallback: '60s', kind: 'duration' },
  {
    group: 'breaker',
    key: 'consecutive_failures',
    label: 'Or failures in a row',
    fallback: '5',
    kind: 'int',
  },
  { group: 'breaker', key: 'open_duration', label: 'First pause', fallback: '30s', kind: 'duration' },
  { group: 'breaker', key: 'max_open_duration', label: 'Longest pause', fallback: '300s', kind: 'duration' },
  {
    group: 'admission',
    key: 'max_in_flight',
    label: 'In flight at most',
    fallback: '—',
    kind: 'int',
    hint: 'Per node',
  },
  {
    group: 'admission',
    key: 'max_queue',
    label: 'Waiting at most',
    fallback: '—',
    kind: 'int',
    hint: 'Per node',
  },
  { group: 'admission', key: 'queue_timeout', label: 'Longest wait', fallback: '—', kind: 'duration' },
];

const GROUPS: { id: Item['group'][]; title: string; note: string }[] = [
  {
    id: ['timeouts'],
    title: 'Timeouts',
    note: 'How long the gateway waits before it gives up on a deployment.',
  },
  {
    id: ['retry', 'backoff'],
    title: 'Retries and backoff',
    note: 'Attempts include fallbacks to other deployments.',
  },
  {
    id: ['breaker'],
    title: 'Circuit breaker',
    note: 'When a deployment is paused after errors, and for how long.',
  },
  {
    id: ['admission'],
    title: 'Admission',
    note: 'Requests a node takes at once; the rest wait, then get 503.',
  },
];

function shown(item: Item, value: Json): string {
  if (value === undefined || value === null)
    return item.kind === 'ratio' ? `${item.fallback}%` : item.fallback;
  if (item.kind === 'ratio' && typeof value === 'number') return formatPercent(value, 0);
  return text(value);
}

/** The defaults panel (RO-24): timeouts, retries and backoff, breaker and admission, editable. */
export function DefaultsPanel() {
  const d = useDefaults();
  const [editing, setEditing] = useState(false);
  const canPolicies = useCan('patchPoliciesSettings');
  const canResilience = useCan('patchResilienceSettings');
  const canServer = useCan('patchServerSettings');
  const values: Record<Item['group'], Record<string, Json>> = {
    timeouts: d.timeouts,
    retry: d.retry,
    backoff: d.backoff,
    breaker: d.breaker,
    admission: d.admission,
  };
  const hedging = hedgingFrom(d.res.hedging);
  const checks = predicatesFrom(d.retry.retry_on_response);
  const gateway = useRouting((s) => s.gateway.data);
  // Admission has no fixed default: the cap in effect is what the node reports.
  const effective: Record<string, number | undefined> = {
    'admission.max_in_flight': gateway?.admission.max_in_flight,
    'admission.max_queue': gateway?.admission.max_queue,
  };
  const loading = !d.policies.data && !d.policies.error;
  return (
    <Panel
      title="Defaults"
      subtitle="For every route, unless a rule sets its own"
      actions={
        canPolicies || canResilience || canServer ? (
          <Button size="sm" icon="edit" onClick={() => setEditing(true)} disabled={loading}>
            Edit defaults
          </Button>
        ) : null
      }
    >
      {d.policies.error ? <ErrorState error={d.policies.error} subject="Defaults" size="sm" /> : null}
      {loading ? <LoadingRows rows={3} /> : null}
      {!loading ? (
        <div className="grid gap-5 sm:grid-cols-2">
          {GROUPS.map((g) => (
            <section key={g.title} aria-label={g.title}>
              <h3 className="text-ui font-semibold text-fg">{g.title}</h3>
              <p className="mb-2 text-caption text-muted">{g.note}</p>
              {g.id[0] === 'admission' && d.server.error ? (
                <p className="text-ui text-muted">Not available to you.</p>
              ) : (
                <dl className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-1 text-ui">
                  {ITEMS.filter((i) => g.id.includes(i.group)).map((i) => {
                    const v = values[i.group][i.key];
                    const inEffect = v === undefined ? effective[`${i.group}.${i.key}`] : undefined;
                    return (
                      <div key={`${i.group}.${i.key}`} className="contents">
                        <dt className="text-muted">{i.label}</dt>
                        <dd className="mono text-right text-fg tabular-nums">
                          {inEffect !== undefined ? formatCount(inEffect) : shown(i, v)}
                          {v === undefined && (i.fallback !== '—' || inEffect !== undefined) ? (
                            <span className="ml-1 text-caption text-dim">default</span>
                          ) : null}
                        </dd>
                      </div>
                    );
                  })}
                </dl>
              )}
            </section>
          ))}
        </div>
      ) : null}
      {!loading ? (
        <section aria-label="Slow and failed answers" className="mt-5 border-t border-line pt-4">
          <h3 className="text-ui font-semibold text-fg">Slow and failed answers</h3>
          <p className="mb-2 text-caption text-muted">
            Another deployment is asked when an answer is slow, or when it says it failed.
          </p>
          <dl className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-1 text-ui">
            <dt className="text-muted">Hedged requests</dt>
            <dd className="text-right text-fg">
              {hedging.enabled ? (
                <span className="mono tabular-nums">
                  after {hedging.delay}, up to {hedging.maxExtra} more
                </span>
              ) : (
                <>
                  Off<span className="ml-1 text-caption text-dim">default</span>
                </>
              )}
            </dd>
            <dt className="text-muted">Answers that count as failures</dt>
            <dd className="text-right text-fg">
              {checks.length > 0 ? (
                checks.map((c) => predicateText(c)).join('; ')
              ) : (
                <>
                  None<span className="ml-1 text-caption text-dim">default</span>
                </>
              )}
            </dd>
          </dl>
        </section>
      ) : null}
      {editing ? (
        <DefaultsDrawer
          hedging={hedging}
          checks={checks}
          values={values}
          can={{ policies: canPolicies, resilience: canResilience, server: canServer }}
          onClose={() => setEditing(false)}
        />
      ) : null}
    </Panel>
  );
}

const DURATION = /^[0-9]+(\.[0-9]+)?(ms|s|m|h|d)$/;

function parse(item: Item, text: string): { value: Json; error?: string } {
  const t = text.trim();
  if (t === '') return { value: null };
  if (item.kind === 'duration')
    return DURATION.test(t) ? { value: t } : { value: null, error: 'A number and a unit, e.g. 30s' };
  const n = Number(t);
  if (!Number.isFinite(n) || n < 0) return { value: null, error: 'Enter a number' };
  if (item.kind === 'int')
    return Number.isInteger(n) ? { value: n } : { value: null, error: 'A whole number' };
  if (item.kind === 'ratio')
    return n > 0 && n <= 100
      ? { value: Math.round(n * 100) / 10_000 }
      : { value: null, error: 'Percent above 0, at most 100' };
  return { value: n };
}

function initial(item: Item, value: Json): string {
  if (value === undefined || value === null) return '';
  if (item.kind === 'ratio' && typeof value === 'number') return String(Math.round(value * 10_000) / 100);
  return text(value);
}

/** Edits the defaults as merge patches of policies, resilience and server; empty fields return to the default. */
function DefaultsDrawer({
  hedging: hedgingBefore,
  checks: checksBefore,
  values,
  can,
  onClose,
}: {
  hedging: HedgingDraft;
  checks: PredicateDraft[];
  values: Record<Item['group'], Record<string, Json>>;
  can: { policies: boolean; resilience: boolean; server: boolean };
  onClose: () => void;
}) {
  const saveDefaults = useRouting((s) => s.saveDefaults);
  const [form, setForm] = useState<Record<string, string>>(() =>
    Object.fromEntries(ITEMS.map((i) => [`${i.group}.${i.key}`, initial(i, values[i.group][i.key])])),
  );
  const [hedging, setHedging] = useState(hedgingBefore);
  const [checks, setChecks] = useState(checksBefore);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const edited = useEdited({ form, hedging, checks });
  const allowed = (g: Item['group']) =>
    g === 'breaker' ? can.resilience : g === 'admission' ? can.server : can.policies;

  async function save() {
    const out: Record<string, string> = {};
    const changed: Record<Item['group'], Record<string, Json>> = {
      timeouts: {},
      retry: {},
      backoff: {},
      breaker: {},
      admission: {},
    };
    for (const i of ITEMS) {
      const id = `${i.group}.${i.key}`;
      const text = form[id] ?? '';
      if (text === initial(i, values[i.group][i.key])) continue;
      const r = parse(i, text);
      if (r.error) out[id] = r.error;
      else changed[i.group][i.key] = r.value;
    }
    Object.assign(out, validateExtras(hedging, checks, ''));
    setErrors(out);
    if (Object.keys(out).length > 0) return;
    const has = (o: Record<string, Json>) => Object.keys(o).length > 0;
    const same = (a: Json, b: Json) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
    const nextChecks = checks.length > 0 ? predicatesTo(checks) : null;
    const retry: Record<string, Json> = {
      ...changed.retry,
      ...(has(changed.backoff) ? { backoff: changed.backoff } : {}),
      ...(same(nextChecks, checksBefore.length > 0 ? predicatesTo(checksBefore) : null)
        ? {}
        : { retry_on_response: nextChecks }),
    };
    const nextHedging = hedgingTo(hedging) ?? null;
    const hedge = same(nextHedging, hedgingTo(hedgingBefore) ?? null) ? {} : { hedging: nextHedging };
    const patch = {
      ...(has(changed.timeouts) || has(retry) || has(hedge)
        ? {
            policies: {
              resilience: {
                ...(has(changed.timeouts) ? { timeouts: changed.timeouts } : {}),
                ...(has(retry) ? { retry } : {}),
                ...hedge,
              },
            },
          }
        : {}),
      ...(has(changed.breaker) ? { resilience: { health: { breaker: changed.breaker } } } : {}),
      ...(has(changed.admission) ? { server: { admission: changed.admission } } : {}),
    };
    if (Object.keys(patch).length === 0) {
      onClose();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await saveDefaults(patch);
      notify('success', 'Defaults saved', 'They apply to new requests at once.');
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const fields = (group: Item['group']): ReactNode =>
    ITEMS.filter((i) => i.group === group).map((i) => {
      const id = `${i.group}.${i.key}`;
      const disabled = !allowed(i.group);
      return (
        <Field
          key={id}
          label={i.label}
          error={errors[id]}
          hint={
            i.fallback !== '—'
              ? `Default ${i.kind === 'ratio' ? `${i.fallback}%` : i.fallback}${i.hint ? ` · ${i.hint}` : ''}`
              : i.hint
          }
        >
          {i.kind === 'duration' ? (
            <Input
              mono
              disabled={disabled}
              value={form[id] ?? ''}
              placeholder={i.fallback === '—' ? '' : i.fallback}
              onChange={(e) => setForm((f) => ({ ...f, [id]: e.target.value }))}
            />
          ) : (
            <NumberInput
              disabled={disabled}
              min={0}
              step={i.kind === 'int' ? 1 : 0.5}
              value={form[id] ?? ''}
              onChange={(v) => setForm((f) => ({ ...f, [id]: v }))}
            />
          )}
        </Field>
      );
    });

  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      title="Edit routing defaults"
      subtitle="Empty fields use the built-in default. Rules can set their own timeouts and attempts."
      footer={
        <>
          <span className="flex-1" />
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void save()}>
            Save defaults
          </Button>
        </>
      }
    >
      <InlineError error={error} />
      {GROUPS.map((g) => (
        <fieldset key={g.title} className="mb-5">
          <legend className="text-ui font-semibold text-fg">{g.title}</legend>
          <p className="mb-3 text-caption text-muted">
            {g.note}
            {!allowed(g.id[0] ?? 'timeouts') ? ' You can’t change these.' : ''}
          </p>
          <div className="grid gap-3 sm:grid-cols-3">{g.id.map((x) => fields(x))}</div>
        </fieldset>
      ))}
      <fieldset className="mb-5">
        <legend className="text-ui font-semibold text-fg">Slow and failed answers</legend>
        <p className="mb-3 text-caption text-muted">
          When another deployment is asked: an answer is slow, or it says it failed.
          {can.policies ? '' : ' You can’t change these.'}
        </p>
        <div className="flex flex-col gap-5">
          <HedgingFields value={hedging} onChange={setHedging} errors={errors} disabled={!can.policies} />
          <ResponseChecks value={checks} onChange={setChecks} errors={errors} disabled={!can.policies} />
        </div>
      </fieldset>
    </Drawer>
  );
}

/** Confirms clearing a route's session bindings (RO-34). */
export function ResetSessionsDialog({ route, onClose }: { route: string | null; onClose: () => void }) {
  const resetAffinity = useRouting((s) => s.resetAffinity);
  return (
    <ConfirmDialog
      open={route !== null}
      onClose={onClose}
      title={`Reset the sessions of route ${route ?? ''}?`}
      objectName={route ?? undefined}
      confirmLabel="Reset sessions"
      tone="primary"
      effect="Each session’s next request is routed afresh and stays on whichever deployment serves it. Nothing else changes."
      onConfirm={async () => {
        if (!route) return;
        const deleted = await resetAffinity(route);
        notify(
          'success',
          'Sessions reset',
          `${plural(deleted, 'session')} of route ${route} will be routed afresh.`,
        );
      }}
    />
  );
}
