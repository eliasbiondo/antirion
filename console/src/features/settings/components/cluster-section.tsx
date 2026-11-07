'use client';

import { useEffect, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Meter } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Gate, useCan } from '@/components/ui/gate';
import { DefinitionGrid, Section } from '@/components/ui/layout';
import { Drawer } from '@/components/ui/overlay';
import { EmptyState, LoadError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { formatCount } from '@/lib/format';
import { emptySlot, failSlot, type Slot } from '@/lib/slot';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { type ClusterNode, settingsApi } from '../api';
import { useRuntime } from '../store';
import { translateNow } from '@/lib/i18n';

/** How often an open node is read again while it drains, in ms. */
export const DRAIN_POLL_MS = 2000;

function statusWords(n: ClusterNode): string {
  if (n.draining) return 'Draining';
  return n.ready ? 'Ready' : 'Not ready';
}

/** "12 / 16,384"; a dash when the gateway did not report the node's load. */
function loadWords(n: ClusterNode): string {
  if (n.in_flight === undefined) return '—';
  return n.max_in_flight !== undefined
    ? `${formatCount(n.in_flight)} / ${formatCount(n.max_in_flight)}`
    : formatCount(n.in_flight);
}

/**
 * One node, read from `/runtime/nodes/{id}`: its state, load and uptime. While it drains, it is read
 * again every few seconds so the requests still in progress count down.
 */
function NodeDrawer({
  id,
  only,
  running,
  onClose,
  onDrain,
}: {
  id: string | null;
  /** Whether it is the gateway's only node (draining it stops all traffic). */
  only: boolean;
  /** The configuration version the gateway runs, to tell a node that lags. */
  running: number | null;
  onClose: () => void;
  onDrain: (id: string) => void;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const [node, setNode] = useState<Slot<ClusterNode>>(() => ({ ...emptySlot(), status: 'loading' }));
  const draining = node.data?.draining === true && (node.data.in_flight ?? 0) > 0;

  useEffect(() => {
    if (!id) return;
    let live = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const read = () => {
      settingsApi.node(id).then(
        (data) => {
          if (!live) return;
          setNode({ data, status: 'ready', error: null, at: Date.now() });
          if (data.draining && (data.in_flight ?? 0) > 0) timer = setTimeout(read, DRAIN_POLL_MS);
        },
        (e: unknown) => live && setNode((s) => failSlot(s, e)),
      );
    };
    read();
    return () => {
      live = false;
      if (timer) clearTimeout(timer);
    };
  }, [id]);

  const n = node.data?.node_id === id ? node.data : null;
  return (
    <Drawer
      open={id !== null}
      onClose={onClose}
      title={id ? `Node ${id}` : ''}
      subtitle={n ? `${statusWords(n)} · version ${n.version}` : undefined}
      footer={
        n && !n.draining ? (
          <Gate operation="drainNode">
            <Button variant="danger" onClick={() => onDrain(n.node_id)}>
              Drain node
            </Button>
          </Gate>
        ) : null
      }
    >
      {node.status === 'loading' && !n ? <LoadingRows rows={4} /> : null}
      <LoadError of={node} subject="This node" size="sm" />
      {n ? (
        <div className="flex flex-col gap-5">
          {n.draining ? (
            <p role="status" className="text-ui text-fg">
              {draining
                ? `Draining: ${formatCount(n.in_flight ?? 0)} ${n.in_flight === 1 ? 'request' : 'requests'} still in progress.`
                : 'Drained: no request in progress. It takes none until it is restarted.'}
            </p>
          ) : null}
          {n.degraded.length > 0 ? (
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-ui text-muted">Degraded:</span>
              {n.degraded.map((d) => (
                <Badge key={d} tone="warn">
                  {d}
                </Badge>
              ))}
            </div>
          ) : null}
          <DefinitionGrid
            columns={2}
            items={[
              { label: 'Active requests', value: loadWords(n) },
              {
                label: 'Waiting requests',
                value: n.queue_depth !== undefined ? formatCount(n.queue_depth) : '—',
              },
              {
                label: 'Configuration version',
                value:
                  running !== null && n.config_version < running
                    ? `${n.config_version} (behind ${running})`
                    : String(n.config_version),
              },
              { label: 'Last seen', value: formatRelative(n.last_seen) },
              {
                label: 'Started',
                value: n.started_at ? formatDateTime(n.started_at, timeZone) : '—',
              },
              {
                label: 'Ready since',
                value: n.ready_since ? formatDateTime(n.ready_since, timeZone) : '—',
              },
            ]}
          />
          {n.in_flight !== undefined && n.max_in_flight ? (
            <Meter
              value={n.in_flight}
              max={n.max_in_flight}
              label="Active requests against the most this node accepts at once"
            />
          ) : null}
          {only && !n.draining ? (
            <p className="text-caption text-muted">
              This is the gateway&apos;s only node: draining it stops every app until it is restarted.
            </p>
          ) : null}
        </div>
      ) : null}
    </Drawer>
  );
}

/**
 * The cluster from `/cluster`: its mode and summed load, each node with its load and uptime (a
 * row opens the node), and which node holds each background job.
 */
export function ClusterSection({ running }: { running: number | null }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const cluster = useRuntime((s) => s.cluster);
  const load = useRuntime((s) => s.load);
  const canRead = useCan('getCluster');
  const [open, setOpen] = useState<string | null>(null);
  const [draining, setDraining] = useState<string | null>(null);
  // Read the open node again after it is drained, even when it was open already.
  const [reads, setReads] = useState(0);
  if (!canRead) return null;
  const c = cluster.data;
  const nodes = c?.nodes ?? [];
  const ready = nodes.filter((n) => n.ready).length;
  const t = c?.totals;
  return (
    <>
      <Section
        title="Nodes"
        subtitle={
          c
            ? [
                c.mode === 'cluster' ? 'Cluster' : 'Single node',
                `${ready} of ${nodes.length} ready`,
                ...(t
                  ? [
                      `${formatCount(t.in_flight)} of ${formatCount(t.max_in_flight)} active`,
                      `${formatCount(t.queue_depth)} waiting`,
                    ]
                  : []),
              ].join(' · ')
            : undefined
        }
      >
        {cluster.status === 'loading' && !c ? <LoadingRows rows={2} /> : null}
        <LoadError of={cluster} subject="Nodes" />
        {c && nodes.length === 0 ? <EmptyState size="sm" title="No node has reported yet" /> : null}
        {nodes.length > 0 ? (
          <Table label="Nodes">
            <thead>
              <tr>
                <Th>Node</Th>
                <Th>Status</Th>
                <Th numeric>Active</Th>
                <Th numeric className="max-md:hidden">
                  Waiting
                </Th>
                <Th numeric className="max-md:hidden">
                  Configuration
                </Th>
                <Th className="max-lg:hidden">Version</Th>
                <Th className="max-lg:hidden">Up since</Th>
                <Th className="max-md:hidden">Last seen</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {nodes.map((n) => (
                <Tr
                  key={n.node_id}
                  interactive
                  tabIndex={0}
                  aria-label={`Node ${n.node_id}`}
                  onClick={() => setOpen(n.node_id)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') setOpen(n.node_id);
                  }}
                >
                  <Td mono>{n.node_id}</Td>
                  <Td>
                    <span className="flex items-center gap-1.5">
                      <Dot tone={n.draining ? 'info' : n.ready ? 'success' : 'error'} />
                      {statusWords(n)}
                      {n.degraded.length > 0 ? (
                        <Badge tone="warn">Degraded: {n.degraded.join(', ')}</Badge>
                      ) : null}
                    </span>
                  </Td>
                  <Td numeric>{loadWords(n)}</Td>
                  <Td numeric className="max-md:hidden">
                    {n.queue_depth !== undefined ? formatCount(n.queue_depth) : '—'}
                  </Td>
                  <Td numeric className="max-md:hidden">
                    {running !== null && n.config_version < running ? (
                      <span className="text-warn">
                        {n.config_version} <span className="text-caption">behind</span>
                      </span>
                    ) : (
                      n.config_version
                    )}
                  </Td>
                  <Td mono className="max-lg:hidden">
                    {n.version}
                  </Td>
                  <Td
                    muted
                    className="max-lg:hidden"
                    title={n.started_at ? formatDateTime(n.started_at, timeZone) : undefined}
                  >
                    {n.started_at ? formatRelative(n.started_at) : '—'}
                  </Td>
                  <Td muted className="max-md:hidden">
                    {formatRelative(n.last_seen)}
                  </Td>
                  <Td className="text-right">
                    {n.draining ? null : (
                      <Gate operation="drainNode" mode="hide">
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={(e) => {
                            e.stopPropagation();
                            setDraining(n.node_id);
                          }}
                        >
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
      {c && (c.mode === 'cluster' || c.jobs.length > 0) ? (
        <Section
          title="Background jobs"
          subtitle="Work one node does for the whole cluster, such as rollups and clean-up."
        >
          {c.jobs.length === 0 ? (
            <EmptyState size="sm" title="No job is held right now" />
          ) : (
            <Table label="Background jobs">
              <thead>
                <tr>
                  <Th>Job</Th>
                  <Th>Held by</Th>
                  <Th>Until</Th>
                </tr>
              </thead>
              <tbody>
                {c.jobs.map((j) => (
                  <Tr key={j.name}>
                    <Td mono>{j.name}</Td>
                    <Td mono>{j.holder}</Td>
                    <Td muted title={formatDateTime(j.expires_at, timeZone)}>
                      {formatRelative(j.expires_at)}
                    </Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          )}
        </Section>
      ) : null}
      <NodeDrawer
        key={`${open ?? ''}-${reads}`}
        id={open}
        only={nodes.length <= 1}
        running={running}
        onClose={() => setOpen(null)}
        onDrain={setDraining}
      />
      <ConfirmDialog
        open={draining !== null}
        onClose={() => setDraining(null)}
        title={`Drain node ${draining ?? ''}?`}
        objectName={draining ?? undefined}
        confirmLabel="Drain node"
        typeToConfirm={draining ?? undefined}
        withReason={false}
        effect={
          nodes.length <= 1
            ? "This is the gateway's only node: every app gets a “try again” error until it's restarted. Requests in progress finish."
            : 'The node stops taking new requests, so your load balancer sends them to the other nodes. Requests in progress finish. It stays drained until it is restarted.'
        }
        onConfirm={async () => {
          if (!draining) return;
          const id = draining;
          const r = await settingsApi.drainNode(id);
          notify(
            'success',
            translateNow('Node {0} is draining', id),
            'in_flight' in r && typeof r.in_flight === 'number'
              ? translateNow(
                  '{0} requests still in progress. Open the node to follow them.',
                  formatCount(r.in_flight),
                )
              : undefined,
          );
          await load({ quiet: true });
          setReads((r) => r + 1);
          setOpen(id);
        }}
      />
    </>
  );
}
