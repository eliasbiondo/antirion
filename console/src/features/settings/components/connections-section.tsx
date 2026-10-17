'use client';

import { useEffect, useState } from 'react';

import type { AlertChannelResource } from '@/lib/api/types';
import { Badge, Dot } from '@/components/ui/badge';
import { Button, IconButton } from '@/components/ui/button';
import { permissionOf, useCan } from '@/components/ui/gate';
import { noPermissionReason } from '@/lib/permissions';
import { Icon, type IconName } from '@/components/ui/icon';
import { ErrorState } from '@/components/ui/states';
import { TileGrid } from '@/components/ui/layout';
import { useAlertEditors } from '@/features/alerts/editors';
import { useAlerts } from '@/features/alerts/store';
import { formatRelative } from '@/lib/time';

import { actorWords, lastChange } from '../last-change';
import { useLinks, useOrgSettings } from '../store';
import { useAlertConfig } from './alert-editors';
import {
  OtlpDrawer,
  SecretStoreDrawer,
  SinkDrawer,
  type SinkKind,
  STORE_LABEL,
  type StoreKind,
} from './connection-drawers';
import { DeliveryLog } from './delivery-log';
import { Card } from './parts';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj => (typeof v === 'object' && v !== null && !Array.isArray(v) ? (v as Obj) : {});

type Open =
  | { kind: 'otlp' }
  | { kind: 'sink'; sink: SinkKind; name: string | null }
  | { kind: 'store'; store: StoreKind }
  | null;

interface Item {
  id: string;
  name: string;
  icon: IconName;
  detail: string;
  /** What is connected: the channels, sinks or the store, by name. */
  connected: string[];
  /** Config paths whose last write is shown. */
  targets: string[];
  /** Opens the form for a new connection, or for one by name. */
  open: (name: string | null) => void;
  /** Several connections of this kind can exist. */
  many: boolean;
  canWrite: boolean;
  writeOp: Parameters<typeof permissionOf>[0];
  note?: string | undefined;
  /** Opens the delivery log of a connection by name (alert channels). */
  log?: ((name: string) => void) | undefined;
}

function ChangedLine({ targets }: { targets: string[] }) {
  const writes = useLinks((s) => s.writes.data);
  const entry = lastChange(writes, targets);
  if (!entry) return null;
  return (
    <p className="text-caption text-dim">
      Changed by {actorWords(entry)} · {formatRelative(entry.time)}
    </p>
  );
}

