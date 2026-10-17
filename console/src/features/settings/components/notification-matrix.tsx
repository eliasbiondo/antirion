'use client';

import { useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Checkbox } from '@/components/ui/choice';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Tooltip } from '@/components/ui/tooltip';
import { alertsApi } from '@/features/alerts/api';
import { CHANNEL_TYPES } from '@/features/alerts/describe';
import { useAlerts } from '@/features/alerts/store';
import { describeError } from '@/lib/api/client';
import { notify } from '@/stores/ui';

import {
  type CellState,
  channelCell,
  channelToggle,
  MATRIX_ROWS,
  type MatrixRow,
  webhookCell,
  webhookToggle,
} from '../matrix';
import { loadAlertConfig, useAlertConfig } from './alert-editors';
import { Card } from './parts';

interface Column {
  id: string;
  name: string;
  kind: string;
  type: 'channel' | 'webhook';
}

function Cell({
  state,
  label,
  busy,
  canWrite,
  onToggle,
}: {
  state: CellState;
  label: string;
  busy: boolean;
  canWrite: boolean;
  onToggle: (on: boolean) => void;
}) {
  if (state.kind === 'blocked')
    return (
      <Tooltip content={state.detail || null}>
        <span aria-label={`${label}: not available`} className="text-dim">
          –
        </span>
      </Tooltip>
    );
  return (
    <Tooltip content={canWrite ? state.detail : `${state.detail} Your role can't change this.`}>
      <span className="inline-flex">
        <Checkbox
          aria-label={label}
          checked={state.kind === 'on' ? true : state.kind === 'mixed' ? 'mixed' : false}
          disabled={!canWrite || busy}
          onChange={(on) => onToggle(on || state.kind === 'mixed')}
        />
      </span>
    </Tooltip>
  );
}

/**
 * Which events and alerts reach which channel and event webhook. A tick edits the alert rules' channels
 * (alerts of a severity, or the event rule of a category, created when there is none) and the
 * webhooks' subscriptions.
 */
