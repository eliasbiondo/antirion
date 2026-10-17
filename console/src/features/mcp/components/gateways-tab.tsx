'use client';

import { useQueryParam } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';

import { Input, Switch } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { FilterBar, Panel } from '@/components/ui/layout';
import { EmptyAction, EmptyState, NoMatch } from '@/components/ui/states';
import { CountNote, Table, Td, Th, Tr } from '@/components/ui/table';
import type { CsvColumn } from '@/lib/csv';
import { useUrlFilters } from '@/lib/url-filters';
import { useShell } from '@/stores/shell';
import { notify } from '@/stores/ui';

import type { McpGatewayResource, McpSessionStats } from '../api';
import { dataPlaneAddress } from '../fields';
import { gatewayPaths } from '../form';
import { useMcp } from '../store';
import { describeError } from '@/lib/api/client';

const NONE: never[] = [];

export const GATEWAY_FILTERS = { gq: '' };

export const GATEWAY_CSV: CsvColumn<McpGatewayResource>[] = [
  { header: 'name', value: (g) => g.name },
  { header: 'path', value: (g) => gatewayPaths(g.name).join(' ') },
  { header: 'servers', value: (g) => g.spec.servers.join(' ') },
  { header: 'session_mode', value: (g) => g.spec.session_mode ?? 'stateful' },
  { header: 'prefix_mode', value: (g) => g.spec.prefix_mode ?? 'auto' },
  { header: 'on_server_failure', value: (g) => g.spec.on_server_failure ?? 'partial' },
  { header: 'tool_search', value: (g) => (g.spec.tool_search ? 'true' : 'false') },
  { header: 'oauth', value: (g) => (g.spec.oauth ? 'true' : 'false') },
  { header: 'enabled', value: (g) => (g.spec.enabled === false ? 'false' : 'true') },
];

export const SESSION_MODE_LABEL: Record<McpSessionStats['session_mode'], string> = {
  stateful: 'Kept by the gateway',
  stateless: 'Each request alone',
};

/** The Gateways tab: the endpoints clients connect to, and the servers each combines. */
export function GatewaysTab({ onCreate }: { onCreate: () => void }) {
  const gateways = useMcp((s) => s.gateways.data) ?? NONE;
  const sessions = useMcp((s) => s.sessions.data);
  const setEnabled = useMcp((s) => s.setGatewayEnabled);
  const canEdit = useCan('patchMcpGateway');
  const base = useShell((s) => s.status?.data_plane_url ?? null);
  const { filters, set, clear } = useUrlFilters(GATEWAY_FILTERS);
  const [, setSelected] = useQueryParam('gateway');
  const q = filters.gq.trim().toLowerCase();
  const visible = gateways.filter(
    (g) => !q || g.name.includes(q) || g.spec.servers.some((s) => s.includes(q)),
  );

  if (gateways.length === 0)
    return (
      <EmptyState
        title="No MCP gateways yet"
        description="A gateway is the address clients connect to."
        action={
          <Gate operation="createMcpGateway" mode="hide">
            <EmptyAction onClick={onCreate}>Add gateway</EmptyAction>
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
            aria-label="Search gateways"
            icon="search"
            value={filters.gq}
            onChange={(e) => set({ gq: e.target.value })}
            placeholder="Search by gateway or server"
          />
        </div>
        <span className="ml-auto">
          <CountNote shown={visible.length} total={gateways.length} noun="gateways" singular="gateway" />
        </span>
      </FilterBar>
      <Panel flush>
        {visible.length === 0 ? (
          <NoMatch onClear={clear} />
        ) : (
          <Table label="MCP gateways">
            <thead>
              <tr>
                <Th>Gateway</Th>
                <Th className="max-sm:hidden">Servers</Th>
                <Th className="max-md:hidden">Sessions</Th>
                <Th className="max-lg:hidden">Features</Th>
                <Th>On</Th>
              </tr>
            </thead>
            <tbody>
              {visible.map((g) => {
                const stats = sessions?.find((x) => x.gateway === g.name);
                return (
                  <Tr
                    key={g.name}
                    interactive
                    tabIndex={0}
                    aria-label={`${g.name}: details`}
                    onClick={() => setSelected(g.name)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && e.target === e.currentTarget) setSelected(g.name);
                    }}
                  >
                    <Td>
                      <div className="mono text-body text-fg">{g.name}</div>
                      <div className="mono max-w-[360px] truncate text-caption text-muted">
                        {dataPlaneAddress(base, gatewayPaths(g.name)[0] ?? '')}
                      </div>
                    </Td>
                    <Td className="max-sm:hidden">
                      <span className="flex flex-wrap gap-1">
                        {g.spec.servers.map((s) => (
                          <Badge key={s} mono>
                            {s}
                          </Badge>
                        ))}
                      </span>
                    </Td>
                    <Td muted className="max-md:hidden">
                      {SESSION_MODE_LABEL[g.spec.session_mode ?? 'stateful']}
                      {stats ? <span className="mono tabular-nums"> · {stats.issued} issued</span> : null}
                    </Td>
                    <Td className="max-lg:hidden">
                      <span className="flex flex-wrap gap-1">
                        {g.spec.tool_search ? <Badge tone="info">Tool search</Badge> : null}
                        {g.spec.oauth ? <Badge>OAuth</Badge> : null}
                        {g.spec.on_server_failure === 'fail' ? <Badge>Fails as a whole</Badge> : null}
                        {!g.spec.tool_search && !g.spec.oauth && g.spec.on_server_failure !== 'fail' ? (
                          <span className="text-muted">—</span>
                        ) : null}
                      </span>
                    </Td>
                    <Td onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
                      <Switch
                        label={`Turn ${g.name} ${g.spec.enabled !== false ? 'off' : 'on'}`}
                        hideLabel
                        checked={g.spec.enabled !== false}
                        disabled={!canEdit}
                        disabledReason="You don't have permission to change MCP gateways."
                        onChange={(on) =>
                          void setEnabled(g.name, on).then(
                            () =>
                              notify(
                                'success',
                                on ? `${g.name} turned on` : `${g.name} turned off`,
                                undefined,
                                {
                                  label: 'Undo',
                                  onClick: () =>
                                    void setEnabled(g.name, !on).catch((e: unknown) =>
                                      notify('error', `Couldn't change ${g.name}`, describeError(e)),
                                    ),
                                },
                              ),
                            (e: unknown) => notify('error', `Couldn't change ${g.name}`, describeError(e)),
                          )
                        }
                      />
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        )}
      </Panel>
    </div>
  );
}
