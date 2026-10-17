'use client';

import { useMemo } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { Badge, Dot } from '@/components/ui/badge';

import { FavouriteButton, StarredFilter, useStarredFilter } from '@/components/ui/favourite';
import { Input, Segmented, Select, Switch } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { KeyboardHints } from '@/components/ui/keyboard-hints';
import { FilterBar, Panel } from '@/components/ui/layout';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { EmptyAction, EmptyState, NoMatch } from '@/components/ui/states';
import { CountNote, Table, Td, Th, Tr } from '@/components/ui/table';
import type { CsvColumn } from '@/lib/csv';
import { plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { useUrlFilters } from '@/lib/url-filters';
import { notify } from '@/stores/ui';

import type { McpServerResource } from '../api';
import {
  HEALTH_LABEL,
  HEALTH_TONE,
  serverHealth,
  type ServerHealth,
  TRANSPORT_LABEL,
  waitingTools,
} from '../form';
import { useMcp } from '../store';
import { describeError } from '@/lib/api/client';

export const SERVER_FILTERS = { q: '', transport: '', state: 'all' };
export type ServerStateFilter = 'all' | 'on' | 'off' | 'failing' | 'waiting';
const STATES: ServerStateFilter[] = ['all', 'on', 'off', 'failing', 'waiting'];

export const SERVER_CSV: CsvColumn<{
  server: McpServerResource;
  health: ServerHealth;
  tools: number;
  waiting: number;
}>[] = [
  { header: 'name', value: (r) => r.server.name },
  { header: 'transport', value: (r) => r.server.spec.transport },
  { header: 'url', value: (r) => r.server.spec.url ?? r.server.spec.command ?? '' },
  { header: 'prefix', value: (r) => r.server.spec.prefix ?? r.server.name },
  { header: 'enabled', value: (r) => (r.server.spec.enabled === false ? 'false' : 'true') },
  { header: 'status', value: (r) => r.health },
  { header: 'tools_seen', value: (r) => r.tools },
  { header: 'tools_waiting', value: (r) => r.waiting },
  { header: 'pin_tool_definitions', value: (r) => (r.server.spec.pin_tool_definitions ? 'true' : 'false') },
];

/** The servers as the list shows them: each with its health and tool counts. */
export function useServerRows() {
  const servers = useMcp((s) => s.servers.data);
  const statuses = useMcp((s) => s.statuses);
  return useMemo(
    () =>
      (servers ?? []).map((server) => {
        const status = statuses[server.name]?.status;
        return {
          server,
          health: serverHealth(server, status),
          tools: status?.tools.length ?? 0,
          waiting: waitingTools(status),
        };
      }),
    [servers, statuses],
  );
}

/** The Servers tab: the MCP servers the gateway reaches, their discovery status and tools waiting. */
export function ServersTab({
  onCreate,
  onApprove,
}: {
  onCreate: () => void;
  onApprove: (name: string) => void;
}) {
  const rows = useServerRows();
  const statuses = useMcp((s) => s.statuses);
  const setEnabled = useMcp((s) => s.setServerEnabled);
  const gateways = useMcp((s) => s.gateways.data);
  const canEdit = useCan('patchMcpServer');
  const canApprove = useCan('approveMcpServerTools');
  const { filters, set, setWith, active } = useUrlFilters(SERVER_FILTERS);
  const starred = useStarredFilter('mcp_server');
  const [, setSelected] = useQueryParam('server');
  const state = (
    STATES.includes(filters.state as ServerStateFilter) ? filters.state : 'all'
  ) as ServerStateFilter;
  const q = filters.q.trim().toLowerCase();
  const transports = [...new Set(rows.map((r) => r.server.spec.transport))];

  const visible = starred.apply(
    rows.filter((r) => {
      if (filters.transport && r.server.spec.transport !== filters.transport) return false;
      if (state === 'on' && r.server.spec.enabled === false) return false;
      if (state === 'off' && r.server.spec.enabled !== false) return false;
      if (state === 'failing' && r.health !== 'failing') return false;
      if (state === 'waiting' && r.waiting === 0) return false;
      if (q && !r.server.name.includes(q) && !(r.server.spec.url ?? '').toLowerCase().includes(q))
        return false;
      return true;
    }),
    (r) => r.server.name,
  );

  const clear = () => setWith(SERVER_FILTERS, { starred: null });
  const usedBy = (name: string) =>
    (gateways ?? []).filter((g) => g.spec.servers.includes(name)).map((g) => g.name);

  if (rows.length === 0)
    return (
      <EmptyState
        title="No MCP servers yet"
        description="Add the MCP servers your apps and agents should reach."
        action={
          <Gate operation="createMcpServer" mode="hide">
            <EmptyAction onClick={onCreate}>Add MCP server</EmptyAction>
          </Gate>
        }
      />
    );

  return (
    <div className="flex flex-col">
      <FilterBar>
        <div className="w-64 max-sm:w-full">
          <Input
            data-page-search
            aria-label="Search servers"
            icon="search"
            value={filters.q}
            onChange={(e) => set({ q: e.target.value })}
            placeholder="Search by name or URL"
          />
        </div>
        <Segmented<ServerStateFilter>
          label="Server status"
          value={state}
          onChange={(v) => set({ state: v })}
          className="max-w-full overflow-x-auto"
          options={[
            { value: 'all', label: 'All' },
            { value: 'on', label: 'On' },
            { value: 'off', label: 'Off' },
            { value: 'failing', label: 'Failing' },
            { value: 'waiting', label: 'Tools waiting' },
          ]}
        />
        {transports.length > 1 ? (
          <div className="w-48 max-sm:flex-1">
            <Select
              aria-label="Transport"
              value={filters.transport}
              onChange={(v) => set({ transport: v })}
              emptyOption="Any transport"
              options={transports.map((t) => ({ value: t, label: TRANSPORT_LABEL[t] }))}
            />
          </div>
        ) : null}
        <StarredFilter kind="mcp_server" pressed={starred.on} onChange={starred.set} />
        {active || starred.on ? (
          <button type="button" className="text-ui text-muted hover:text-fg" onClick={clear}>
            Clear filters
          </button>
        ) : null}
        <span className="ml-auto">
          <CountNote shown={visible.length} total={rows.length} noun="servers" singular="server" />
        </span>
      </FilterBar>
      <Panel flush>
        {visible.length === 0 ? (
          <NoMatch onClear={clear} />
        ) : (
          <Table label="MCP servers">
            <thead>
              <tr>
                <Th>Server</Th>
                <Th className="max-md:hidden">Transport</Th>
                <Th>Status</Th>
                <Th className="max-sm:hidden">Tools</Th>
                <Th className="max-lg:hidden">Gateways</Th>
                <Th>On</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {visible.map(({ server, health, tools, waiting }) => {
                const status = statuses[server.name]?.status;
                const gateways = usedBy(server.name);
                const fileOnly = server.spec.transport === 'stdio';
                const items: MenuItem[] = [
                  { label: 'Open', icon: 'arrowRight', onSelect: () => setSelected(server.name) },
                  ...(canApprove && !fileOnly
                    ? [
                        {
                          label: 'Approve current tools',
                          icon: 'checkCircle' as const,
                          onSelect: () => onApprove(server.name),
                        },
                      ]
                    : []),
                ];
                return (
                  <Tr
                    key={server.name}
                    interactive
                    tabIndex={0}
                    aria-label={`${server.name}: details`}
                    onClick={() => setSelected(server.name)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && e.target === e.currentTarget) setSelected(server.name);
                    }}
                  >
                    <Td>
                      <div className="flex items-center gap-2">
                        <span onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
                          <FavouriteButton
                            kind="mcp_server"
                            name={server.name}
                            label={`MCP server ${server.name}`}
                          />
                        </span>
                        <div className="min-w-0">
                          <div className="mono text-body text-fg">{server.name}</div>
                          <div className="mono max-w-[360px] truncate text-caption text-muted">
                            {server.spec.url ?? server.spec.command ?? '—'}
                          </div>
                        </div>
                      </div>
                    </Td>
                    <Td muted className="max-md:hidden">
                      {TRANSPORT_LABEL[server.spec.transport]}
                    </Td>
                    <Td>
                      <span className="flex items-center gap-1.5">
                        <Dot tone={HEALTH_TONE[health]} />
                        <span className="max-sm:sr-only">{HEALTH_LABEL[health]}</span>
                        {status?.last_contact_at ? (
                          <span className="text-caption text-muted max-md:hidden">
                            · {formatRelative(status.last_contact_at)}
                          </span>
                        ) : null}
                      </span>
                    </Td>
                    <Td className="max-sm:hidden">
                      <span className="flex items-center gap-2">
                        <span className="mono tabular-nums">{status ? tools : '—'}</span>
                        {waiting > 0 ? (
                          <Badge tone="warn">{plural(waiting, 'waiting', 'waiting')}</Badge>
                        ) : null}
                        {server.spec.pin_tool_definitions ? (
                          <span className="text-caption text-muted">pinned</span>
                        ) : null}
                      </span>
                    </Td>
                    <Td muted className="max-lg:hidden">
                      {gateways.length > 0 ? gateways.join(', ') : 'None'}
                    </Td>
                    <Td onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
                      <Switch
                        label={`Turn ${server.name} ${server.spec.enabled !== false ? 'off' : 'on'}`}
                        hideLabel
                        checked={server.spec.enabled !== false}
                        disabled={!canEdit || fileOnly}
                        disabledReason={
                          fileOnly
                            ? 'A local process server is set in the configuration file.'
                            : "You don't have permission to change MCP servers."
                        }
                        onChange={(on) =>
                          void setEnabled(server.name, on).then(
                            () =>
                              notify(
                                'success',
                                on ? `${server.name} turned on` : `${server.name} turned off`,
                                undefined,
                                {
                                  label: 'Undo',
                                  onClick: () =>
                                    void setEnabled(server.name, !on).catch((e: unknown) =>
                                      notify('error', `Couldn't change ${server.name}`, describeError(e)),
                                    ),
                                },
                              ),
                            (e: unknown) =>
                              notify('error', `Couldn't change ${server.name}`, describeError(e)),
                          )
                        }
                      />
                    </Td>
                    <Td
                      className="text-right"
                      onClick={(e) => e.stopPropagation()}
                      onKeyDown={(e) => e.stopPropagation()}
                    >
                      <MenuButton
                        items={items}
                        label={`Actions for ${server.name}`}
                        iconOnly
                        variant="ghost"
                        size="sm"
                      />
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        )}
      </Panel>
      <KeyboardHints keys={['search', 'open', 'close', 'export', 'new']} />
    </div>
  );
}
