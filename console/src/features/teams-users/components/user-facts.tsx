'use client';

import Link from 'next/link';
import { type ReactNode, useEffect, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { TimeChart } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Gate, useCan } from '@/components/ui/gate';
import { DefinitionGrid } from '@/components/ui/layout';
import { DetailSection } from '@/components/ui/list-detail';
import { ColorChip } from '@/components/ui/misc';
import { Banner, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import type { Anomaly } from '@/lib/api/types';
import { formatCount, formatMoney, formatPercent, plural } from '@/lib/format';
import { formatDate, formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import {
  actionVerb,
  describeAgent,
  effectivePermissions,
  INACTIVE_DAYS,
  isSso,
  mfaLabel,
  sessionMethod,
  signInLabel,
} from '../facts';
import { useDirectory, userActions, useTeams, viewerFor } from '../store';
import { roleLabel, type TeamView, type UserView } from '../types';
import { describeError } from '@/lib/api/client';

/** Sign-in method and two-step badges for the header (people only). */
export function SignInBadges({ view }: { view: UserView }) {
  if (view.kind !== 'human') return null;
  return (
    <>
      <Badge tone={isSso(view.signIn) ? 'success' : 'neutral'} title="How they last signed in">
        {signInLabel(view.signIn)}
      </Badge>
      <Badge tone={view.mfa.length > 0 ? 'success' : 'warn'} dot title="Two-step verification">
        {view.mfa.length > 0 ? `Two-step: ${mfaLabel(view.mfa).toLowerCase()}` : 'No two-step verification'}
      </Badge>
    </>
  );
}

const KIND_TEXT: Record<Anomaly['kind'], string> = {
  network: 'used from a network it had not used before',
  user_agent: 'used by an app it had not seen before',
  country: 'used from a new country',
  spike: 'used far more than usual',
  exposed: 'found exposed',
};

function flagSubject(a: Anomaly): string | null {
  const d = a.detail ?? {};
  const value = d.network ?? d.user_agent_family ?? d.country ?? null;
  return typeof value === 'string' ? value : null;
}

/** The open flags of the keys a user owns, each with its requests, the key and a dismiss. */
export function FlagBanner({ view }: { view: UserView }) {
  const slot = useDirectory((s) => s.anomalies[view.name]);
  const load = useDirectory((s) => s.loadAnomalies);
  const dismiss = useDirectory((s) => s.dismissAnomaly);
  const canRead = useCan('listAnomalies');
  const canDismiss = useCan('dismissAnomaly');
  const [busy, setBusy] = useState<string | null>(null);
  useEffect(() => {
    if (view.flagged && canRead) void load(view.name);
  }, [view.flagged, view.name, canRead, load]);
  if (!view.flagged) return null;
  const flags = slot?.data ?? [];
  return (
    <Banner tone="warn" title="Flagged: unusual activity on a key they own">
      {flags.length === 0 ? (
        canRead ? (
          'Loading the flags…'
        ) : (
          "Your role can't see key flags."
        )
      ) : (
        <ul className="flex flex-col gap-2">
          {flags.map((f) => (
            <li key={f.id} className="flex flex-col gap-1">
              <span>
                <FlagText flag={f} />
              </span>
              <FlagActions flag={f} busy={busy} onDismiss={onDismiss} />
            </li>
          ))}
        </ul>
      )}
    </Banner>
  );

  async function onDismiss(flag: Anomaly) {
    if (!canDismiss) return;
    setBusy(flag.id);
    try {
      await dismiss(view.name, flag.id);
      notify('success', 'Flag dismissed', flag.key);
    } catch (e) {
      notify('error', "Couldn't dismiss the flag", describeError(e));
    } finally {
      setBusy(null);
    }
  }
}

function FlagText({ flag }: { flag: Anomaly }) {
  const subject = flagSubject(flag);
  return (
    <>
      Key <span className="mono text-fg">{flag.key}</span> was {KIND_TEXT[flag.kind]}
      {subject ? (
        <>
          {' '}
          (<span className="mono">{subject}</span>)
        </>
      ) : null}
      , {plural(flag.count, 'time')} since {formatRelative(flag.first_seen)}.
    </>
  );
}

function FlagActions({
  flag,
  busy,
  onDismiss,
}: {
  flag: Anomaly;
  busy: string | null;
  onDismiss: (flag: Anomaly) => void;
}) {
  return (
    <span className="flex flex-wrap items-center gap-3">
      <Link
        href={`/request-log/?key=${encodeURIComponent(flag.key)}`}
        className="text-ui font-medium text-accent hover:underline"
      >
        Review requests
      </Link>
      <Link
        href={`/keys/?key=${encodeURIComponent(flag.key)}`}
        className="text-ui font-medium text-accent hover:underline"
      >
        Open key
      </Link>
      <Gate operation="dismissAnomaly">
        <Button size="sm" variant="secondary" busy={busy === flag.id} onClick={() => onDismiss(flag)}>
          Dismiss
        </Button>
      </Gate>
    </span>
  );
}

/** No activity for 60 days or more: suggest disabling them or keeping them as a viewer. */
export function InactiveBanner({ view, onDisable }: { view: UserView; onDisable: () => void }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const canCreate = useCan('createRoleBinding');
  const canDelete = useCan('deleteRoleBinding');
  const canBind = canCreate && canDelete;
  const canPatch = useCan('patchUser');
  const [open, setOpen] = useState(false);
  const changes = view.bindings.filter((b) => b.role !== viewerFor(b.scope));
  return (
    <>
      <Banner
        tone="warn"
        title={`No activity for more than ${INACTIVE_DAYS} days`}
        actions={
          <>
            {canBind && changes.length > 0 ? (
              <Button size="sm" variant="secondary" onClick={() => setOpen(true)}>
                Downgrade to viewer
              </Button>
            ) : null}
            {canPatch ? (
              <Button size="sm" variant="secondary" onClick={onDisable}>
                Disable
              </Button>
            ) : null}
          </>
        }
      >
        {view.lastActive
          ? `Last active ${formatDate(view.lastActive, timeZone)}.`
          : `Never active since they joined on ${formatDate(view.user?.metadata.created_at, timeZone)}.`}{' '}
        Review whether they still need access.
      </Banner>
      <ConfirmDialog
        open={open}
        onClose={() => setOpen(false)}
        title={`Downgrade ${view.name} to viewer?`}
        tone="primary"
        confirmLabel="Downgrade to viewer"
        effect="They keep seeing what they see now, and can no longer change anything."
        onConfirm={async (reason) => {
          await userActions.downgradeToViewer(view, reason);
          notify('success', 'Downgraded to viewer', view.name);
        }}
      >
        <ul className="flex flex-col gap-1 text-ui text-muted">
          {changes.map((b) => (
            <li key={`${b.role}@${b.scope}`}>
              {roleLabel(b.role)} → {roleLabel(viewerFor(b.scope))}{' '}
              <span className="text-dim">({b.scope === 'gateway' ? 'whole gateway' : b.scope})</span>
            </li>
          ))}
        </ul>
      </ConfirmDialog>
    </>
  );
}

function dayLabel(start: string, timeZone: string): string {
  return new Intl.DateTimeFormat('en-US', { timeZone, month: 'short', day: 'numeric' }).format(
    new Date(start),
  );
}

/** Requests over 24 hours and 30 days, spend over 30 days with the share of the team, per day. */
export function UsageSection({ view }: { view: UserView }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const slot = useDirectory((s) => s.usage[view.name]);
  const load = useDirectory((s) => s.loadUsage);
  const canRead = useCan('getUsageAnalytics');
  const team = view.primaryTeam;
  useEffect(() => {
    if (canRead) void load(view.name, team);
  }, [canRead, view.name, team, load]);
  if (!canRead) return null;
  const usage = slot?.data;
  return (
    <DetailSection title="Usage">
      {slot?.error ? <ErrorState error={slot.error} subject="Their usage" size="sm" /> : null}
      {!usage && !slot?.error ? <LoadingRows rows={3} /> : null}
      {usage ? (
        <div className="flex flex-col gap-3">
          <DefinitionGrid
            columns={usage.teamCost30d !== null ? 4 : 3}
            items={[
              { label: 'Requests, last 24 h', value: formatCount(usage.requests24h) },
              { label: 'Requests, last 30 days', value: formatCount(usage.requests30d) },
              { label: 'Spend, last 30 days', value: formatMoney(usage.cost30d) },
              ...(usage.teamCost30d !== null && team
                ? [
                    {
                      label: `Share of ${team}'s spend`,
                      value: (
                        <Tooltip
                          content={`${formatMoney(usage.cost30d)} of ${formatMoney(usage.teamCost30d)} spent by ${team} in 30 days`}
                        >
                          <span>
                            {usage.teamCost30d > 0
                              ? formatPercent(usage.cost30d / usage.teamCost30d, 1)
                              : '—'}
                          </span>
                        </Tooltip>
                      ),
                    },
                  ]
                : []),
            ]}
          />
          <TimeChart
            kind="bars"
            title="Requests per day, last 30 days"
            labels={usage.days.map((d) => dayLabel(d.start, timeZone))}
            series={[{ name: 'Requests', values: usage.days.map((d) => d.requests), tone: 'accent' }]}
            overlay={{
              name: 'Spend',
              values: usage.days.map((d) => d.cost),
              tone: 'warn',
              format: (v) => formatMoney(v),
            }}
            height={120}
            formatValue={(v) => formatCount(v)}
            emptyLabel="No requests with their keys in the last 30 days"
          />
        </div>
      ) : null}
    </DetailSection>
  );
}

/** The teams a user belongs to: the primary team, teams of their roles, each with its lead and spend. */
export function TeamsSection({ view, teams }: { view: UserView; teams: TeamView[] }) {
  const spend = useDirectory((s) => s.teamSpend.data);
  const load = useDirectory((s) => s.loadTeamSpend);
  const allTeams = useTeams((s) => s.items);
  useEffect(() => {
    void load();
  }, [load]);
  const names = new Set<string>();
  if (view.primaryTeam) names.add(view.primaryTeam);
  for (const b of view.bindings) if (b.scope.startsWith('team/')) names.add(b.scope.slice(5));
  for (const t of teams) if (t.lead === view.name) names.add(t.name);
  if (names.size === 0)
    return (
      <DetailSection title="Teams">
        <p className="text-ui text-muted">Not in a team. Set their team with Edit.</p>
      </DetailSection>
    );
  return (
    <DetailSection title="Teams">
      <ul className="flex flex-col gap-1.5">
        {[...names].map((name) => {
          const t = teams.find((x) => x.name === name);
          const counter = (spend ?? []).find((c) => c.owner === name && c.unit === 'usd');
          const exists = allTeams.some((x) => x.name === name);
          return (
            <li key={name} className="flex flex-wrap items-center gap-2 text-ui">
              <ColorChip color={t?.color} className="h-2.5 w-2.5" />
              <span className="font-semibold text-fg">{t?.displayName ?? name}</span>
              {view.primaryTeam === name ? <Badge tone="accent">Primary</Badge> : null}
              {t?.lead === view.name ? <Badge tone="info">Lead</Badge> : null}
              {!exists ? <Badge tone="warn">Team not found</Badge> : null}
              <span className="text-muted">
                {t ? plural(t.members.length, 'member') : null}
                {t?.lead && t.lead !== view.name ? ` · lead ${t.lead}` : ''}
              </span>
              <span className="mono ml-auto text-caption text-muted">
                {counter
                  ? `${formatMoney(counter.spent, { compact: true })} of ${formatMoney(counter.amount, { compact: true })} this month`
                  : null}
              </span>
            </li>
          );
        })}
      </ul>
    </DetailSection>
  );
}

/** A scope with more areas than LONG shows SHORT of them until expanded. */
const LONG = 8;
const SHORT = 6;

/** What a user may do, per scope, in words; the permission ids are in the tooltips. */
export function PermissionsSection({ view }: { view: UserView }) {
  const roles = useDirectory((s) => s.roles.data);
  const [expanded, setExpanded] = useState<string[]>([]);
  if (view.bindings.length === 0) return null;
  if (!roles)
    return (
      <DetailSection title="What they can do">
        <LoadingRows rows={2} />
      </DetailSection>
    );
  const scopes = effectivePermissions(view.bindings, roles);
  return (
    <DetailSection title="What they can do">
      <div className="flex flex-col gap-4">
        {scopes.map((s) => (
          <div key={s.scope}>
            <div className="mb-1.5 flex flex-wrap items-baseline gap-2 text-ui">
              <span className="font-semibold text-fg">{s.label}</span>
              <span className="text-caption text-muted">
                as {s.roles.map((r) => roleLabel(r)).join(' and ')}
              </span>
            </div>
            {s.unknownRoles.length > 0 ? (
              <p className="text-caption text-warn">
                {s.unknownRoles.map((r) => roleLabel(r)).join(', ')}: this role no longer exists, so it grants
                nothing.
              </p>
            ) : null}
            {s.groups.length === 0 && s.unknownRoles.length === 0 ? (
              <p className="text-caption text-muted">Nothing beyond what the whole gateway already allows.</p>
            ) : null}
            <dl className="grid grid-cols-[minmax(120px,max-content)_1fr] gap-x-4 gap-y-1 text-ui max-sm:grid-cols-1">
              {(s.groups.length > LONG && !expanded.includes(s.scope)
                ? s.groups.slice(0, SHORT)
                : s.groups
              ).map((g) => (
                <div key={g.resource} className="contents">
                  <dt className="text-muted">{g.label}</dt>
                  <dd className="text-fg">
                    {g.permissions.map((p, i) => (
                      <span key={p}>
                        {i > 0 ? ', ' : ''}
                        <Tooltip content={p}>
                          <span>{actionVerb(p)}</span>
                        </Tooltip>
                      </span>
                    ))}
                  </dd>
                </div>
              ))}
            </dl>
            {s.groups.length > LONG ? (
              <Button
                size="sm"
                variant="ghost"
                className="mt-1 -ml-2"
                onClick={() =>
                  setExpanded((e) => (e.includes(s.scope) ? e.filter((x) => x !== s.scope) : [...e, s.scope]))
                }
              >
                {expanded.includes(s.scope) ? 'Show less' : `Show all ${s.groups.length} areas`}
              </Button>
            ) : null}
          </div>
        ))}
      </div>
    </DetailSection>
  );
}

/** A user's console sessions, with "Sign out everywhere". */
export function SessionsSection({ view, onSignOut }: { view: UserView; onSignOut: () => void }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const slot = useDirectory((s) => s.sessions[view.name]);
  const load = useDirectory((s) => s.loadSessions);
  const canRead = useCan('listUserSessions');
  // Someone invited has not signed in yet: the gateway keeps no sessions for them.
  const signedUp = view.kind === 'human' && view.state !== 'invited';
  useEffect(() => {
    if (canRead && signedUp) void load(view.name);
  }, [canRead, signedUp, view.name, load]);
  if (!canRead || !signedUp) return null;
  const sessions = slot?.data ?? null;
  return (
    <DetailSection
      title="Sessions"
      aside={
        sessions && sessions.length > 0 ? (
          <Button size="sm" icon="logout" onClick={onSignOut}>
            Sign out everywhere
          </Button>
        ) : null
      }
    >
      {slot?.error ? <ErrorState error={slot.error} subject="Their sessions" size="sm" /> : null}
      {!sessions && !slot?.error ? <LoadingRows rows={2} /> : null}
      {sessions?.length === 0 ? <p className="text-ui text-muted">Not signed in anywhere.</p> : null}
      {sessions && sessions.length > 0 ? (
        <Table label={`Sessions of ${view.name}`}>
          <thead>
            <tr>
              <Th>Browser</Th>
              <Th>Signed in with</Th>
              <Th>Started</Th>
              <Th>Last seen</Th>
              <Th>Ends</Th>
            </tr>
          </thead>
          <tbody>
            {sessions.map((s) => (
              <Tr key={s.id}>
                <Td title={s.user_agent}>
                  <span className="text-fg">{describeAgent(s.user_agent)}</span>
                  {s.client_ip ? (
                    <span className="mono ml-2 text-caption text-muted">{s.client_ip}</span>
                  ) : null}
                </Td>
                <Td muted>
                  <span className="flex flex-wrap items-center gap-1.5">
                    {sessionMethod(s)}
                    {s.mfa ? <Badge tone="success">Two-step</Badge> : null}
                    {s.trusted ? <Badge tone="info">Trusted browser</Badge> : null}
                  </span>
                </Td>
                <Td muted title={formatDateTime(s.created_at, timeZone)}>
                  {formatRelative(s.created_at)}
                </Td>
                <Td muted title={formatDateTime(s.last_seen_at, timeZone)}>
                  {formatRelative(s.last_seen_at)}
                </Td>
                <Td muted title={formatDateTime(s.expires_at, timeZone)}>
                  {formatRelative(s.expires_at)}
                </Td>
              </Tr>
            ))}
          </tbody>
        </Table>
      ) : null}
    </DetailSection>
  );
}

/** "Sign-in" facts for the details grid: the method, two-step and the last sign-in and activity. */
export function signInItems(view: UserView, timeZone: string): { label: string; value: ReactNode }[] {
  const last = view.user?.metadata.last_login_at ?? null;
  return [
    {
      label: 'Last signed in',
      value: last ? (
        <Tooltip content={formatDateTime(last, timeZone)}>
          <span>
            {formatRelative(last)} · {signInLabel(view.signIn)}
          </span>
        </Tooltip>
      ) : (
        'Never'
      ),
    },
    {
      label: 'Two-step verification',
      value: (
        <span className="flex items-center gap-1.5">
          <Dot tone={view.mfa.length > 0 ? 'success' : 'warn'} />
          {mfaLabel(view.mfa)}
        </span>
      ),
    },
    {
      label: 'Last active',
      value: view.lastActive ? (
        <Tooltip content={formatDateTime(view.lastActive, timeZone)}>
          <span>{formatRelative(view.lastActive)}</span>
        </Tooltip>
      ) : (
        'Never'
      ),
    },
  ];
}