export function NotificationsMatrixSection() {
  useAlertConfig();
  const rules = useAlerts((s) => s.rules);
  const channels = useAlerts((s) => s.channels);
  const webhooks = useAlerts((s) => s.webhooks);
  const saveWebhook = useAlerts((s) => s.saveWebhook);
  const canReplace = useCan('replaceAlertRule');
  const canCreate = useCan('createAlertRule');
  const canRules = canReplace && canCreate;
  const canHooks = useCan('patchObservabilitySettings');
  const accountHref = useScopedHref('', { account: 'notifications' });
  const alertsHref = useScopedHref('/alerts/', { view: 'channels' });
  const [busy, setBusy] = useState<string | null>(null);

  const columns: Column[] = [
    ...(channels.data ?? []).map((c) => ({
      id: `c:${c.name}`,
      name: c.name,
      kind: CHANNEL_TYPES[c.spec.type],
      type: 'channel' as const,
    })),
    ...Object.keys(webhooks.data?.items ?? {}).map((n) => ({
      id: `w:${n}`,
      name: n,
      kind: 'Event webhook',
      type: 'webhook' as const,
    })),
  ];

  async function toggle(row: MatrixRow, col: Column, on: boolean) {
    const key = `${row.id}|${col.id}`;
    setBusy(key);
    try {
      if (col.type === 'channel') {
        const plan = channelToggle(row, col.name, on, rules.data ?? []);
        for (const r of plan.replace) await alertsApi.replaceRule(r.rule.name, r.spec, r.rule.metadata.etag);
        if (plan.create) await alertsApi.createRule(plan.create.name, plan.create.spec);
        if (plan.still.length)
          notify(
            'info',
            `Still sent to ${col.name}`,
            `${plan.still.join(', ')} also cover${plan.still.length === 1 ? 's' : ''} other events; edit ${plan.still.length === 1 ? 'it' : 'them'} in Alerts.`,
          );
        else
          notify(
            'success',
            on ? `${row.label} now go to ${col.name}` : `${row.label} no longer go to ${col.name}`,
          );
      } else {
        const hook = webhooks.data?.items[col.name];
        if (!hook) return;
        const next = webhookToggle(row, hook, on);
        if (next.events.length === 0) {
          notify(
            'error',
            'A webhook needs at least one event',
            'Remove the webhook instead, in Webhooks & audit copies.',
          );
          return;
        }
        await saveWebhook(col.name, { events: next.events });
        if (next.still.length)
          notify(
            'info',
            `Still sent to ${col.name}`,
            `It also subscribes to ${next.still.join(', ')}, which covers them.`,
          );
        else
          notify(
            'success',
            on
              ? `${col.name} now receives ${row.label.toLowerCase()}`
              : `${col.name} no longer receives ${row.label.toLowerCase()}`,
          );
      }
    } catch (e) {
      notify('error', "Couldn't change where it goes", describeError(e));
    } finally {
      await loadAlertConfig({ quiet: true });
      setBusy(null);
    }
  }

  const error = rules.error ?? channels.error ?? webhooks.error;
  const ready = rules.data && channels.data && webhooks.data;
  return (
    <Card title="Who gets what" description="Which events and alerts go to which channel and webhook.">
      {error ? <ErrorState error={error} subject="Notification settings" size="sm" /> : null}
      {!ready && !error ? <LoadingRows rows={4} /> : null}
      {ready && columns.length === 0 ? (
        <EmptyState
          size="sm"
          title="No channels or webhooks yet"
          description="Connect one above, then choose what it receives here."
        />
      ) : null}
      {ready && columns.length > 0 ? (
        <div className="overflow-x-auto">
          <table className="w-full border-separate border-spacing-0 text-ui">
            <thead>
              <tr>
                <th
                  scope="col"
                  className="sticky left-0 bg-raised py-2 pr-3 text-left text-caption font-medium text-muted"
                >
                  Event
                </th>
                {columns.map((c) => (
                  <th key={c.id} scope="col" className="px-2 py-2 text-center align-bottom font-normal">
                    <span className="mono block max-w-[9rem] truncate text-caption text-fg">{c.name}</span>
                    <span className="block text-caption text-dim">{c.kind}</span>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {MATRIX_ROWS.map((row) => (
                <tr key={row.id}>
                  <th
                    scope="row"
                    className="sticky left-0 border-t border-line bg-raised py-2 pr-3 text-left font-normal text-fg"
                  >
                    {row.label}
                  </th>
                  {row.kind === 'personal' ? (
                    <td
                      colSpan={columns.length}
                      className="border-t border-line px-2 py-2 text-caption text-muted"
                    >
                      Emailed to each person who turns it on in their{' '}
                      <a href={accountHref} className="text-accent hover:underline">
                        account notifications
                      </a>
                      .
                    </td>
                  ) : (
                    columns.map((c) => {
                      const channel = (channels.data ?? []).find((x) => x.name === c.name);
                      const state =
                        c.type === 'channel' && channel
                          ? channelCell(row, channel, rules.data ?? [])
                          : webhookCell(row, webhooks.data?.items[c.name] ?? { url: '', events: [] });
                      return (
                        <td key={c.id} className="border-t border-line px-2 py-2 text-center">
                          <Cell
                            state={state}
                            label={`${row.label} to ${c.name}`}
                            busy={busy !== null}
                            canWrite={c.type === 'channel' ? canRules : canHooks}
                            onToggle={(on) => void toggle(row, c, on)}
                          />
                        </td>
                      );
                    })
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      <p className="mt-3 flex items-start gap-1.5 text-caption text-muted">
        <Icon name="info" size={14} className="mt-px shrink-0" />
        <span>
          A tick on a severity adds the channel to every alert rule of that severity. A tick on an event
          category adds it to that category&apos;s event rule, created when there is none. Fine-tune rules in{' '}
          <a href={alertsHref} className="text-accent hover:underline">
            Alerts
          </a>
          .
        </span>
      </p>
    </Card>
  );
}
