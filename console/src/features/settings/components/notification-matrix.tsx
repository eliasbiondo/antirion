'use client';

import Link from 'next/link';
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
import { translateNow, useTranslate } from '@/lib/i18n';
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
  const translate = useTranslate();
  if (state.kind === 'blocked')
    return (
      <Tooltip content={state.detail || null}>
        <span aria-label={translate('{0}: not available', label)} className="text-dim">
          –
        </span>
      </Tooltip>
    );
  return (
    <Tooltip
      content={canWrite ? state.detail : `${state.detail} ${translate("Your role can't change this.")}`}
    >
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
  const translate = useTranslate();
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
            translateNow('Still sent to {0}', col.name),
            translateNow(
              plan.still.length === 1
                ? '{0} also covers other events; edit it in Alerts.'
                : '{0} also cover other events; edit them in Alerts.',
              plan.still.join(', '),
            ),
          );
        else
          notify(
            'success',
            on
              ? translateNow('{0} now go to {1}', translateNow(row.label), col.name)
              : translateNow('{0} no longer go to {1}', translateNow(row.label), col.name),
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
            translateNow('Still sent to {0}', col.name),
            translateNow('It also subscribes to {0}, which covers them.', next.still.join(', ')),
          );
        else
          notify(
            'success',
            on
              ? translateNow('{0} now receives {1}', col.name, translateNow(row.label).toLowerCase())
              : translateNow('{0} no longer receives {1}', col.name, translateNow(row.label).toLowerCase()),
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
                  className="sticky inset-s-0 bg-raised py-2 pe-3 text-start text-caption font-medium text-muted"
                >
                  {translate('Event')}
                </th>
                {columns.map((c) => (
                  <th key={c.id} scope="col" className="px-2 py-2 text-center align-bottom font-normal">
                    <span className="mono block max-w-[9rem] truncate text-caption text-fg">{c.name}</span>
                    <span className="block text-caption text-dim">{translate(c.kind)}</span>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {MATRIX_ROWS.map((row) => (
                <tr key={row.id}>
                  <th
                    scope="row"
                    className="sticky inset-s-0 border-t border-line bg-raised py-2 pe-3 text-start font-normal text-fg"
                  >
                    {translate(row.label)}
                  </th>
                  {row.kind === 'personal' ? (
                    <td
                      colSpan={columns.length}
                      className="border-t border-line px-2 py-2 text-caption text-muted"
                    >
                      {translate('Emailed to each person who turns it on in their')}{' '}
                      <Link href={accountHref} className="text-accent hover:underline">
                        {translate('account notifications')}
                      </Link>
                      .
                    </td>
                  ) : (
                    columns.map((c) => {
                      const channel = (channels.data ?? []).find((x) => x.name === c.name);
                      const state =
                        c.type === 'channel' && channel
                          ? channelCell(row, channel, rules.data ?? [], translate)
                          : webhookCell(
                              row,
                              webhooks.data?.items[c.name] ?? { url: '', events: [] },
                              translate,
                            );
                      return (
                        <td key={c.id} className="border-t border-line px-2 py-2 text-center">
                          <Cell
                            state={state}
                            label={translate('{0} to {1}', translate(row.label), c.name)}
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
          {translate(
            "A tick on a severity adds the channel to every alert rule of that severity. A tick on an event category adds it to that category's event rule, created when there is none. Fine-tune rules in",
          )}{' '}
          <Link href={alertsHref} className="text-accent hover:underline">
            {translate('Alerts')}
          </Link>
          .
        </span>
      </p>
    </Card>
  );
}
