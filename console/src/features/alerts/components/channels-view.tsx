'use client';

import { useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { useCan } from '@/components/ui/gate';
import { Icon, type IconName } from '@/components/ui/icon';
import { Panel } from '@/components/ui/layout';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { RevealOnceDialog } from '@/components/ui/reveal-once';
import { EmptyState, ErrorState, isUnavailable, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { describeError } from '@/lib/api/client';
import type { AlertChannelResource } from '@/lib/api/types';
import { formatCount, plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { notify } from '@/stores/ui';

import { alertsApi, type DeliveryStatus, type RuleSpec, type TestResult } from '../api';
import { agoWords, CHANNEL_TYPES, severityFloor } from '../describe';
import { useAlertEditors } from '../editors';
import { categoryForEvents, covers, EVENT_SOURCES } from '../events';
import { suggestName } from '../form';
import { useAlerts } from '../store';

export const CHANNEL_ICON: Record<AlertChannelResource['spec']['type'], IconName> = {
  slack: 'messages',
  pagerduty: 'phone',
  email: 'mail',
  webhook: 'webhook',
};

/** "200 · 3 min ago" with its 24-hour counts, or "Never" before the first delivery. */
function LastDelivery({ status, now }: { status: DeliveryStatus | undefined; now: number }) {
  if (!status?.last_delivery_at)
    return (
      <span className="text-muted">{status?.queued ? `${formatCount(status.queued)} queued` : 'Never'}</span>
    );
  const ok = status.last_status != null && status.last_status >= 200 && status.last_status < 300;
  return (
    <span className="block">
      <span className="inline-flex items-center gap-1.5">
        <Dot tone={ok ? 'success' : 'error'} />
        <span className="text-fg">
          {ok ? 'Delivered' : status.last_status ? `Failed (${status.last_status})` : 'Failed'}
        </span>
        <span className="text-muted">{agoWords(status.last_delivery_at, now)}</span>
      </span>
      <span className="block text-caption text-muted">
        24 h: {formatCount(status.deliveries_24h)} sent
        {status.failures_24h ? `, ${formatCount(status.failures_24h)} failed` : ''}
        {status.queued ? ` · ${formatCount(status.queued)} queued` : ''}
      </span>
    </span>
  );
}

/** Runs a test send and says how it went. */
export async function runTest(name: string, test: (name: string) => Promise<TestResult>): Promise<void> {
  try {
    const r = await test(name);
    if (r.ok)
      notify(
        'success',
        'Test delivered',
        `${name} answered${r.status_code ? ` ${r.status_code}` : ''}${r.latency_ms != null ? ` in ${Math.round(r.latency_ms)} ms` : ''}.`,
      );
    else
      notify(
        'error',
        'Test not delivered',
        r.error ?? (r.status_code ? `${name} answered ${r.status_code}.` : name),
      );
  } catch (e) {
    notify('error', 'Test not sent', describeError(e));
  }
}

function destinationOf(c: AlertChannelResource): string {
  const hint = c.metadata.secret_hints ? Object.values(c.metadata.secret_hints)[0] : undefined;
  if (c.spec.type === 'email') return (c.spec.email?.to ?? []).join(', ');
  if (c.spec.type === 'webhook') return `Event webhook ${c.spec.webhook?.webhook ?? ''}`;
  return hint ? `Secret ending ${hint.replace(/^…/, '')}` : 'Secret stored';
}

function ChannelsPanel() {
  const channels = useAlerts((s) => s.channels);
  const status = useAlerts((s) => s.channelStatus.data);
  const rules = useAlerts((s) => s.rules.data);
  const testChannel = useAlerts((s) => s.testChannel);
  const deleteChannel = useAlerts((s) => s.deleteChannel);
  const show = useAlertEditors((s) => s.show);
  const now = useNow(30_000);
  const canCreate = useCan('createAlertChannel');
  const canTest = useCan('testAlertChannel');
  const canDelete = useCan('deleteAlertChannel');
  const [deleting, setDeleting] = useState<AlertChannelResource | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const list = channels.data ?? [];
  const createDenied = canCreate ? undefined : "You don't have permission to add channels.";
  const testDenied = canTest ? undefined : "You don't have permission to test channels.";
  const add = (
    <Button
      size="sm"
      variant="primary"
      icon="plus"
      disabled={!canCreate}
      disabledReason={createDenied}
      onClick={() => show({ kind: 'channel', name: null })}
    >
      New channel
    </Button>
  );
  if (channels.error && !channels.data)
    return <ErrorState error={channels.error} subject="The channel list" />;
  if (!channels.data) return <LoadingRows label="Loading notification channels" />;
  const usedBy = (name: string) =>
    (rules ?? []).filter(
      (r) => (r.spec.channels ?? []).includes(name) || (r.spec.escalation ?? []).includes(name),
    );
  return (
    <Panel
      title="Channels"
      subtitle={list.length ? plural(list.length, 'channel') : 'Where alerts are sent'}
      bodyClassName="px-0 pb-0"
      actions={add}
    >
      {list.length === 0 ? (
        <div className="px-5 pb-6">
          <EmptyState
            title="No notification channels yet"
            description="A channel is where alerts go: Slack, PagerDuty, email or a webhook."
            size="sm"
          />
        </div>
      ) : (
        <Table label="Notification channels">
          <thead>
            <tr>
              <Th className="pl-5">Channel</Th>
              <Th>Sends</Th>
              <Th>Destination</Th>
              <Th numeric>Used by</Th>
              <Th>Last delivery</Th>
              <Th>State</Th>
              <Th className="w-28">
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {list.map((c) => {
              const used = usedBy(c.name);
              const enabled = c.spec.enabled !== false;
              const items: MenuItem[] = [
                { label: 'Edit', icon: 'edit', onSelect: () => show({ kind: 'channel', name: c.name }) },
                {
                  label: 'Delete',
                  icon: 'trash',
                  danger: true,
                  disabled: !canDelete,
                  disabledReason: canDelete ? undefined : "You don't have permission to delete channels.",
                  onSelect: () => setDeleting(c),
                },
              ];
              return (
                <Tr
                  key={c.name}
                  interactive
                  onClick={(e) => {
                    if ((e.target as HTMLElement).closest('button,a')) return;
                    show({ kind: 'channel', name: c.name });
                  }}
                >
                  <Td className="pl-5">
                    <span className="flex items-center gap-2">
                      <Icon name={CHANNEL_ICON[c.spec.type]} size={14} className="text-muted" />
                      <span className="min-w-0">
                        <span className="mono block text-fg">{c.name}</span>
                        <span className="block text-caption text-muted">{CHANNEL_TYPES[c.spec.type]}</span>
                      </span>
                    </span>
                  </Td>
                  <Td>{severityFloor(c.spec.min_severity)}</Td>
                  <Td muted className="max-w-64 truncate" title={destinationOf(c)}>
                    {destinationOf(c)}
                  </Td>
                  <Td numeric muted title={used.map((r) => r.name).join(', ') || undefined}>
                    {plural(used.length, 'rule')}
                  </Td>
                  <Td>
                    <LastDelivery status={status?.find((s) => s.name === c.name)} now={now} />
                  </Td>
                  <Td>
                    <span className="inline-flex items-center gap-1.5">
                      <Dot tone={enabled ? 'success' : 'neutral'} />
                      {enabled ? 'On' : 'Off'}
                    </span>
                  </Td>
                  <Td>
                    <span className="flex items-center justify-end gap-1">
                      <Button
                        size="sm"
                        variant="ghost"
                        icon="send"
                        busy={testing === c.name}
                        disabled={!canTest}
                        disabledReason={testDenied}
                        onClick={() => {
                          setTesting(c.name);
                          void runTest(c.name, testChannel).finally(() => setTesting(null));
                        }}
                      >
                        Test
                      </Button>
                      <MenuButton
                        label={`Actions for ${c.name}`}
                        iconOnly
                        size="sm"
                        variant="ghost"
                        items={items}
                      />
                    </span>
                  </Td>
                </Tr>
              );
            })}
          </tbody>
        </Table>
      )}
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title={`Delete channel ${deleting?.name ?? ''}?`}
        typeToConfirm={deleting?.name}
        confirmLabel="Delete channel"
        objectName={deleting?.name}
        effect={
          deleting && usedBy(deleting.name).length
            ? `${plural(usedBy(deleting.name).length, 'rule')} still notify it (${usedBy(deleting.name)
                .map((r) => r.name)
                .join(', ')}); remove it from them first, or the gateway refuses the change.`
            : 'Alerts stop going to this destination.'
        }
        onConfirm={async () => {
          if (!deleting) return;
          await deleteChannel(deleting);
          notify('success', 'Channel deleted', deleting.name);
        }}
      />
    </Panel>
  );
}

function WebhooksPanel() {
  const webhooks = useAlerts((s) => s.webhooks);
  const status = useAlerts((s) => s.webhookStatus.data);
  const channels = useAlerts((s) => s.channels.data);
  const testWebhook = useAlerts((s) => s.testWebhook);
  const saveWebhook = useAlerts((s) => s.saveWebhook);
  const deleteWebhook = useAlerts((s) => s.deleteWebhook);
  const show = useAlertEditors((s) => s.show);
  const now = useNow(30_000);
  const canWrite = useCan('patchObservabilitySettings');
  const canTest = useCan('testWebhook');
  const canRotate = useCan('rotateWebhookSecret');
  const [deleting, setDeleting] = useState<string | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const [secret, setSecret] = useState<{ name: string; value: string } | null>(null);
  const writeDenied = canWrite ? undefined : "You don't have permission to change webhooks.";
  const entries = Object.entries(webhooks.data?.items ?? {}).sort(([a], [b]) => a.localeCompare(b));
  const viaChannels = (name: string) =>
    (channels ?? []).filter((c) => c.spec.type === 'webhook' && c.spec.webhook?.webhook === name);
  return (
    <Panel
      title="Event webhooks"
      subtitle="Gateway events posted as signed JSON to your endpoint"
      bodyClassName="px-0 pb-0"
      actions={
        <Button
          size="sm"
          icon="plus"
          disabled={!canWrite}
          disabledReason={writeDenied}
          onClick={() => show({ kind: 'webhook', name: null })}
        >
          New webhook
        </Button>
      }
    >
      {webhooks.error && !webhooks.data ? (
        <div className="px-5 pb-5">
          {webhooks.error.status === 403 ? (
            <p className="text-ui text-muted">You don&apos;t have permission to see event webhooks.</p>
          ) : isUnavailable(webhooks.error) ? (
            <p className="text-ui text-muted">Event webhooks aren&apos;t available on this gateway yet.</p>
          ) : (
            <ErrorState error={webhooks.error} subject="Event webhooks" size="sm" />
          )}
        </div>
      ) : !webhooks.data ? (
        <LoadingRows rows={2} label="Loading event webhooks" />
      ) : entries.length === 0 ? (
        <div className="px-5 pb-5">
          <EmptyState
            size="sm"
            title="No event webhooks yet"
            description="A webhook receives the events it subscribes to, and can carry alerts as a channel."
          />
        </div>
      ) : (
        <Table label="Event webhooks">
          <thead>
            <tr>
              <Th className="pl-5">Webhook</Th>
              <Th>Subscribed to</Th>
              <Th>Last delivery</Th>
              <Th>State</Th>
              <Th className="w-28">
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {entries.map(([name, w]) => {
              const enabled = w.enabled !== false;
              const via = viaChannels(name);
              let host = w.url;
              try {
                const u = new URL(w.url);
                host = u.host + (u.pathname === '/' ? '' : u.pathname);
              } catch {
                // keep the text as configured
              }
              return (
                <Tr
                  key={name}
                  interactive
                  onClick={(e) => {
                    if ((e.target as HTMLElement).closest('button,a')) return;
                    show({ kind: 'webhook', name });
                  }}
                >
                  <Td className="pl-5" title={w.url}>
                    <span className="mono block text-fg">{name}</span>
                    <span className="block max-w-64 truncate text-caption text-muted">{host}</span>
                  </Td>
                  <Td className="whitespace-normal">
                    <span className="flex flex-wrap gap-1">
                      {w.events.slice(0, 3).map((ev) => (
                        <Badge key={ev} mono>
                          {ev}
                        </Badge>
                      ))}
                      {w.events.length > 3 ? <Badge>+{w.events.length - 3}</Badge> : null}
                    </span>
                    {via.length ? (
                      <span className="mt-1 block text-caption text-muted">
                        Alerts through {via.map((c) => c.name).join(', ')}
                      </span>
                    ) : null}
                  </Td>
                  <Td>
                    <LastDelivery status={status?.find((s) => s.name === name)} now={now} />
                  </Td>
                  <Td>
                    <span className="inline-flex items-center gap-1.5">
                      <Dot tone={enabled ? 'success' : 'warn'} />
                      {enabled ? 'On' : 'Paused'}
                    </span>
                  </Td>
                  <Td>
                    <span className="flex items-center justify-end gap-1">
                      <Button
                        size="sm"
                        variant="ghost"
                        icon="send"
                        busy={testing === name}
                        disabled={!canTest}
                        disabledReason={canTest ? undefined : "You don't have permission to test webhooks."}
                        onClick={() => {
                          setTesting(name);
                          void runTest(name, testWebhook).finally(() => setTesting(null));
                        }}
                      >
                        Test
                      </Button>
                      <MenuButton
                        label={`Actions for ${name}`}
                        iconOnly
                        size="sm"
                        variant="ghost"
                        items={[
                          { label: 'Edit', icon: 'edit', onSelect: () => show({ kind: 'webhook', name }) },
                          {
                            label: enabled ? 'Pause deliveries' : 'Resume deliveries',
                            icon: enabled ? 'pause' : 'play',
                            disabled: !canWrite,
                            disabledReason: writeDenied,
                            onSelect: () =>
                              void saveWebhook(name, { enabled: !enabled }).then(
                                () => notify('success', enabled ? 'Webhook paused' : 'Webhook resumed', name),
                                (e: unknown) =>
                                  notify('error', "Couldn't change the webhook", describeError(e)),
                              ),
                          },
                          {
                            label: 'Generate a signing secret',
                            icon: 'key',
                            disabled: !canRotate,
                            disabledReason: canRotate
                              ? undefined
                              : "You don't have permission to generate secrets.",
                            onSelect: () =>
                              void alertsApi.rotateWebhookSecret(name).then(
                                (r) => setSecret({ name, value: r.secret }),
                                (e: unknown) =>
                                  notify('error', "Couldn't generate a secret", describeError(e)),
                              ),
                          },
                          {
                            label: 'Delete',
                            icon: 'trash',
                            danger: true,
                            disabled: !canWrite,
                            disabledReason: writeDenied,
                            onSelect: () => setDeleting(name),
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
      )}
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title={`Delete webhook ${deleting ?? ''}?`}
        confirmLabel="Delete webhook"
        objectName={deleting ?? undefined}
        effect={
          deleting && viaChannels(deleting).length
            ? `The channel ${viaChannels(deleting)
                .map((c) => c.name)
                .join(', ')} sends through it; delete that channel first, or the gateway refuses the change.`
            : 'Events stop going to this endpoint; queued events are dropped.'
        }
        onConfirm={async () => {
          if (!deleting) return;
          await deleteWebhook(deleting);
          notify('success', 'Webhook deleted', deleting);
        }}
      />
      <RevealOnceDialog
        secret={secret?.value ?? null}
        onClose={() => setSecret(null)}
        title={`Signing secret for ${secret?.name ?? ''}`}
        label="Signing secret"
        description="Verify each delivery's x-antirion-signature header with it. It is shown only now; the previous secret keeps working for an hour."
      />
    </Panel>
  );
}

/** The gateway's built-in event sources, each with who is subscribed and how to subscribe. */
function EventSourcesPanel() {
  const webhooks = useAlerts((s) => s.webhooks.data?.items);
  const rules = useAlerts((s) => s.rules.data);
  const saveWebhook = useAlerts((s) => s.saveWebhook);
  const show = useAlertEditors((s) => s.show);
  const canRule = useCan('createAlertRule');
  const canHook = useCan('patchObservabilitySettings');
  const [open, setOpen] = useState<string | null>(EVENT_SOURCES[0]?.prefix ?? null);
  const taken = (rules ?? []).map((r) => r.name);
  const hooks = Object.entries(webhooks ?? {});
  const eventRules = (rules ?? []).filter(
    (r) => r.spec.kind === 'event' || (!r.spec.metric && r.spec.events),
  );
  return (
    <Panel
      title="Event sources"
      subtitle="Built-in events you can alert on or send to a webhook"
      bodyClassName="px-0 pb-0"
    >
      <ul className="divide-y divide-line border-t border-line">
        {EVENT_SOURCES.map((source) => {
          const expanded = open === source.prefix;
          const subscribed = source.events.filter(
            (e) =>
              hooks.some(([, w]) => w.events.some((p) => covers(p, e.type))) ||
              eventRules.some((r) => (r.spec.events ?? []).some((p) => covers(p, e.type))),
          ).length;
          return (
            <li key={source.prefix}>
              <button
                type="button"
                aria-expanded={expanded}
                onClick={() => setOpen(expanded ? null : source.prefix)}
                className="flex w-full items-center gap-3 px-5 py-2.5 text-left hover:bg-hover"
              >
                <Icon
                  name="chevronRight"
                  size={14}
                  className={
                    expanded ? 'rotate-90 text-muted transition-transform' : 'text-muted transition-transform'
                  }
                />
                <span className="text-ui font-medium text-fg">{source.label}</span>
                <span className="mono text-caption text-dim">{source.prefix}.*</span>
                <span className="ml-auto text-caption text-muted">
                  {plural(source.events.length, 'event')}
                  {subscribed ? ` · ${formatCount(subscribed)} subscribed` : ''}
                </span>
              </button>
              {expanded ? (
                <ul className="divide-y divide-line border-t border-line">
                  {source.events.map((e) => {
                    const hookNames = hooks
                      .filter(([, w]) => w.events.some((p) => covers(p, e.type)))
                      .map(([n]) => n);
                    const ruleNames = eventRules
                      .filter((r) => (r.spec.events ?? []).some((p) => covers(p, e.type)))
                      .map((r) => r.name);
                    const addTo: MenuItem[] = [
                      ...hooks
                        .filter(([n]) => !hookNames.includes(n))
                        .map(([n, w]) => ({
                          label: `Add to ${n}`,
                          icon: 'webhook' as const,
                          onSelect: () =>
                            void saveWebhook(n, { events: [...w.events, e.type] }).then(
                              () => notify('success', 'Subscribed', `${n} now receives ${e.type}`),
                              (err: unknown) => notify('error', "Couldn't subscribe", describeError(err)),
                            ),
                        })),
                      {
                        label: 'New webhook…',
                        icon: 'plus',
                        onSelect: () => show({ kind: 'webhook', name: null, events: [e.type] }),
                      },
                    ];
                    return (
                      <li
                        key={e.type}
                        className="flex flex-wrap items-start gap-x-4 gap-y-2 px-5 py-2.5 pl-12 max-sm:pl-5"
                      >
                        <div className="min-w-0 flex-1 basis-64">
                          <p className="text-ui text-fg">
                            {e.label} <span className="mono text-caption text-dim">{e.type}</span>
                          </p>
                          <p className="text-caption text-muted">{e.detail}</p>
                          {hookNames.length || ruleNames.length ? (
                            <p className="mt-1 flex flex-wrap gap-1">
                              {ruleNames.map((n) => (
                                <Badge key={`r-${n}`} tone="info">
                                  rule {n}
                                </Badge>
                              ))}
                              {hookNames.map((n) => (
                                <Badge key={`w-${n}`}>webhook {n}</Badge>
                              ))}
                            </p>
                          ) : null}
                        </div>
                        <div className="flex shrink-0 items-center gap-1.5">
                          <Button
                            size="sm"
                            icon="bell"
                            disabled={!e.rule || !canRule}
                            disabledReason={
                              !e.rule
                                ? 'An alert event cannot raise another alert; send it to a webhook instead.'
                                : canRule
                                  ? undefined
                                  : "You don't have permission to create alert rules."
                            }
                            onClick={() =>
                              show({
                                kind: 'rule',
                                name: null,
                                draft: {
                                  name: suggestName(e.type.replace(/\./g, '-'), taken),
                                  spec: {
                                    kind: 'event',
                                    events: [e.type],
                                    cooldown: '0s',
                                    category: categoryForEvents([e.type]) as RuleSpec['category'],
                                  },
                                },
                              })
                            }
                          >
                            Alert on it
                          </Button>
                          <MenuButton
                            label="Send to a webhook"
                            icon="webhook"
                            size="sm"
                            items={addTo.map((i) => ({
                              ...i,
                              disabled: !canHook,
                              disabledReason: canHook
                                ? undefined
                                : "You don't have permission to change webhooks.",
                            }))}
                          />
                        </div>
                      </li>
                    );
                  })}
                </ul>
              ) : null}
            </li>
          );
        })}
      </ul>
    </Panel>
  );
}

/** The Channels tab: where alerts go, the event webhooks and the events to subscribe to. */
export function ChannelsView() {
  return (
    <div className="flex flex-col gap-px bg-line">
      <ChannelsPanel />
      <WebhooksPanel />
      <EventSourcesPanel />
    </div>
  );
}
