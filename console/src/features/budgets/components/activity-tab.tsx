'use client';

import { useState } from 'react';

import { Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Gate } from '@/components/ui/gate';
import { Panel } from '@/components/ui/layout';
import { EmptyState, ErrorState } from '@/components/ui/states';
import { Tooltip } from '@/components/ui/tooltip';
import type { BudgetCounter } from '@/lib/api/types';

type BudgetGrant = NonNullable<BudgetCounter['grants']>[number];
import { formatMoney } from '@/lib/format';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { useBudgets } from '../store';
import { EVENT_LABEL, parsePartition, partitionLabel } from '../types';

interface Waiver {
  counter: BudgetCounter;
  grant: BudgetGrant;
  label: string;
}

function budgetLabel(c: BudgetCounter): string {
  const p = parsePartition(c.partition);
  return `${c.owner} · ${c.name}${p ? ` · ${partitionLabel(p)}` : ''}`;
}

/** Running grants of every budget, soonest to expire first. */
export function useWaivers(): Waiver[] {
  const counters = useBudgets((s) => s.counters.data) ?? [];
  return counters
    .flatMap((c) => (c.grants ?? []).map((grant) => ({ counter: c, grant, label: budgetLabel(c) })))
    .sort((a, b) => a.grant.expires_at.localeCompare(b.grant.expires_at));
}

function Waivers() {
  const timeZone = usePreferences((s) => s.timeZone);
  const waivers = useWaivers();
  const revoke = useBudgets((s) => s.revokeGrant);
  const [revoking, setRevoking] = useState<Waiver | null>(null);
  return (
    <Panel title="Approved overages" subtitle="Running until they expire or the extra amount is spent">
      {waivers.length === 0 ? (
        <EmptyState size="sm" title="None running" description="Approve an overage from a blocked budget." />
      ) : null}
      <ul className="flex flex-col">
        {waivers.map((w) => (
          <li
            key={w.grant.id}
            className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-line py-2 text-ui last:border-0"
          >
            <Dot tone="info" />
            <span className="font-semibold text-fg">{w.label}</span>
            <span className="mono text-fg">+{formatMoney(w.grant.amount)}</span>
            <Tooltip content={formatDateTime(w.grant.expires_at, timeZone)}>
              <span className="text-caption text-muted">expires {formatRelative(w.grant.expires_at)}</span>
            </Tooltip>
            <span className="basis-full pl-5 text-caption text-muted">
              {w.grant.reason} · by {w.grant.granted_by}
            </span>
            <Gate operation="revokeBudgetGrant" mode="hide">
              <Button size="sm" variant="ghost" className="ml-auto" onClick={() => setRevoking(w)}>
                Revoke
              </Button>
            </Gate>
          </li>
        ))}
      </ul>
      {revoking ? (
        <ConfirmDialog
          open
          onClose={() => setRevoking(null)}
          title="Revoke this overage?"
          confirmLabel="Revoke overage"
          objectName={revoking.label}
          effect="The budget goes back to its own amount at once; if it's spent, new requests are refused again."
          onConfirm={async (reason) => {
            await revoke(revoking.counter, revoking.grant.id, reason);
            notify('success', 'Overage revoked', revoking.label);
          }}
        />
      ) : null}
    </Panel>
  );
}

/** Requests to go over, approved overages and the budget events feed. */
export function ActivityTab() {
  const events = useBudgets((s) => s.events);
  const overage = useBudgets((s) => s.overage);
  const decideOverage = useBudgets((s) => s.decideOverage);
  return (
    <div className="grid grid-cols-1 gap-px border-b border-line bg-line xl:grid-cols-2">
      <div className="flex flex-col gap-px">
        <Panel title="Requests to go over a budget">
          {overage.error ? (
            <ErrorState error={overage.error} subject="Requests to go over a budget" size="sm" />
          ) : null}
          {overage.data?.length === 0 ? <EmptyState size="sm" title="None waiting" /> : null}
          <ul className="flex flex-col gap-2">
            {(overage.data ?? []).map((o) => (
              <li key={o.id} className="flex flex-wrap items-center gap-3 text-ui">
                <Dot tone="warn" />
                <span className="text-fg">
                  <span className="font-semibold">{o.owner}</span>{' '}
                  <span className="mono text-muted">{o.name}</span>
                </span>
                <span className="text-caption text-muted">
                  {o.amount ? `${formatMoney(o.amount)} more · ` : ''}
                  {formatRelative(o.opened_at)}
                </span>
                <span className="ml-auto flex gap-1">
                  <Gate operation="denyOverageRequest" mode="hide">
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() =>
                        void decideOverage(o.id, false).then(() =>
                          notify('success', 'Request denied', o.owner),
                        )
                      }
                    >
                      Deny
                    </Button>
                  </Gate>
                  <Gate operation="approveOverageRequest" mode="hide">
                    <Button
                      size="sm"
                      onClick={() =>
                        void decideOverage(o.id, true).then(() =>
                          notify('success', 'Going over approved', o.owner),
                        )
                      }
                    >
                      Approve
                    </Button>
                  </Gate>
                </span>
              </li>
            ))}
          </ul>
        </Panel>
        <Waivers />
      </div>
      <Panel title="Activity" subtitle="Newest first">
        {events.error ? <ErrorState error={events.error} subject="Budget activity" size="sm" /> : null}
        {events.data?.length === 0 ? <EmptyState size="sm" title="Nothing happened this period" /> : null}
        <ol className="flex flex-col">
          {(events.data ?? []).slice(0, 30).map((e, i) => (
            <li
              key={`${e.time}-${i}`}
              className="flex flex-wrap items-center gap-x-3 gap-y-0.5 border-b border-line py-2 text-ui last:border-0"
            >
              <Dot
                tone={
                  e.kind === 'exhausted'
                    ? 'error'
                    : e.kind === 'threshold_crossed' || e.kind === 'forecast_exceeded'
                      ? 'warn'
                      : 'info'
                }
              />
              <span className="text-fg">{EVENT_LABEL[e.kind] ?? e.kind.replace(/_/g, ' ')}</span>
              <span className="text-muted">
                {e.owner} <span className="mono">{e.name}</span>
                {e.actor ? ` · by ${e.actor}` : ''}
              </span>
              <span className="ml-auto text-caption text-dim">{formatRelative(e.time)}</span>
            </li>
          ))}
        </ol>
      </Panel>
    </div>
  );
}
