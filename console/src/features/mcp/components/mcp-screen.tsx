'use client';

import { useEffect, useMemo, useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { Button } from '@/components/ui/button';
import { Forbidden, Gate, usePermission } from '@/components/ui/gate';
import { PageHeader, SummaryStrip, Tabs } from '@/components/ui/layout';
import { UpdatedAt } from '@/components/ui/misc';
import { ErrorState, isUnavailable, LoadingRows, UnavailableState } from '@/components/ui/states';
import { ExportCsvButton } from '@/components/ui/table';
import { exportCsv } from '@/lib/csv';
import { formatCount } from '@/lib/format';
import { usePolling } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';

import { useMcp } from '../store';
import { GATEWAY_CSV, GatewaysTab } from './gateways-tab';
import { GatewayDrawer } from './gateway-drawer';
import { ApproveToolsDialog, ServerDrawer } from './server-drawer';
import { SERVER_CSV, SERVER_FILTERS, ServersTab, useServerRows } from './servers-tab';
import { SessionsTab } from './sessions-tab';

type Tab = 'servers' | 'gateways' | 'sessions';
const TABS: Tab[] = ['servers', 'gateways', 'sessions'];

/**
 * MCP: the servers whose tools apps and agents reach through the gateway (with tool approval), the
 * gateways clients connect to, and their sessions.
 */
export function McpScreen() {
  const canRead = usePermission('mcp:read');
  const servers = useMcp((s) => s.servers);
  const gateways = useMcp((s) => s.gateways);
  const sessions = useMcp((s) => s.sessions);
  const load = useMcp((s) => s.load);
  const loadSessions = useMcp((s) => s.loadSessions);
  const rows = useServerRows();
  const [tabParam, setTabParam] = useQueryParam('tab');
  const [serverParam, setServerParam] = useQueryParam('server');
  const [gatewayParam, setGatewayParam] = useQueryParam('gateway');
  const [creating, setCreating] = useState<'server' | 'gateway' | null>(null);
  const [approving, setApproving] = useState<string | null>(null);
  const filters = useUrlFilters(SERVER_FILTERS);
  const tab: Tab = TABS.includes(tabParam as Tab) ? (tabParam as Tab) : 'servers';

  useEffect(() => {
    if (!canRead) return;
    void load();
    void loadSessions();
  }, [canRead, load, loadSessions]);
  usePolling(() => {
    void load({ quiet: true });
    void loadSessions({ quiet: true });
  }, canRead);

  const summary = useMemo(() => {
    const on = rows.filter((r) => r.server.spec.enabled !== false).length;
    const failing = rows.filter((r) => r.health === 'failing').length;
    const waiting = rows.reduce((a, r) => a + r.waiting, 0);
    const issued = (sessions.data ?? []).reduce((a, s) => a + s.issued, 0);
    return { on, failing, waiting, issued };
  }, [rows, sessions.data]);

  const create = () => setCreating(tab === 'gateways' ? 'gateway' : 'server');
  usePageShortcut('n', create);
  usePageShortcut('e', () => {
    if (tab === 'servers' && rows.length > 0) exportCsv('mcp-servers', SERVER_CSV, rows);
    if (tab === 'gateways' && gateways.data?.length) exportCsv('mcp-gateways', GATEWAY_CSV, gateways.data);
  });

  if (!canRead) return <Forbidden what="MCP servers and gateways" />;

  const unavailable = isUnavailable(servers.error);
  const selectedServer = servers.data?.find((s) => s.name === serverParam) ?? null;
  const selectedGateway = gateways.data?.find((g) => g.name === gatewayParam) ?? null;

  return (
    <>
      <PageHeader
        title="MCP"
        description="Tool servers your apps and agents reach through the gateway, and the addresses they connect to"
        actions={
          <>
            <UpdatedAt at={servers.at} />
            {tab === 'servers' ? (
              <ExportCsvButton filename="mcp-servers" columns={SERVER_CSV} rows={rows} />
            ) : tab === 'gateways' ? (
              <ExportCsvButton filename="mcp-gateways" columns={GATEWAY_CSV} rows={gateways.data ?? []} />
            ) : null}
            {tab === 'gateways' ? (
              <Gate operation="createMcpGateway" mode="hide">
                <Button variant="primary" icon="plus" onClick={() => setCreating('gateway')}>
                  Add gateway
                </Button>
              </Gate>
            ) : (
              <Gate operation="createMcpServer" mode="hide">
                <Button variant="primary" icon="plus" onClick={() => setCreating('server')}>
                  Add MCP server
                </Button>
              </Gate>
            )}
          </>
        }
      />
      {unavailable ? (
        <UnavailableState subject="MCP" operation="GET /mcp/servers" />
      ) : (
        <>
          {servers.data && rows.length > 0 ? (
            <SummaryStrip
              label="MCP summary"
              items={[
                { label: 'Servers', value: formatCount(rows.length), sub: `${summary.on} on` },
                {
                  label: 'Failing',
                  value: formatCount(summary.failing),
                  tone: summary.failing > 0 ? 'error' : 'neutral',
                  onClick:
                    summary.failing > 0
                      ? () => filters.setWith({ state: 'failing' }, { tab: null })
                      : undefined,
                  title: 'Servers the gateway could not reach the last time it tried',
                },
                {
                  label: 'Tools waiting for approval',
                  value: formatCount(summary.waiting),
                  tone: summary.waiting > 0 ? 'warn' : 'neutral',
                  onClick:
                    summary.waiting > 0
                      ? () => filters.setWith({ state: 'waiting' }, { tab: null })
                      : undefined,
                  title: 'Pinned tools not approved, and definitions changed since the approval',
                },
                {
                  label: 'Gateways',
                  value: gateways.data ? formatCount(gateways.data.length) : '—',
                  onClick: () => setTabParam('gateways'),
                },
                {
                  label: 'Sessions issued',
                  value: sessions.data ? formatCount(summary.issued) : '—',
                  sub: 'this node, since it started',
                  onClick: () => setTabParam('sessions'),
                },
              ]}
            />
          ) : null}
          <Tabs<Tab>
            label="MCP views"
            value={tab}
            onChange={(v) => setTabParam(v === 'servers' ? null : v)}
            items={[
              {
                value: 'servers',
                label: 'Servers',
                count: servers.data ? rows.length : undefined,
                icon: 'tools',
              },
              { value: 'gateways', label: 'Gateways', count: gateways.data?.length, icon: 'route' },
              { value: 'sessions', label: 'Sessions', icon: 'activity' },
            ]}
          />
          <div className="flex flex-col">
            {servers.status === 'loading' && !servers.data ? <LoadingRows rows={4} /> : null}
            {servers.error ? (
              <ErrorState error={servers.error} subject="MCP servers" onRetry={() => void load()} />
            ) : null}
            {servers.data && tab === 'servers' ? (
              <ServersTab onCreate={() => setCreating('server')} onApprove={setApproving} />
            ) : null}
            {servers.data && tab === 'gateways' ? (
              <GatewaysTab onCreate={() => setCreating('gateway')} />
            ) : null}
            {tab === 'sessions' ? <SessionsTab /> : null}
          </div>
        </>
      )}
      <ServerDrawer
        key={`server:${creating === 'server' ? '+new' : (selectedServer?.name ?? '')}`}
        server={creating === 'server' ? null : selectedServer}
        creating={creating === 'server'}
        onClose={() => {
          setCreating(null);
          setServerParam(null);
        }}
        onCreated={(name) => {
          setCreating(null);
          setServerParam(name);
        }}
      />
      <ApproveToolsDialog name={approving} onClose={() => setApproving(null)} />
      <GatewayDrawer
        key={`gateway:${creating === 'gateway' ? '+new' : (selectedGateway?.name ?? '')}`}
        gateway={creating === 'gateway' ? null : selectedGateway}
        creating={creating === 'gateway'}
        onClose={() => {
          setCreating(null);
          setGatewayParam(null);
        }}
        onCreated={(name) => {
          setCreating(null);
          setGatewayParam(name);
        }}
      />
    </>
  );
}