function ConnectionCard({ item }: { item: Item }) {
  const on = item.connected.length > 0;
  const reason = item.canWrite ? undefined : noPermissionReason(permissionOf(item.writeOp));
  return (
    <li className="flex flex-col gap-2 bg-raised px-4 py-3">
      <div className="flex items-start gap-3">
        <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-control border border-line bg-sunken text-muted">
          <Icon name={item.icon} size={16} />
        </span>
        <div className="min-w-0 flex-1">
          <p className="flex flex-wrap items-center gap-2 text-body font-semibold text-fg">
            <span>{item.name}</span>
            <Badge tone={on ? 'success' : 'neutral'} dot>
              {on ? (item.many ? `Connected (${item.connected.length})` : 'Connected') : 'Not connected'}
            </Badge>
          </p>
          <p className="text-caption text-muted">{item.detail}</p>
        </div>
      </div>
      {on && item.many ? (
        <ul className="ml-11 flex flex-wrap gap-1.5">
          {item.connected.map((n) => (
            <li key={n} className="flex items-center">
              <button
                type="button"
                onClick={() => item.open(n)}
                className="mono rounded-xs border border-line px-1.5 py-0.5 text-caption text-fg hover:border-line-strong hover:bg-hover"
              >
                {n}
              </button>
              {item.log ? (
                <IconButton
                  icon="list"
                  size={14}
                  label={`Deliveries to ${n}`}
                  onClick={() => item.log?.(n)}
                />
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
      {item.note ? <p className="ml-11 text-caption text-warn">{item.note}</p> : null}
      <div className="mt-auto ml-11 flex flex-wrap items-center justify-between gap-2">
        <ChangedLine targets={item.targets} />
        <Button
          size="sm"
          variant={on ? 'ghost' : 'secondary'}
          icon={on && !item.many ? 'edit' : 'plus'}
          disabled={!item.canWrite}
          disabledReason={reason}
          onClick={() => item.open(on && !item.many ? (item.connected[0] ?? null) : null)}
          className="ml-auto"
        >
          {on ? (item.many ? 'Add another' : 'Edit') : 'Connect'}
        </Button>
      </div>
    </li>
  );
}

function Group({ title, items }: { title: string; items: Item[] }) {
  return (
    <div className="flex flex-col gap-2">
      <h4 className="text-caption font-medium text-muted">{title}</h4>
      <TileGrid as="ul" variant="inset" cols={{ base: 1, lg: 2 }}>
        {items.map((i) => (
          <ConnectionCard key={i.id} item={i} />
        ))}
      </TileGrid>
    </div>
  );
}

const CHANNELS: {
  type: AlertChannelResource['spec']['type'];
  name: string;
  icon: IconName;
  detail: string;
}[] = [
  { type: 'slack', name: 'Slack', icon: 'messages', detail: 'Send alerts to a Slack channel.' },
  {
    type: 'pagerduty',
    name: 'PagerDuty',
    icon: 'phone',
    detail: 'Page whoever is on call for critical alerts.',
  },
  { type: 'email', name: 'Email', icon: 'mail', detail: 'Email alerts to a person or a list.' },
  { type: 'webhook', name: 'Webhook', icon: 'webhook', detail: 'Post alerts to one of your event webhooks.' },
];

/**
 * Integrations: alert channels (their editor is the one of Alerts), telemetry export, copies of the
 * request log and external secret stores, each with its state, who changed it last and its form.
 */
export function ConnectionsSection() {
  useAlertConfig();
  const loadSettings = useOrgSettings((s) => s.loadSettings);
  const observability = useOrgSettings((s) => s.observability);
  const security = useOrgSettings((s) => s.security);
  const channels = useAlerts((s) => s.channels);
  const show = useAlertEditors((s) => s.show);
  const smtp = useLinks((s) => s.smtp.data);
  const loadSmtp = useLinks((s) => s.loadSmtp);
  const loadWrites = useLinks((s) => s.loadWrites);
  const canAudit = useCan('listAudit');
  const canChannel = useCan('createAlertChannel');
  const canObs = useCan('patchObservabilitySettings');
  const canSec = useCan('patchSecuritySettings');
  const [open, setOpen] = useState<Open>(null);
  const [log, setLog] = useState<string | null>(null);
  const canLog = useCan('listAlertDeliveries');
  useEffect(() => {
    void loadSettings();
    void loadSmtp();
  }, [loadSettings, loadSmtp]);
  const obsTag = observability.data?.metadata.etag;
  const secTag = security.data?.metadata.etag;
  const chanTag = (channels.data ?? []).map((c) => c.metadata.etag).join();
  useEffect(() => {
    if (canAudit) void loadWrites();
  }, [canAudit, loadWrites, obsTag, secTag, chanTag]);

  const obs = obj(observability.data?.spec);
  const sinks = obj(obj(obj(obs.request_logs).sinks));
  const metricsOn = obj(obj(obs.metrics).otlp).enabled === true;
  const tracesOn = obj(obs.traces).enabled === true;
  const otlpLogs = Object.keys(obj(sinks.otlp));
  const stores = obj(obj(obj(security.data?.spec).secrets).stores);
  const mailOn = obj(smtp?.spec).enabled === true;

  const channelItems: Item[] = CHANNELS.map((c) => ({
    id: c.type,
    name: c.name,
    icon: c.icon,
    detail: c.detail,
    connected: (channels.data ?? []).filter((ch) => ch.spec.type === c.type).map((ch) => ch.name),
    targets: (channels.data ?? [])
      .filter((ch) => ch.spec.type === c.type)
      .map((ch) => `observability.alerts.channels.${ch.name}`),
    open: (name) => show({ kind: 'channel', name }),
    many: true,
    canWrite: canChannel,
    writeOp: 'createAlertChannel',
    log: canLog ? (name: string) => setLog(name) : undefined,
    note:
      c.type === 'email' && smtp && !mailOn
        ? 'Turn on the email server below before adding an email channel.'
        : undefined,
  }));

  const telemetry: Item[] = [
    {
      id: 'otlp',
      name: 'OpenTelemetry collector',
      icon: 'activity',
      detail:
        'Export metrics, traces and request records to a collector, Datadog or any service that takes OTLP.',
      connected: [
        ...(metricsOn ? ['metrics'] : []),
        ...(tracesOn ? ['traces'] : []),
        ...(otlpLogs.length ? ['logs'] : []),
      ],
      targets: [
        'observability.metrics.otlp',
        'observability.traces',
        'observability.request_logs.sinks.otlp',
      ],
      open: () => setOpen({ kind: 'otlp' }),
      many: false,
      canWrite: canObs,
      writeOp: 'patchObservabilitySettings',
    },
    {
      id: 'http',
      name: 'Data warehouse',
      icon: 'server',
      detail: 'Copy request-log records to your warehouse or any HTTPS intake.',
      connected: Object.keys(obj(sinks.http)),
      targets: ['observability.request_logs.sinks.http'],
      open: (name) => setOpen({ kind: 'sink', sink: 'http', name }),
      many: true,
      canWrite: canObs,
      writeOp: 'patchObservabilitySettings',
    },
    {
      id: 'langfuse',
      name: 'Trace ingestion',
      icon: 'route',
      detail:
        'Send each request as a trace, with a generation per attempt, to a Langfuse-compatible service.',
      connected: Object.keys(obj(sinks.langfuse)),
      targets: ['observability.request_logs.sinks.langfuse'],
      open: (name) => setOpen({ kind: 'sink', sink: 'langfuse', name }),
      many: true,
      canWrite: canObs,
      writeOp: 'patchObservabilitySettings',
    },
  ];

  const secretItems: Item[] = (['vault', 'aws_sm', 'gcp_sm', 'azure_kv'] as const).map((k) => ({
    id: k,
    name: STORE_LABEL[k],
    icon: k === 'vault' ? 'lock' : 'key',
    detail:
      k === 'vault'
        ? 'Keep provider keys in Vault; the configuration names them instead of holding them.'
        : `Read provider keys and other secrets from ${STORE_LABEL[k]}.`,
    connected: Object.keys(obj(stores[k])).length ? [k] : [],
    targets: [`security.secrets.stores.${k}`],
    open: () => setOpen({ kind: 'store', store: k }),
    many: false,
    canWrite: canSec,
    writeOp: 'patchSecuritySettings',
  }));

  const ready = observability.data && security.data;
  return (
    <Card title="Connections" description="Connect alerting, telemetry and secret storage.">
      <div className="flex flex-col gap-5">
        {channels.error ? <ErrorState error={channels.error} subject="Alert channels" size="sm" /> : null}
        {observability.error ? (
          <ErrorState error={observability.error} subject="Telemetry settings" size="sm" />
        ) : null}
        {security.error ? <ErrorState error={security.error} subject="Secret stores" size="sm" /> : null}
        <Group title="Alerts" items={channelItems} />
        {ready ? <Group title="Telemetry and request log" items={telemetry} /> : null}
        {ready ? <Group title="Secret stores" items={secretItems} /> : null}
        <p className="flex items-center gap-1.5 text-caption text-muted">
          <Dot tone="neutral" />
          Channels are also managed in Alerts. A secret store&apos;s health shows in Endpoints & security.
        </p>
      </div>
      {open?.kind === 'otlp' ? <OtlpDrawer onClose={() => setOpen(null)} /> : null}
      {open?.kind === 'sink' ? (
        <SinkDrawer
          kind={open.sink}
          name={open.name}
          taken={Object.keys(obj(sinks[open.sink]))}
          onClose={() => setOpen(null)}
        />
      ) : null}
      {open?.kind === 'store' ? <SecretStoreDrawer kind={open.store} onClose={() => setOpen(null)} /> : null}
      {log ? <DeliveryLog target={log} kind="channel" onClose={() => setLog(null)} /> : null}
    </Card>
  );
}
