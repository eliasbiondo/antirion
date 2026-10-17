'use client';

import { useEffect, useMemo, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Meter } from '@/components/ui/charts';
import { Field, Input, Select } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { FilterBar, Panel, TileGrid } from '@/components/ui/layout';
import { EmptyState, ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { CountNote, ExportCsvButton, Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { auditEntryLabel, auditActorBy, auditActorLabel, auditToggle } from '@/lib/audit-words';
import { useTokenNames } from '@/stores/actors';
import type { AuditEntry, BudgetCounter, OrganizationResource } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount, formatMoney, formatPercent, plural } from '@/lib/format';
import { formatDate, formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { budgetRatio } from '../hooks';
import { useKeysContext } from '../store';
import type { KeyView } from '../types';

const DAY = 86_400_000;

/* ---------------------------------------------------------------- by team */

/** Live keys grouped by team, each team with its budget's bar and its keys' last 24 hours. */
export function KeysByTeam({
  views,
  teamOf,
  onOpen,
}: {
  views: KeyView[];
  teamOf: (project: string) => string | undefined;
  onOpen: (name: string) => void;
}) {
  const teams = useKeysContext((s) => s.teams.data);
  const budgets = useKeysContext((s) => s.budgets.data);
  const usage = useKeysContext((s) => s.usage.data);
  const groups = useMemo(() => {
    const live = views.filter((v) => v.status !== 'expired' && v.status !== 'disabled');
    const by = new Map<string, KeyView[]>();
    for (const v of live) {
      const team = teamOf(v.project) ?? '';
      by.set(team, [...(by.get(team) ?? []), v]);
    }
    const names = new Map((teams ?? []).map((t) => [t.name, t.spec.display_name ?? t.name]));
    return [...by.entries()]
      .map(([team, keys]) => {
        const teamBudgets = (budgets ?? []).filter(
          (c) => c.level === 'team' && c.owner === team && c.partition === null,
        );
        const budget = teamBudgets.reduce<BudgetCounter | null>(
          (best, c) => (!best || budgetRatio(c) > budgetRatio(best) ? c : best),
          null,
        );
        const requests = keys.reduce((a, k) => a + (usage?.get(k.name)?.requests ?? 0), 0);
        const cost = keys.reduce((a, k) => a + (usage?.get(k.name)?.cost ?? 0), 0);
        return { team, name: team ? (names.get(team) ?? team) : 'No team', keys, budget, requests, cost };
      })
      .sort((a, b) => b.cost - a.cost || b.keys.length - a.keys.length);
  }, [views, teamOf, teams, budgets, usage]);
  if (groups.length === 0) return <EmptyState title="No live keys" />;
  return (
    <TileGrid as="ul" label="Keys by team" cols={{ base: 1, lg: 2 }}>
      {groups.map((g) => {
        const ratio = g.budget ? budgetRatio(g.budget) : 0;
        return (
          <li
            key={g.team || '-'}
            className="flex flex-col gap-3 bg-raised px-gutter py-4 max-sm:px-gutter-sm"
          >
            <div className="flex flex-wrap items-baseline gap-2">
              <span className="text-body font-semibold text-fg">{g.name}</span>
              <span className="text-caption text-muted">
                {plural(g.keys.length, 'live key')} · {formatCount(g.requests, { compact: true })} requests
                and {formatMoney(g.cost, { compact: true })} in 24 h
              </span>
            </div>
            {g.budget ? (
              <div className="flex flex-col gap-1">
                <Meter
                  value={Number(g.budget.spent)}
                  max={Number(g.budget.amount)}
                  tone={ratio >= 1 ? 'error' : ratio >= 0.8 ? 'warn' : 'accent'}
                  label={`${g.name} budget ${g.budget.name}: ${formatPercent(ratio, 0)}`}
                />
                <span className="text-caption text-muted">
                  Team budget <span className="mono">{g.budget.name}</span>: {formatMoney(g.budget.spent)} of{' '}
                  {formatMoney(g.budget.amount)} ({formatPercent(ratio, 0)})
                </span>
              </div>
            ) : (
              <span className="text-caption text-dim">
                {g.team ? 'No team budget' : 'Keys whose project has no team'}
              </span>
            )}
            <div className="flex flex-wrap gap-1.5">
              {g.keys.map((k) => (
                <button
                  key={k.name}
                  type="button"
                  onClick={() => onOpen(k.name)}
                  className="mono rounded-control border border-line bg-sunken px-2 py-0.5 text-caption text-fg hover:border-accent"
                >
                  {k.name}
                </button>
              ))}
            </div>
          </li>
        );
      })}
    </TileGrid>
  );
}

/* ---------------------------------------------------------------- rotation */

/** Durations as the policy writes them: `90d`, `24h`, `30m`. */
const DURATION = /^\d+(s|m|h|d)$/;

function days(duration: string | undefined): number | null {
  const m = /^(\d+)(s|m|h|d)$/.exec(duration ?? '');
  if (!m) return null;
  const n = Number(m[1]);
  return m[2] === 'd' ? n : m[2] === 'h' ? n / 24 : m[2] === 'm' ? n / 1440 : n / 86400;
}

interface Due {
  view: KeyView;
  kind: 'rotation' | 'expiry';
  at: string;
  /** Days from now; negative when past. */
  inDays: number;
  /** The reminder days already passed for this date, and the next one. */
  sent: number[];
  next: number | null;
}

/** Keys whose rotation or expiry is past or within 14 days, with the reminders for that date. */
export function dueKeys(views: KeyView[], reminderDays: number[], now: number): Due[] {
  const out: Due[] = [];
  const reminders = [...reminderDays].sort((a, b) => b - a);
  for (const v of views) {
    if (v.status === 'expired' || v.status === 'disabled') continue;
    const dates: [Due['kind'], string | null | undefined][] = [
      ['rotation', v.key.metadata.rotation_due_at],
      ['expiry', v.key.spec.expires_at],
    ];
    for (const [kind, at] of dates) {
      if (!at) continue;
      const inDays = (new Date(at).getTime() - now) / DAY;
      if (inDays > 14) continue;
      out.push({
        view: v,
        kind,
        at,
        inDays,
        sent: reminders.filter((d) => inDays <= d),
        next: reminders.find((d) => inDays > d) ?? null,
      });
    }
  }
  return out.sort((a, b) => a.inDays - b.inDays);
}

function PolicyPanel({ organizations }: { organizations: OrganizationResource[] }) {
  const [org, setOrg] = useState(organizations[0]?.name ?? '');
  const current = organizations.find((o) => o.name === org);
  return (
    <Panel
      title="Rotation policy"
      subtitle="For keys in the organization; a team or project can set its own"
      actions={
        organizations.length > 1 ? (
          <div className="w-48">
            <Select
              aria-label="Organization"
              value={org}
              onChange={setOrg}
              options={organizations.map((o) => ({ value: o.name, label: o.spec.display_name ?? o.name }))}
            />
          </div>
        ) : null
      }
    >
      {current ? (
        <PolicyForm key={`${current.name}/${current.metadata.etag}`} organization={current} />
      ) : null}
    </Panel>
  );
}

function PolicyForm({ organization }: { organization: OrganizationResource }) {
  const save = useKeysContext((s) => s.saveKeyPolicy);
  const canWrite = useCan('patchOrganization');
  const org = organization.name;
  const policy = organization.spec.key_policy ?? {};
  const [values, setValues] = useState({
    lifetime: policy.default_lifetime ?? '',
    maxAge: policy.rotation_max_age ?? '',
    reminders: (policy.reminder_days ?? []).join(', '),
    grace: policy.default_grace_period ?? '',
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [fieldError, setFieldError] = useState<string | null>(null);

  async function submit() {
    for (const [label, v] of [
      ['Default lifetime', values.lifetime],
      ['Rotate after', values.maxAge],
      ['Default grace', values.grace],
    ] as const)
      if (v.trim() && !DURATION.test(v.trim())) {
        setFieldError(`${label}: a length such as 90d, 24h or 30m.`);
        return;
      }
    const reminders = values.reminders
      .split(',')
      .map((d) => d.trim())
      .filter(Boolean);
    if (reminders.some((d) => !/^\d{1,3}$/.test(d))) {
      setFieldError('Reminders: whole days before the date, separated by commas, such as 14, 7, 1.');
      return;
    }
    setFieldError(null);
    setBusy(true);
    setError(null);
    try {
      await save(org, {
        default_lifetime: values.lifetime.trim() || null,
        rotation_max_age: values.maxAge.trim() || null,
        reminder_days: reminders.length > 0 ? reminders.map(Number) : null,
        default_grace_period: values.grace.trim() || null,
      });
      notify('success', 'Key policy saved', org);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const maxLifetime = organization.spec.max_key_lifetime;
  return (
    <form
      noValidate
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <InlineError error={error} />
      {fieldError ? <p className="text-ui text-error">{fieldError}</p> : null}
      <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
        <Field label="Default lifetime" optional hint="A new key without an expiry gets this.">
          <Input
            mono
            value={values.lifetime}
            onChange={(e) => setValues({ ...values, lifetime: e.target.value })}
            placeholder="e.g. 90d"
          />
        </Field>
        <Field label="Rotate after" optional hint="A key older than this needs rotation.">
          <Input
            mono
            value={values.maxAge}
            onChange={(e) => setValues({ ...values, maxAge: e.target.value })}
            placeholder="e.g. 30d"
          />
        </Field>
        <Field
          label="Reminders, days before"
          optional
          hint="Owners are reminded before a key expires or is due."
        >
          <Input
            mono
            value={values.reminders}
            onChange={(e) => setValues({ ...values, reminders: e.target.value })}
            placeholder="e.g. 14, 7, 1"
          />
        </Field>
        <Field
          label="Default grace on rotation"
          optional
          hint="How long the old secret keeps working; 7d at most."
        >
          <Input
            mono
            value={values.grace}
            onChange={(e) => setValues({ ...values, grace: e.target.value })}
            placeholder="e.g. 24h"
          />
        </Field>
      </div>
      <p className="text-caption text-muted">
        Longest a key may live: <span className="mono text-fg">{maxLifetime ?? 'no limit'}</span>
        {maxLifetime ? ' (set on the organization in Settings)' : ''}.
      </p>
      <Gate operation="patchOrganization">
        <Button type="submit" variant="primary" busy={busy} disabled={!canWrite || !org} className="w-fit">
          Save policy
        </Button>
      </Gate>
    </form>
  );
}

/** The rotation policy of an organization, and the keys due or upcoming within 14 days. */
export function RotationTab({
  views,
  onOpen,
  now,
}: {
  views: KeyView[];
  onOpen: (name: string) => void;
  now: number;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const organizations = useKeysContext((s) => s.organizations);
  const orgs = organizations.data ?? [];
  const policy = orgs[0]?.spec.key_policy;
  const due = dueKeys(views, policy?.reminder_days ?? [14, 7, 1], now);
  const maxAge = days(policy?.rotation_max_age);
  return (
    <div className="grid grid-cols-1 gap-px border-b border-line bg-line xl:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]">
      {organizations.error ? (
        <Panel title="Rotation policy">
          <ErrorState error={organizations.error} subject="The key policy" size="sm" />
        </Panel>
      ) : orgs.length > 0 ? (
        <PolicyPanel key={orgs.map((o) => o.name).join()} organizations={orgs} />
      ) : (
        <Panel title="Rotation policy">
          <LoadingRows rows={3} />
        </Panel>
      )}
      <Panel
        title="Due within 14 days"
        subtitle={
          maxAge !== null
            ? `Keys are rotated every ${plural(Math.round(maxAge), 'day')}`
            : 'Rotations and expiries coming up'
        }
      >
        {due.length === 0 ? (
          <EmptyState size="sm" title="No key needs rotation or expires in the next 14 days" />
        ) : (
          <Table label="Keys due for rotation or expiring">
            <thead>
              <tr>
                <Th>Key</Th>
                <Th>Due</Th>
                <Th>When</Th>
                <Th>Reminders</Th>
              </tr>
            </thead>
            <tbody>
              {due.map((d) => (
                <Tr key={`${d.view.name}/${d.kind}`}>
                  <Td>
                    <button
                      type="button"
                      onClick={() => onOpen(d.view.name)}
                      className="text-left font-semibold text-fg hover:underline"
                    >
                      {d.view.name}
                    </button>
                    <div className="text-caption text-muted">{d.view.owner ?? 'No owner'}</div>
                  </Td>
                  <Td>
                    <Badge tone={d.inDays < 0 ? 'error' : 'warn'} dot>
                      {d.kind === 'rotation'
                        ? d.inDays < 0
                          ? 'Needs rotation'
                          : 'Rotation'
                        : d.inDays < 0
                          ? 'Expired'
                          : 'Expires'}
                    </Badge>
                  </Td>
                  <Td>
                    <Tooltip content={formatDateTime(d.at, timeZone)}>
                      <span className={cn(d.inDays < 0 && 'text-error')}>{formatRelative(d.at)}</span>
                    </Tooltip>
                  </Td>
                  <Td muted>
                    {d.sent.length > 0
                      ? `Sent ${d.sent.map((n) => `${n} d`).join(', ')} before`
                      : 'None sent yet'}
                    {d.next !== null
                      ? ` · next ${formatDate(new Date(new Date(d.at).getTime() - d.next * DAY), timeZone)}`
                      : ''}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        )}
      </Panel>
    </div>
  );
}

/* ---------------------------------------------------------------- activity */

const ACTION_LABEL: Record<string, string> = {
  'keys.create': 'Created',
  'keys.replace': 'Edited',
  'keys.update': 'Edited',
  'keys.patch': 'Edited',
  'keys.delete': 'Revoked',
  'keys.rotate': 'Rotated',
  'keys.secret_revealed': 'Secret shown',
  'keys.expiring': 'Expiry reminder sent',
  'keys.rotation_due': 'Rotation reminder sent',
  'keys.anomaly_flagged': 'Unusual activity flagged',
  'keys.anomaly_dismissed': 'Unusual activity dismissed',
  'playground.mint': 'Made in the playground',
};

function actionLabel(e: AuditEntry): string {
  const toggled = /\.(update|patch|replace)$/.test(e.action) ? auditToggle(e.changes) : null;
  if (toggled !== null) return toggled ? 'Enabled' : 'Disabled';
  return ACTION_LABEL[e.action] ?? auditEntryLabel(e);
}

function keyOf(e: AuditEntry): string {
  return e.resource?.replace(/^keys\//, '') ?? '';
}

/** Key events across the scope, newest first, with a filter and export. */
export function KeyActivity({ onOpen }: { onOpen: (name: string) => void }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const activity = useKeysContext((s) => s.activity);
  const load = useKeysContext((s) => s.loadActivity);
  const [query, setQuery] = useState('');
  const tokens = useTokenNames();
  useEffect(() => {
    void load();
  }, [load]);
  const q = query.trim().toLowerCase();
  const rows = (activity.data ?? []).filter(
    (e) =>
      !q || `${keyOf(e)} ${actionLabel(e)} ${auditActorLabel(e.actor, tokens)}`.toLowerCase().includes(q),
  );
  if (activity.error)
    return <ErrorState error={activity.error} subject="Key activity" onRetry={() => void load()} />;
  if (!activity.data) return <LoadingRows />;
  return (
    <>
      <FilterBar>
        <div className="w-64 max-sm:w-full">
          <Input
            aria-label="Search activity"
            icon="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search activity"
          />
        </div>
        <span className="ml-auto flex items-center gap-3">
          <CountNote shown={rows.length} total={activity.data.length} noun="events" />
          <ExportCsvButton
            filename="key-activity.csv"
            rows={rows}
            columns={[
              { header: 'time', value: (e) => e.time },
              { header: 'key', value: (e) => keyOf(e) },
              { header: 'action', value: (e) => e.action },
              { header: 'actor', value: (e) => auditActorLabel(e.actor, tokens) },
              { header: 'outcome', value: (e) => e.outcome },
              { header: 'reason', value: (e) => e.reason ?? '' },
            ]}
          />
        </span>
      </FilterBar>
      {rows.length === 0 ? (
        <EmptyState
          title="No key activity yet"
          description="Creations, rotations and revocations show here."
        />
      ) : (
        <ol aria-label="Key activity" className="border-b border-line bg-raised">
          {rows.map((e) => (
            <li
              key={e.id}
              className="flex flex-wrap items-center gap-x-3 gap-y-0.5 border-b border-line px-gutter py-2 text-ui last:border-0 max-sm:px-gutter-sm"
            >
              <Dot
                tone={
                  e.outcome !== 'success'
                    ? 'error'
                    : e.action.includes('anomaly') ||
                        e.action.includes('due') ||
                        e.action.includes('expiring')
                      ? 'warn'
                      : e.action === 'keys.delete'
                        ? 'error'
                        : 'info'
                }
              />
              <span className="text-fg">{actionLabel(e)}</span>
              <button type="button" onClick={() => onOpen(keyOf(e))} className="mono text-fg hover:underline">
                {keyOf(e)}
              </button>
              <span className="text-muted">by {auditActorBy(e.actor, tokens)}</span>
              {e.reason ? <span className="text-caption text-muted">“{e.reason}”</span> : null}
              <Tooltip content={formatDateTime(e.time, timeZone)}>
                <span className="ml-auto text-caption text-dim">{formatRelative(e.time)}</span>
              </Tooltip>
            </li>
          ))}
        </ol>
      )}
    </>
  );
}
