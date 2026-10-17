'use client';

import { useEffect, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Meter } from '@/components/ui/charts';
import { Gate } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { DefinitionGrid, Section } from '@/components/ui/layout';
import { UpdatedAt } from '@/components/ui/misc';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { formatBytes, formatCount } from '@/lib/format';
import { usePolling } from '@/lib/hooks';
import { formatRelative } from '@/lib/time';
import { useShell } from '@/stores/shell';
import { notify } from '@/stores/ui';

import { healthName } from '@/features/models/types';
import { TestModePanel } from '@/features/test-mode/components/test-mode-panel';

import { settingsApi } from '../api';
import { useRuntime } from '../store';

const DEPENDENCY_LABEL: Record<string, string> = {
  store: 'Database',
  cache: 'Cache',
  queue: 'Queue',
};

/** Runtime health: dependencies, admission, spool, nodes, deployment breakers and provider credentials. */
export function HealthSection() {
  const health = useRuntime((s) => s.health);
  const deployments = useRuntime((s) => s.deployments);
  const providers = useRuntime((s) => s.providerStatus);
  const nodes = useRuntime((s) => s.nodes);
  const load = useRuntime((s) => s.load);
  const resetDeployment = useRuntime((s) => s.resetDeployment);
  const status = useShell((s) => s.status);
  const [draining, setDraining] = useState<string | null>(null);

  useEffect(() => {
    void load();
  }, [load]);
  usePolling(() => void load({ quiet: true }));

  const h = health.data;
  return (
    <div className="flex flex-col">
      <Section
        title="Gateway"
        subtitle={
          status
            ? `Version ${status.version}${status.test_mode ? ' · Test mode' : ''} · ${status.mode === 'standalone' ? 'Single node' : 'Cluster'}`
            : undefined
        }
        actions={<UpdatedAt at={health.at} />}
      >
        {health.status === 'loading' && !h ? <LoadingRows rows={3} /> : null}
        {health.error ? (
          <ErrorState error={health.error} subject="Health" onRetry={() => void load()} />
        ) : null}
        {h ? (
          <div className="flex flex-col gap-4">
            <div className="flex flex-wrap items-center gap-2">
              <Badge tone={h.status === 'ok' ? 'success' : h.status === 'draining' ? 'info' : 'warn'} dot>
                {h.status === 'ok' ? 'Operational' : h.status === 'draining' ? 'Draining' : 'Degraded'}
              </Badge>
              <span className="text-ui text-muted">Configuration version {h.config.generation}</span>
            </div>
            <details className="group order-last">
              <summary className="flex w-fit cursor-pointer list-none items-center gap-1 text-caption text-muted hover:text-fg [&::-webkit-details-marker]:hidden">
                <Icon name="chevronRight" size={12} className="transition-transform group-open:rotate-90" />
                Technical details
              </summary>
              <dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-caption">
                <dt className="text-muted">Build</dt>
                <dd className="mono text-fg">{status?.commit ?? '—'}</dd>
                <dt className="text-muted">Configuration hash</dt>
                <dd className="mono break-all text-fg">{h.config.hash}</dd>
                <dt className="text-muted">Clock difference</dt>
                <dd className="mono text-fg">{h.clock_skew_ms ?? '—'} ms</dd>
              </dl>
            </details>
            <DefinitionGrid
              columns={4}
              items={[
                {
                  label: 'Active requests',
                  value: `${formatCount(h.admission.in_flight)} / ${formatCount(h.admission.max_in_flight)}`,
                },
                {
                  label: 'Waiting requests',
                  value: `${formatCount(h.admission.queued)} / ${formatCount(h.admission.max_queue)}`,
                },
                {
                  label: 'Log buffer',
                  value: `${formatBytes(h.spool.bytes)} / ${formatBytes(h.spool.max_bytes)}`,
                },
                { label: 'Records waiting to be written', value: formatCount(h.spool.records) },
              ]}
            />
            <Meter
              value={h.admission.in_flight}
              max={h.admission.max_in_flight}
              label="Active requests against the most this node accepts at once"
            />
            <ul className="flex flex-col gap-1.5">
              {Object.entries(h.dependencies).map(([name, d]) => (
                <li key={name} className="flex items-center gap-2 text-ui">
                  <Dot tone={d.status === 'up' ? 'success' : 'error'} />
                  <span className="text-fg">{DEPENDENCY_LABEL[name] ?? name}</span>
                  <span className="text-muted">{d.status === 'up' ? 'Healthy' : 'Not responding'}</span>
                  {d.latency_ms !== null ? (
                    <span className="mono text-caption text-dim">{d.latency_ms} ms</span>
                  ) : null}
                  {d.last_error ? (
                    <span className="truncate text-caption text-error">{d.last_error}</span>
                  ) : null}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
      </Section>
      <TestModePanel />
      <Section
        title="Nodes"
        subtitle={
          nodes.data ? `${nodes.data.filter((n) => n.ready).length} of ${nodes.data.length} ready` : undefined
        }
      >
        {nodes.error ? <ErrorState error={nodes.error} subject="Nodes" /> : null}
        {nodes.data ? (
          <Table label="Nodes">
            <thead>
              <tr>
                <Th>Node</Th>
                <Th>Version</Th>
                <Th numeric>Configuration</Th>
                <Th>Status</Th>
                <Th>Last seen</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {nodes.data.map((n) => (
                <Tr key={n.node_id}>
                  <Td mono>{n.node_id}</Td>
                  <Td mono>{n.version}</Td>
                  <Td numeric>{n.config_version}</Td>
                  <Td>
                    <span className="flex items-center gap-1.5">
                      <Dot tone={n.ready ? 'success' : 'error'} />
                      {n.draining ? 'Draining' : n.ready ? 'Ready' : 'Not ready'}
                      {n.degraded.length > 0 ? (
                        <Badge tone="warn">Degraded: {n.degraded.join(', ')}</Badge>
                      ) : null}
                    </span>
                  </Td>
                  <Td muted>{formatRelative(n.last_seen)}</Td>
                  <Td className="text-right">
                    {n.draining ? null : (
                      <Gate operation="drainNode" mode="hide">
                        <Button size="sm" variant="ghost" onClick={() => setDraining(n.node_id)}>
                          Drain
                        </Button>
                      </Gate>
                    )}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        ) : null}
      </Section>
      <ConfirmDialog
        open={draining !== null}
        onClose={() => setDraining(null)}
        title={`Drain node ${draining ?? ''}?`}
        objectName={draining ?? undefined}
        confirmLabel="Drain node"
        typeToConfirm={draining ?? undefined}
        withReason={false}
        effect={
          (nodes.data?.length ?? 0) <= 1
            ? "This is the gateway's only node: every app gets a “try again” error until it's restarted. Requests in progress finish."
            : 'The node stops taking new requests, so your load balancer sends them to the other nodes. Requests in progress finish. It stays drained until it is restarted.'
        }
        onConfirm={async () => {
          if (!draining) return;
          const r = await settingsApi.drainNode(draining);
          notify(
            'success',
            `Node ${draining} is draining`,
            'in_flight' in r && typeof r.in_flight === 'number'
              ? `${formatCount(r.in_flight)} requests still in progress.`
              : undefined,
          );
          await load({ quiet: true });
        }}
      />
      <Section
        title="Deployments"
        subtitle="A deployment that keeps failing is paused for a while, then tried again."
      >
        {deployments.error ? (
          <ErrorState error={deployments.error} subject="Deployment health" size="sm" />
        ) : null}
        {deployments.data?.length === 0 ? <EmptyState size="sm" title="No deployments yet" /> : null}
        {deployments.data && deployments.data.length > 0 ? (
          <Table label="Deployment health">
            <thead>
              <tr>
                <Th>Deployment</Th>
                <Th>Status</Th>
                <Th numeric>Errors / requests</Th>
                <Th numeric>Times paused</Th>
                <Th>Tried again</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {deployments.data.map((d) => (
                <Tr key={healthName(d)}>
                  <Td mono>{healthName(d)}</Td>
                  <Td>
                    <span className="flex items-center gap-1.5">
                      <Dot
                        tone={d.state === 'closed' ? 'success' : d.state === 'half_open' ? 'warn' : 'error'}
                      />
                      {d.state === 'closed'
                        ? 'Healthy'
                        : d.state === 'half_open'
                          ? 'Testing again'
                          : 'Paused after errors'}
                    </span>
                  </Td>
                  <Td numeric>
                    {d.window.failures} / {d.window.requests}
                  </Td>
                  <Td numeric>{d.trips}</Td>
                  <Td muted>{d.cooldown_until ? formatRelative(d.cooldown_until) : '—'}</Td>
                  <Td className="text-right">
                    {d.state !== 'closed' ? (
                      <Gate operation="resetDeployment" mode="hide">
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() =>
                            void resetDeployment(healthName(d)).then(() =>
                              notify('success', 'Deployment resumed', healthName(d)),
                            )
                          }
                        >
                          Resume now
                        </Button>
                      </Gate>
                    ) : null}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        ) : null}
      </Section>
      <Section title="Provider keys" subtitle="Whether the gateway's key for each provider works.">
        {providers.error ? <ErrorState error={providers.error} subject="Provider status" size="sm" /> : null}
        {providers.data?.length === 0 ? <EmptyState size="sm" title="No providers yet" /> : null}
        <ul className="flex flex-col gap-1.5">
          {(providers.data ?? []).map((p) => (
            <li key={p.name} className="flex items-center gap-2 text-ui">
              <Dot tone={p.credential.state === 'valid' ? 'success' : 'error'} />
              <span className="mono text-fg">{p.name}</span>
              <span className="text-muted">
                {p.type} · {p.credential.state === 'valid' ? 'Valid' : p.credential.state}
              </span>
            </li>
          ))}
        </ul>
      </Section>
    </div>
  );
}
