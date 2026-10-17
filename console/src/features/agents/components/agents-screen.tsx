'use client';

import { useEffect, useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { FavouriteButton, StarredFilter, useStarredFilter } from '@/components/ui/favourite';
import { Input, Segmented, Switch } from '@/components/ui/form';
import { Forbidden, Gate, useCan, usePermission } from '@/components/ui/gate';
import { KeyboardHints } from '@/components/ui/keyboard-hints';
import { FilterBar, PageHeader, Panel, SummaryStrip } from '@/components/ui/layout';
import { UpdatedAt } from '@/components/ui/misc';
import {
  EmptyAction,
  EmptyState,
  ErrorState,
  isUnavailable,
  LoadingRows,
  NoMatch,
  UnavailableState,
} from '@/components/ui/states';
import { CountNote, ExportCsvButton, Table, Td, Th, Tr } from '@/components/ui/table';
import { type CsvColumn, exportCsv } from '@/lib/csv';
import { formatCount } from '@/lib/format';
import { usePolling } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';
import { useShell } from '@/stores/shell';
import { notify } from '@/stores/ui';

import { dataPlaneAddress } from '@/features/mcp/fields';

import type { AgentResource } from '../api';
import { PROTOCOL_LABEL } from '../form';
import { useAgents } from '../store';
import { AgentDrawer } from './agent-drawer';
import { describeError } from '@/lib/api/client';

const FILTERS = { q: '', state: 'all' };
type StateFilter = 'all' | 'on' | 'off' | 'public';
const STATES: StateFilter[] = ['all', 'on', 'off', 'public'];

const CSV: CsvColumn<AgentResource>[] = [
  { header: 'name', value: (a) => a.name },
  { header: 'path', value: (a) => `/a2a/${a.name}` },
  { header: 'card_url', value: (a) => a.spec.card_url },
  { header: 'url', value: (a) => a.spec.url ?? '' },
  { header: 'protocol', value: (a) => a.spec.protocol ?? 'auto' },
  { header: 'public_card', value: (a) => (a.spec.public_card ? 'true' : 'false') },
  { header: 'enabled', value: (a) => (a.spec.enabled === false ? 'false' : 'true') },
];

/** Agents: the A2A agents apps and other agents call through the gateway. */
export function AgentsScreen() {
  const canRead = usePermission('mcp:read');
  const agents = useAgents((s) => s.agents);
  const load = useAgents((s) => s.load);
  const setEnabled = useAgents((s) => s.setEnabled);
  const canEdit = useCan('patchA2aAgent');
  const base = useShell((s) => s.status?.data_plane_url ?? null);
  const { filters, set, setWith, active } = useUrlFilters(FILTERS);
  const starred = useStarredFilter('agent');
  const [selected, setSelected] = useQueryParam('agent');
  const [creating, setCreating] = useState(false);
  const state = (STATES.includes(filters.state as StateFilter) ? filters.state : 'all') as StateFilter;

  useEffect(() => {
    if (canRead) void load();
  }, [canRead, load]);
  usePolling(() => void load({ quiet: true }), canRead);
  usePageShortcut('n', () => setCreating(true));
  usePageShortcut('e', () => {
    if (agents.data?.length) exportCsv('agents', CSV, agents.data);
  });

  if (!canRead) return <Forbidden what="agents" />;

  const all = agents.data ?? [];
  const q = filters.q.trim().toLowerCase();
  const visible = starred.apply(
    all.filter((a) => {
      if (state === 'on' && a.spec.enabled === false) return false;
      if (state === 'off' && a.spec.enabled !== false) return false;
      if (state === 'public' && !a.spec.public_card) return false;
      return !q || a.name.includes(q) || a.spec.card_url.toLowerCase().includes(q);
    }),
    (a) => a.name,
  );
  const clear = () => setWith(FILTERS, { starred: null });
  const unavailable = isUnavailable(agents.error);
  const current = all.find((a) => a.name === selected) ?? null;

  return (
    <>
      <PageHeader
        title="Agents"
        description="A2A agents that apps and other agents call through the gateway"
        actions={
          <>
            <UpdatedAt at={agents.at} />
            <ExportCsvButton filename="agents" columns={CSV} rows={visible} />
            <Gate operation="createA2aAgent" mode="hide">
              <Button variant="primary" icon="plus" onClick={() => setCreating(true)}>
                Add agent
              </Button>
            </Gate>
          </>
        }
      />
      {unavailable ? (
        <UnavailableState subject="Agents" operation="GET /a2a/agents" />
      ) : (
        <>
          {all.length > 0 ? (
            <SummaryStrip
              label="Agents summary"
              items={[
                { label: 'Agents', value: formatCount(all.length) },
                {
                  label: 'On',
                  value: formatCount(all.filter((a) => a.spec.enabled !== false).length),
                  onClick: () => set({ state: 'on' }),
                },
                {
                  label: 'Off',
                  value: formatCount(all.filter((a) => a.spec.enabled === false).length),
                  onClick: () => set({ state: 'off' }),
                },
                {
                  label: 'Public cards',
                  value: formatCount(all.filter((a) => a.spec.public_card).length),
                  title: 'Agents whose card anyone may read; calls still need an API key',
                  onClick: () => set({ state: 'public' }),
                },
              ]}
            />
          ) : null}
          <div className="flex flex-col">
            {agents.status === 'loading' && !agents.data ? <LoadingRows rows={3} /> : null}
            {agents.error ? (
              <ErrorState error={agents.error} subject="Agents" onRetry={() => void load()} />
            ) : null}
            {agents.data && all.length === 0 ? (
              <EmptyState
                title="No agents yet"
                description="Add an A2A agent by the URL of its card."
                action={
                  <Gate operation="createA2aAgent" mode="hide">
                    <EmptyAction onClick={() => setCreating(true)}>Add agent</EmptyAction>
                  </Gate>
                }
              />
            ) : null}
            {all.length > 0 ? (
              <>
                <FilterBar>
                  <div className="w-64 max-sm:w-full">
                    <Input
                      data-page-search
                      aria-label="Search agents"
                      icon="search"
                      value={filters.q}
                      onChange={(e) => set({ q: e.target.value })}
                      placeholder="Search by name or card URL"
                    />
                  </div>
                  <Segmented<StateFilter>
                    label="Agent status"
                    value={state}
                    onChange={(v) => set({ state: v })}
                    options={[
                      { value: 'all', label: 'All' },
                      { value: 'on', label: 'On' },
                      { value: 'off', label: 'Off' },
                      { value: 'public', label: 'Public card' },
                    ]}
                  />
                  <StarredFilter kind="agent" pressed={starred.on} onChange={starred.set} />
                  {active || starred.on ? (
                    <button type="button" className="text-ui text-muted hover:text-fg" onClick={clear}>
                      Clear filters
                    </button>
                  ) : null}
                  <span className="ml-auto">
                    <CountNote shown={visible.length} total={all.length} noun="agents" singular="agent" />
                  </span>
                </FilterBar>
                <Panel flush>
                  {visible.length === 0 ? (
                    <NoMatch onClear={clear} />
                  ) : (
                    <Table label="Agents">
                      <thead>
                        <tr>
                          <Th>Agent</Th>
                          <Th className="max-md:hidden">Card</Th>
                          <Th className="max-sm:hidden">Protocol</Th>
                          <Th>On</Th>
                        </tr>
                      </thead>
                      <tbody>
                        {visible.map((a) => (
                          <Tr
                            key={a.name}
                            interactive
                            tabIndex={0}
                            aria-label={`${a.name}: details`}
                            onClick={() => setSelected(a.name)}
                            onKeyDown={(e) => {
                              if (e.key === 'Enter' && e.target === e.currentTarget) setSelected(a.name);
                            }}
                          >
                            <Td>
                              <div className="flex items-center gap-2">
                                <span
                                  onClick={(e) => e.stopPropagation()}
                                  onKeyDown={(e) => e.stopPropagation()}
                                >
                                  <FavouriteButton kind="agent" name={a.name} label={`agent ${a.name}`} />
                                </span>
                                <div className="min-w-0">
                                  <div className="mono text-body text-fg">{a.name}</div>
                                  <div className="mono max-w-[360px] truncate text-caption text-muted">
                                    {dataPlaneAddress(base, `/a2a/${a.name}`)}
                                  </div>
                                </div>
                              </div>
                            </Td>
                            <Td className="max-md:hidden">
                              <span className="flex items-center gap-2">
                                <span className="mono max-w-[340px] truncate text-ui text-muted">
                                  {a.spec.card_url}
                                </span>
                                {a.spec.public_card ? <Badge tone="info">Public</Badge> : null}
                              </span>
                            </Td>
                            <Td muted className="max-sm:hidden">
                              <span className="flex items-center gap-1.5">
                                <Dot tone={a.spec.enabled === false ? 'neutral' : 'success'} />
                                {PROTOCOL_LABEL[a.spec.protocol ?? 'auto']}
                              </span>
                            </Td>
                            <Td onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
                              <Switch
                                label={`Turn ${a.name} ${a.spec.enabled !== false ? 'off' : 'on'}`}
                                hideLabel
                                checked={a.spec.enabled !== false}
                                disabled={!canEdit}
                                disabledReason="You don't have permission to change agents."
                                onChange={(on) =>
                                  void setEnabled(a.name, on).then(
                                    () =>
                                      notify(
                                        'success',
                                        on ? `${a.name} turned on` : `${a.name} turned off`,
                                        undefined,
                                        {
                                          label: 'Undo',
                                          onClick: () =>
                                            void setEnabled(a.name, !on).catch((e: unknown) =>
                                              notify('error', `Couldn't change ${a.name}`, describeError(e)),
                                            ),
                                        },
                                      ),
                                    (e: unknown) =>
                                      notify('error', `Couldn't change ${a.name}`, describeError(e)),
                                  )
                                }
                              />
                            </Td>
                          </Tr>
                        ))}
                      </tbody>
                    </Table>
                  )}
                </Panel>
                <KeyboardHints keys={['search', 'open', 'close', 'export', 'new']} />
              </>
            ) : null}
          </div>
        </>
      )}
      <AgentDrawer
        key={creating ? '+new' : (current?.name ?? '')}
        agent={creating ? null : current}
        creating={creating}
        onClose={() => {
          setCreating(false);
          setSelected(null);
        }}
        onCreated={(name) => {
          setCreating(false);
          setSelected(name);
        }}
      />
    </>
  );
}
