'use client';

import { useEffect, useMemo, useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { FavouriteButton, StarredFilter, useStarredFilter } from '@/components/ui/favourite';
import { useSwitchChanges } from '@/components/ui/confirm';
import { Input, Segmented, Select, Switch } from '@/components/ui/form';
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
import { formatCount, formatLatency, formatPercent } from '@/lib/format';
import { usePolling } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';

import { attachmentName, type PluginResource, type PluginRuntime } from '../api';
import { failureRate, HOOK_LABEL, type PluginType, TYPE_LABEL } from '../form';
import { usePlugins } from '../store';
import { PluginDrawer } from './plugin-drawer';

const FILTERS = { q: '', type: '', state: 'all' };
type StateFilter = 'all' | 'attached' | 'failing' | 'off';
const STATES: StateFilter[] = ['all', 'attached', 'failing', 'off'];

interface Row {
  plugin: PluginResource;
  runtime: PluginRuntime | undefined;
  attached: boolean;
}

const CSV: CsvColumn<Row>[] = [
  { header: 'name', value: (r) => r.plugin.name },
  { header: 'type', value: (r) => r.plugin.spec.type },
  { header: 'hooks', value: (r) => (r.plugin.spec.hooks ?? []).join(' ') },
  { header: 'order', value: (r) => r.plugin.spec.order ?? 100 },
  { header: 'on_error', value: (r) => r.plugin.spec.on_error ?? 'fail_closed' },
  { header: 'timeout', value: (r) => r.plugin.spec.timeout ?? '500ms' },
  { header: 'enabled', value: (r) => (r.plugin.spec.enabled === false ? 'false' : 'true') },
  { header: 'attached_gateway', value: (r) => (r.attached ? 'true' : 'false') },
  { header: 'calls', value: (r) => r.runtime?.calls ?? 0 },
  { header: 'failures', value: (r) => r.runtime?.failures ?? 0 },
  { header: 'timeouts', value: (r) => r.runtime?.timeouts ?? 0 },
  { header: 'p50_ms', value: (r) => r.runtime?.p50_ms ?? '' },
  { header: 'p99_ms', value: (r) => r.runtime?.p99_ms ?? '' },
  { header: 'last_error', value: (r) => r.runtime?.last_error ?? '' },
];

/** Plugins: the code the gateway runs on requests, with how often it runs and fails. */
export function PluginsScreen() {
  const canRead = usePermission('plugins:read');
  const plugins = usePlugins((s) => s.plugins);
  const runtime = usePlugins((s) => s.runtime.data);
  const policies = usePlugins((s) => s.policies.data);
  const load = usePlugins((s) => s.load);
  const setEnabled = usePlugins((s) => s.setEnabled);
  const setAttached = usePlugins((s) => s.setAttached);
  const canEdit = useCan('patchPlugin');
  const canAttach = useCan('patchPoliciesSettings');
  const { filters, set, setWith, active } = useUrlFilters(FILTERS);
  const starred = useStarredFilter('plugin');
  const [selected, setSelected] = useQueryParam('plugin');
  const [creating, setCreating] = useState(false);
  const state = (STATES.includes(filters.state as StateFilter) ? filters.state : 'all') as StateFilter;
  const switches = useSwitchChanges();
  /** Asks first when a change leaves the plugin on and attached: it then runs on every request. */
  const onEveryRequest = (plugin: PluginResource) => ({
    title: `Run ${plugin.name} on every request?`,
    confirmLabel: 'Run on every request',
    effect:
      plugin.spec.on_error === 'fail_open'
        ? `Every request will wait on ${plugin.name}; if it fails, requests go on without it.`
        : `Every request will wait on ${plugin.name}; if it fails or times out, requests are refused.`,
  });

  useEffect(() => {
    if (canRead) void load();
  }, [canRead, load]);
  usePolling(() => void load({ quiet: true }), canRead);

  const rows: Row[] = useMemo(() => {
    const attach = new Set((policies?.spec.plugins?.attach ?? []).map(attachmentName));
    return (plugins.data ?? []).map((plugin) => ({
      plugin,
      runtime: runtime?.find((r) => r.name === plugin.name),
      attached: attach.has(plugin.name),
    }));
  }, [plugins.data, runtime, policies]);

  usePageShortcut('n', () => setCreating(true));
  usePageShortcut('e', () => {
    if (rows.length > 0) exportCsv('plugins', CSV, rows);
  });

  if (!canRead) return <Forbidden what="plugins" />;

  const q = filters.q.trim().toLowerCase();
  const visible = starred.apply(
    rows.filter((r) => {
      if (filters.type && r.plugin.spec.type !== filters.type) return false;
      if (state === 'attached' && !r.attached) return false;
      if (state === 'off' && r.plugin.spec.enabled !== false) return false;
      if (state === 'failing' && !(r.runtime?.failures ?? 0)) return false;
      return !q || r.plugin.name.toLowerCase().includes(q);
    }),
    (r) => r.plugin.name,
  );
  const clear = () => setWith(FILTERS, { starred: null });
  const unavailable = isUnavailable(plugins.error);
  const current = plugins.data?.find((p) => p.name === selected) ?? null;
  const calls = rows.reduce((a, r) => a + (r.runtime?.calls ?? 0), 0);
  const failures = rows.reduce((a, r) => a + (r.runtime?.failures ?? 0), 0);
  const slowest = rows
    .filter((r) => r.runtime?.p99_ms !== null && r.runtime?.p99_ms !== undefined)
    .sort((a, b) => (b.runtime?.p99_ms ?? 0) - (a.runtime?.p99_ms ?? 0))[0];

  return (
    <>
      <PageHeader
        title="Plugins"
        description="Code the gateway runs on requests: webhooks you host, built-ins and WebAssembly modules"
        actions={
          <>
            <UpdatedAt at={plugins.at} />
            <ExportCsvButton filename="plugins" columns={CSV} rows={visible} />
            <Gate operation="createPlugin" mode="hide">
              <Button variant="primary" icon="plus" onClick={() => setCreating(true)}>
                Add plugin
              </Button>
            </Gate>
          </>
        }
      />
      {unavailable ? (
        <UnavailableState subject="Plugins" operation="GET /plugins" />
      ) : (
        <>
          {rows.length > 0 ? (
            <SummaryStrip
              label="Plugins summary"
              items={[
                {
                  label: 'Plugins',
                  value: formatCount(rows.length),
                  sub: `${rows.filter((r) => r.attached).length} on every request`,
                  onClick: () => set({ state: 'attached' }),
                },
                { label: 'Calls', value: formatCount(calls), sub: 'this node, since it started' },
                {
                  label: 'Failures',
                  value: formatCount(failures),
                  tone: failures > 0 ? 'error' : 'neutral',
                  sub: calls > 0 ? `${formatPercent(failures / calls)} of calls` : undefined,
                  onClick: failures > 0 ? () => set({ state: 'failing' }) : undefined,
                },
                {
                  label: 'Slowest, p99',
                  value: slowest ? formatLatency(slowest.runtime?.p99_ms) : '—',
                  sub: slowest?.plugin.name,
                  onClick: slowest ? () => setSelected(slowest.plugin.name) : undefined,
                },
              ]}
            />
          ) : null}
          <div className="flex flex-col">
            {plugins.status === 'loading' && !plugins.data ? <LoadingRows rows={3} /> : null}
            {plugins.error ? (
              <ErrorState error={plugins.error} subject="Plugins" onRetry={() => void load()} />
            ) : null}
            {plugins.data && rows.length === 0 ? (
              <EmptyState
                title="No plugins yet"
                description="A plugin checks or changes requests and answers."
                action={
                  <Gate operation="createPlugin" mode="hide">
                    <EmptyAction onClick={() => setCreating(true)}>Add plugin</EmptyAction>
                  </Gate>
                }
              />
            ) : null}
            {rows.length > 0 ? (
              <>
                <FilterBar>
                  <div className="w-64 max-sm:w-full">
                    <Input
                      data-page-search
                      aria-label="Search plugins"
                      icon="search"
                      value={filters.q}
                      onChange={(e) => set({ q: e.target.value })}
                      placeholder="Search by name"
                    />
                  </div>
                  <Segmented<StateFilter>
                    label="Plugin status"
                    value={state}
                    onChange={(v) => set({ state: v })}
                    options={[
                      { value: 'all', label: 'All' },
                      { value: 'attached', label: 'On every request' },
                      { value: 'failing', label: 'Failing' },
                      { value: 'off', label: 'Off' },
                    ]}
                  />
                  <div className="w-44 max-sm:flex-1">
                    <Select
                      aria-label="Type"
                      value={filters.type}
                      onChange={(v) => set({ type: v })}
                      emptyOption="Any type"
                      options={(Object.keys(TYPE_LABEL) as PluginType[]).map((t) => ({
                        value: t,
                        label: TYPE_LABEL[t],
                      }))}
                    />
                  </div>
                  <StarredFilter kind="plugin" pressed={starred.on} onChange={starred.set} />
                  {active || starred.on ? (
                    <button type="button" className="text-ui text-muted hover:text-fg" onClick={clear}>
                      Clear filters
                    </button>
                  ) : null}
                  <span className="ml-auto">
                    <CountNote shown={visible.length} total={rows.length} noun="plugins" singular="plugin" />
                  </span>
                </FilterBar>
                <Panel flush>
                  {visible.length === 0 ? (
                    <NoMatch onClear={clear} />
                  ) : (
                    <Table label="Plugins">
                      <thead>
                        <tr>
                          <Th>Plugin</Th>
                          <Th numeric>Calls</Th>
                          <Th numeric className="max-sm:hidden">
                            Failures
                          </Th>
                          <Th numeric className="max-md:hidden">
                            p50 · p99
                          </Th>
                          <Th className="max-lg:hidden">Last error</Th>
                          <Th className="max-sm:hidden">Every request</Th>
                          <Th>On</Th>
                        </tr>
                      </thead>
                      <tbody>
                        {visible.map(({ plugin, runtime: rt, attached }) => {
                          const rate = failureRate(rt);
                          return (
                            <Tr
                              key={plugin.name}
                              interactive
                              tabIndex={0}
                              aria-label={`${plugin.name}: details`}
                              onClick={() => setSelected(plugin.name)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter' && e.target === e.currentTarget)
                                  setSelected(plugin.name);
                              }}
                            >
                              <Td>
                                <div className="flex items-center gap-2">
                                  <span
                                    onClick={(e) => e.stopPropagation()}
                                    onKeyDown={(e) => e.stopPropagation()}
                                  >
                                    <FavouriteButton
                                      kind="plugin"
                                      name={plugin.name}
                                      label={`plugin ${plugin.name}`}
                                    />
                                  </span>
                                  <div className="min-w-0">
                                    <div className="mono text-body text-fg">{plugin.name}</div>
                                    <div className="truncate text-caption text-muted">
                                      {TYPE_LABEL[plugin.spec.type]} ·{' '}
                                      {(plugin.spec.hooks ?? [])
                                        .map((h) => HOOK_LABEL[h].toLowerCase())
                                        .join(', ')}
                                    </div>
                                  </div>
                                </div>
                              </Td>
                              <Td numeric>
                                {rt && rt.calls > 0 ? (
                                  formatCount(rt.calls)
                                ) : (
                                  <span className="font-sans text-muted">No calls yet</span>
                                )}
                              </Td>
                              <Td numeric className="max-sm:hidden">
                                {rt && rt.calls > 0 ? (
                                  <span className={rt.failures > 0 ? 'text-error' : undefined}>
                                    {formatCount(rt.failures)}
                                    {rate !== null && rate > 0 ? ` · ${formatPercent(rate)}` : ''}
                                  </span>
                                ) : (
                                  '—'
                                )}
                              </Td>
                              <Td numeric className="max-md:hidden">
                                {rt?.p50_ms != null
                                  ? `${formatLatency(rt.p50_ms)} · ${formatLatency(rt.p99_ms)}`
                                  : '—'}
                              </Td>
                              <Td muted className="max-lg:hidden">
                                {rt?.last_error ? (
                                  <span className="flex items-center gap-1.5">
                                    <Dot tone="error" />
                                    <span className="max-w-[200px] truncate">{rt.last_error}</span>
                                  </span>
                                ) : (
                                  '—'
                                )}
                              </Td>
                              <Td
                                className="max-sm:hidden"
                                onClick={(e) => e.stopPropagation()}
                                onKeyDown={(e) => e.stopPropagation()}
                              >
                                <Switch
                                  label={`Run ${plugin.name} on every request`}
                                  hideLabel
                                  checked={attached}
                                  disabled={!canAttach}
                                  disabledReason="You don't have permission to change policies."
                                  onChange={(on) =>
                                    switches.change({
                                      name: plugin.name,
                                      apply: () => setAttached(plugin.name, on),
                                      undo: () => setAttached(plugin.name, !on),
                                      done: on
                                        ? `${plugin.name} runs on every request`
                                        : `${plugin.name} detached`,
                                      undone: on
                                        ? `${plugin.name} detached`
                                        : `${plugin.name} runs on every request`,
                                      ask:
                                        on && plugin.spec.enabled !== false
                                          ? onEveryRequest(plugin)
                                          : undefined,
                                    })
                                  }
                                />
                              </Td>
                              <Td onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
                                <span className="flex items-center gap-2">
                                  <Switch
                                    label={`Turn ${plugin.name} ${plugin.spec.enabled !== false ? 'off' : 'on'}`}
                                    hideLabel
                                    checked={plugin.spec.enabled !== false}
                                    disabled={!canEdit}
                                    disabledReason="You don't have permission to change plugins."
                                    onChange={(on) =>
                                      switches.change({
                                        name: plugin.name,
                                        apply: () => setEnabled(plugin.name, on),
                                        undo: () => setEnabled(plugin.name, !on),
                                        done: on ? `${plugin.name} turned on` : `${plugin.name} turned off`,
                                        undone: on ? `${plugin.name} turned off` : `${plugin.name} turned on`,
                                        ask: on && attached ? onEveryRequest(plugin) : undefined,
                                      })
                                    }
                                  />
                                  {plugin.spec.on_error === 'fail_open' ? <Badge>Fails open</Badge> : null}
                                </span>
                              </Td>
                            </Tr>
                          );
                        })}
                      </tbody>
                    </Table>
                  )}
                </Panel>
                <p className="px-gutter py-3 text-caption text-muted max-sm:px-gutter-sm">
                  Calls, failures and times are the counts of the node that answered, since it started.
                </p>
                <KeyboardHints keys={['search', 'open', 'close', 'export', 'new']} />
              </>
            ) : null}
          </div>
        </>
      )}
      {switches.dialog}
      <PluginDrawer
        key={creating ? '+new' : (current?.name ?? '')}
        plugin={creating ? null : current}
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
