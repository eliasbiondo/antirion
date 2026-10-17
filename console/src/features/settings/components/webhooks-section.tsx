'use client';

import { useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { permissionOf, useCan } from '@/components/ui/gate';
import { noPermissionReason } from '@/lib/permissions';
import { Icon } from '@/components/ui/icon';
import { MenuButton } from '@/components/ui/menu';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { runTest } from '@/features/alerts/components/channels-view';
import { useAlertEditors } from '@/features/alerts/editors';
import { eventWords } from '@/features/alerts/events';
import { useAlerts } from '@/features/alerts/store';
import { describeError } from '@/lib/api/client';
import { formatCount } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';

import { useOrgSettings } from '../store';
import { useAlertConfig } from './alert-editors';
import { DeliveryLog } from './delivery-log';
import { Card } from './parts';

/** Up to three subscriptions in words, then how many more. */
function eventsWords(events: string[]): string {
  const words = events.map(eventWords);
  return words.length > 3 ? `${words.slice(0, 3).join(', ')} and ${words.length - 3} more` : words.join(', ');
}

/**
 * Event webhooks: endpoint, events, signing secret's last four, state with pause and resume, the
 * last day's deliveries and failures, a test send and the delivery log; created and edited in the
 * Alerts editor.
 */
export function WebhooksSection() {
  useAlertConfig();
  const webhooks = useAlerts((s) => s.webhooks);
  const status = useAlerts((s) => s.webhookStatus.data);
  const saveWebhook = useAlerts((s) => s.saveWebhook);
  const deleteWebhook = useAlerts((s) => s.deleteWebhook);
  const testWebhook = useAlerts((s) => s.testWebhook);
  const show = useAlertEditors((s) => s.show);
  const loadSettings = useOrgSettings((s) => s.loadSettings);
  const hints = useOrgSettings((s) => s.observability.data?.metadata.secret_hints);
  const canWrite = useCan('patchObservabilitySettings');
  const canTest = useCan('testWebhook');
  const canLog = useCan('listAlertDeliveries');
  const [log, setLog] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  useEffect(() => {
    void loadSettings();
  }, [loadSettings, webhooks.data]);
  const writeReason = canWrite ? undefined : noPermissionReason(permissionOf('patchObservabilitySettings'));
  const entries = Object.entries(webhooks.data?.items ?? {});
  const active = entries.filter(([, w]) => w.enabled !== false).length;
  return (
    <Card
      title="Webhooks"
      description={
        entries.length
          ? `Send gateway events to your own endpoints. ${active} of ${entries.length} delivering.`
          : 'Send gateway events to your own endpoints.'
      }
      actions={
        <Button
          size="sm"
          variant="primary"
          icon="plus"
          disabled={!canWrite}
          disabledReason={writeReason}
          onClick={() => show({ kind: 'webhook', name: null })}
        >
          New webhook
        </Button>
      }
    >
      <div className="flex flex-col gap-4">
        {webhooks.error ? <ErrorState error={webhooks.error} subject="Webhooks" size="sm" /> : null}
        {!webhooks.data && !webhooks.error ? <LoadingRows rows={2} /> : null}
        {webhooks.data && entries.length === 0 ? (
          <EmptyState
            size="sm"
            title="No webhooks yet"
            description="Add one to receive budget, key and configuration events as they happen."
          />
        ) : null}
        {entries.length > 0 ? (
          <Table label="Webhooks">
            <thead>
              <tr>
                <Th>Webhook</Th>
                <Th>Events</Th>
                <Th numeric>Last 24 h</Th>
                <Th>Last delivery</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {entries.map(([name, w]) => {
                const st = status?.find((s) => s.name === name);
                const paused = w.enabled === false;
                const hint = hints?.[`alerts.webhooks.${name}.signing_secret`];
                const signed = Boolean(w.signing_secret);
                return (
                  <Tr key={name} interactive onClick={() => show({ kind: 'webhook', name })}>
                    <Td className="w-full max-w-0 py-2 whitespace-normal">
                      <span className="flex items-center gap-2">
                        <span className="mono truncate font-medium text-fg">{name}</span>
                        <Badge tone={paused ? 'warn' : 'success'} dot>
                          {paused ? 'Paused' : 'Delivering'}
                        </Badge>
                      </span>
                      <span className="mono block truncate text-caption text-muted">{w.url}</span>
                      <span className="flex items-center gap-1 text-caption text-dim">
                        <Icon name="key" size={12} />
                        {signed ? `Signed, secret ${hint ?? 'set by reference'}` : 'Not signed'}
                      </span>
                    </Td>
                    <Td className="max-w-[14rem] truncate" title={w.events.map(eventWords).join(', ')}>
                      {w.events.includes('*') ? 'Every event' : eventsWords(w.events)}
                    </Td>
                    <Td numeric>
                      {st ? formatCount(st.deliveries_24h) : '—'}
                      {st?.failures_24h ? (
                        <span className="block text-caption text-warn">
                          {formatCount(st.failures_24h)} failed
                        </span>
                      ) : null}
                    </Td>
                    <Td muted>
                      {st?.last_delivery_at ? (
                        <>
                          {formatRelative(st.last_delivery_at)}
                          {st.last_status ? (
                            <span
                              className={
                                st.last_status < 300
                                  ? 'block text-caption text-success'
                                  : 'block text-caption text-error'
                              }
                            >
                              {st.last_status < 300 ? 'Delivered' : `Failed (${st.last_status})`}
                            </span>
                          ) : null}
                        </>
                      ) : (
                        'Never'
                      )}
                    </Td>
                    <Td>
                      <span
                        className="flex items-center justify-end gap-1"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <Button
                          size="sm"
                          icon="send"
                          busy={busy === name}
                          disabled={!canTest}
                          disabledReason={
                            canTest ? undefined : noPermissionReason(permissionOf('testWebhook'))
                          }
                          onClick={() => {
                            setBusy(name);
                            void runTest(name, testWebhook).finally(() => setBusy(null));
                          }}
                        >
                          Send test
                        </Button>
                        <MenuButton
                          label={`More for ${name}`}
                          iconOnly
                          variant="ghost"
                          size="sm"
                          items={[
                            { label: 'Edit', icon: 'edit', onSelect: () => show({ kind: 'webhook', name }) },
                            {
                              label: 'Delivery log',
                              icon: 'list',
                              disabled: !canLog,
                              disabledReason: canLog
                                ? undefined
                                : noPermissionReason(permissionOf('listAlertDeliveries')),
                              onSelect: () => setLog(name),
                            },
                            {
                              label: paused ? 'Resume' : 'Pause',
                              icon: paused ? 'play' : 'pause',
                              disabled: !canWrite,
                              disabledReason: writeReason,
                              onSelect: () =>
                                void saveWebhook(name, { enabled: paused }).then(
                                  () =>
                                    notify(
                                      'success',
                                      paused ? 'Webhook resumed' : 'Webhook paused',
                                      paused
                                        ? 'Queued events are sent now.'
                                        : 'Events queue until you resume it, for up to an hour.',
                                    ),
                                  (e: unknown) =>
                                    notify('error', "Couldn't change the webhook", describeError(e)),
                                ),
                            },
                            {
                              label: 'Remove',
                              icon: 'trash',
                              danger: true,
                              disabled: !canWrite,
                              disabledReason: writeReason,
                              onSelect: () => setRemoving(name),
                            },
                          ]}
                        />
                      </span>
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : null}
        <details className="group rounded-control border border-line px-3.5 py-2.5">
          <summary className="flex cursor-pointer list-none items-center gap-1.5 text-ui text-fg [&::-webkit-details-marker]:hidden">
            <Icon
              name="chevronRight"
              size={14}
              className="text-muted transition-transform group-open:rotate-90"
            />
            How deliveries work
          </summary>
          <ul className="mt-2 flex list-disc flex-col gap-1.5 pl-5 text-caption text-muted">
            <li>
              Each event is a JSON POST with its id and type in the{' '}
              <span className="mono">x-antirion-event-id</span> and{' '}
              <span className="mono">x-antirion-event-type</span> headers.
            </li>
            <li>
              With a signing secret, <span className="mono">x-antirion-signature</span> carries{' '}
              <span className="mono">
                t=&lt;unix seconds&gt;,v1=&lt;HMAC-SHA256 of &quot;t.body&quot;&gt;
              </span>
              . Check it, and refuse a timestamp older than a few minutes so a captured delivery can&apos;t be
              replayed.
            </li>
            <li>
              Delivery is at least once and in no set order: a failed delivery is retried with growing pauses
              for up to an hour (or the webhook&apos;s retry window), with the same event id. Skip ids you
              have already handled.
            </li>
          </ul>
        </details>
      </div>
      {log ? <DeliveryLog target={log} kind="webhook" onClose={() => setLog(null)} /> : null}
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title={`Remove webhook ${removing ?? ''}?`}
        objectName={removing ?? undefined}
        confirmLabel="Remove webhook"
        effect="Its endpoint stops receiving events, and events still queued for it are dropped. Channels that send alerts through it stop working."
        onConfirm={async () => {
          if (!removing) return;
          await deleteWebhook(removing);
          notify('success', 'Webhook removed', removing);
        }}
      />
    </Card>
  );
}
