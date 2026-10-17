'use client';

import { usePathname, useRouter } from 'next/navigation';
import { useEffect, useMemo, useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Sparkline } from '@/components/ui/charts';
import { ChipToggle, Combobox, Input, MultiSelect, Select } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { FilterBar, PageHeader, Tabs } from '@/components/ui/layout';
import { StarredFilter, useFavouriteNames, useStarredFilter } from '@/components/ui/favourite';
import { Icon } from '@/components/ui/icon';
import { MarkTile, ModelMark, ProviderMark } from '@/components/ui/marks';
import { KeyboardHints } from '@/components/ui/keyboard-hints';
import { MasterDetail, SelectableList } from '@/components/ui/list-detail';
import { UpdatedAt } from '@/components/ui/misc';
import { EmptyAction, EmptyState, ErrorState, LoadingRows, NoMatch } from '@/components/ui/states';
import { CountNote, Truncate } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import type { EffectivePrice } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount, formatMoney, plural, type TokenUnit } from '@/lib/format';
import { useTokenPrice } from '@/lib/hooks';
import { formatUnitAmount } from '@/lib/price';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';

import { PlaygroundLink } from '@/features/playground/components/playground-link';
import { AddProviderDrawer } from '@/features/providers/components/add-provider-drawer';

import { formatContext, MODALITY_LABEL } from '../catalog';
import {
  filterModels,
  hasState,
  measuresOf,
  MODEL_FILTER_DEFAULTS,
  type ModelMeasures,
  type StateFilter,
  toModelFilters,
  useModelsScreenData,
  useModelViews,
  useProviderMap,
} from '../hooks';
import { exportPricing, useCatalog, useModels } from '../store';
import {
  type Modality,
  type ModelView,
  rate,
  statusOf,
  STATUS_LABEL,
  STATUS_TONE,
  tierLabel,
} from '../types';
import { ActivityTab } from './activity-tab';
import { AddModelDrawer } from './add-model-drawer';
import { AliasesTab } from './aliases-tab';
import { EditModelDrawer } from './edit-model-drawer';
import { ModelDetail } from './model-detail';
import { ModelsHero } from './models-hero';
import { PolicyTab } from './policy-tab';
import { RoutingTab } from './routing-tab';

/** The lowest rate of a field across a Model's leaves, per 1M tokens, or null. */
export function lowestRate(prices: (EffectivePrice | null | undefined)[], field: string): number | null {
  const values = prices
    .map((p) => rate(p?.price, field))
    .filter((v): v is string => v !== null)
    .map(Number)
    .filter((n) => Number.isFinite(n) && n >= 0);
  return values.length > 0 ? Math.min(...values) : null;
}

/** "$2.50 / $10.00 per 1M" (input and output), "$0.02 per 1M" (embeddings), "$0.002 a request" (rerank). */
export function rowPrice(
  prices: (EffectivePrice | null | undefined)[],
  modality: Modality,
  unit: TokenUnit,
): string | null {
  const scale = (n: number) => formatUnitAmount(n, unit);
  const from = prices.filter(Boolean).length > 1 ? 'from ' : '';
  if (modality === 'rerank') {
    const per = lowestRate(prices, 'per_request');
    return per !== null ? `${from}${formatMoney(per)} a request` : null;
  }
  const input = lowestRate(prices, 'input');
  if (input === null) return null;
  const output = modality === 'embeddings' ? null : lowestRate(prices, 'output');
  return `${from}${scale(input)}${output !== null ? ` / ${scale(output)}` : ''} per ${unit}`;
}

function ModelRow({ view, starred, defaults }: { view: ModelView; starred: boolean; defaults: string[] }) {
  const usage = useCatalog((s) => s.usage.data?.get(view.name));
  const prices = useCatalog((s) => s.prices);
  const waiting = useCatalog((s) => s.access[view.name]?.data?.filter((r) => r.state === 'open').length ?? 0);
  const tokenPrice = useTokenPrice();
  const status = statusOf(view);
  const price = rowPrice(
    view.leaves.map((l) => prices[l]),
    view.modality,
    tokenPrice.unit,
  );
  const facts = [
    view.name,
    MODALITY_LABEL[view.modality],
    view.context !== null ? `${formatContext(view.context)} ctx` : null,
  ].filter(Boolean);
  return (
    <div className="flex items-start gap-3">
      <MarkTile>
        <ModelMark
          model={view.name}
          ownedBy={view.model.spec.owned_by}
          family={view.model.spec.catalog?.family}
          upstream={view.upstream}
          size="lg"
        />
        <span
          className={cn(
            'absolute -right-0.5 -bottom-0.5 h-2.5 w-2.5 rounded-full border-2 border-raised',
            view.enabled ? 'bg-success' : 'bg-error',
          )}
        />
      </MarkTile>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-body font-semibold text-fg">{view.displayName}</span>
          {starred ? (
            <span className="text-star" aria-label="Starred">
              <Icon name="starFilled" size={12} />
            </span>
          ) : null}
          {view.aliases.slice(0, 1).map((a) => (
            <Badge key={a} tone="accent">
              @{a}
            </Badge>
          ))}
          {defaults.length > 0 ? (
            <Tooltip content={`Used when a request names no model: ${defaults.join(', ')}`}>
              <span>
                <Badge tone="success">Default</Badge>
              </span>
            </Tooltip>
          ) : null}
        </div>
        <div className="mono mt-0.5 text-caption text-muted">
          <Truncate text={facts.join(' · ')} />
        </div>
        {price ? (
          <div className="mono text-caption text-muted">
            <Truncate text={price} />
          </div>
        ) : null}
      </div>
      <div className="flex shrink-0 flex-col items-end gap-1.5">
        <span className="flex items-center gap-1.5">
          {waiting > 0 ? (
            <Tooltip content={`${plural(waiting, 'request')} for access waiting for a decision`}>
              <span>
                <Badge tone="warn">{waiting} waiting</Badge>
              </span>
            </Tooltip>
          ) : null}
          {status !== 'available' ? <Badge tone={STATUS_TONE[status]}>{STATUS_LABEL[status]}</Badge> : null}
          {view.tier ? <Badge>{tierLabel(view.tier)}</Badge> : null}
        </span>
        {usage && usage.daily.length > 1 ? (
          <Sparkline values={usage.daily} label={`${view.name} daily requests`} className="w-24" />
        ) : null}
        {usage ? (
          <span className="mono text-caption text-dim tabular-nums">
            {formatCount(usage.requests, { compact: true })} req
          </span>
        ) : null}
      </div>
    </div>
  );
}

/** The endpoint ids a Model is the default for, in words ("chat completions"). */
export function endpointLabel(id: string): string {
  return id.replace(/_/g, ' ');
}

type Tab = 'models' | 'aliases' | 'routing' | 'policy' | 'activity';
const TABS: Tab[] = ['models', 'aliases', 'routing', 'policy', 'activity'];

const STATE_CHIPS: {
  value: StateFilter;
  label: string;
  tone: 'success' | 'error' | 'info' | 'warn' | 'muted';
}[] = [
  { value: 'enabled', label: 'On', tone: 'success' },
  { value: 'preview', label: 'Preview', tone: 'info' },
  { value: 'restricted', label: 'Restricted', tone: 'warn' },
  { value: 'deprecated', label: 'Deprecated', tone: 'muted' },
  { value: 'disabled', label: 'Off', tone: 'error' },
];

/** The model catalog: key figures, the list with filters, the selected model, aliases, routing, policy and activity. */
export function ModelsScreen() {
  useModelsScreenData();
  const status = useModels((s) => s.status);
  const error = useModels((s) => s.error);
  const loadedAt = useModels((s) => s.loadedAt);
  const fetch = useModels((s) => s.fetch);
  const views = useModelViews();
  const providers = useProviderMap();
  const providersSlot = useCatalog((s) => s.providers);
  const loadProviders = useCatalog((s) => s.loadProviders);
  const usage = useCatalog((s) => s.usage);
  const prices = useCatalog((s) => s.prices);
  const routing = useCatalog((s) => s.routing.data);
  const router = useRouter();
  const pathname = usePathname();
  const [tabParam, setTabParam] = useQueryParam('tab');
  const tab: Tab = TABS.includes(tabParam as Tab) ? (tabParam as Tab) : 'models';
  const url = useUrlFilters(MODEL_FILTER_DEFAULTS);
  const filters = toModelFilters(url.filters);
  const [selected, setSelected] = useQueryParam('model');
  const [drawer, setDrawer] = useState<'add' | 'edit' | 'lifecycle' | 'provider' | null>(null);

  const measures = useMemo(() => {
    if (!usage.data) return null;
    const map = new Map<string, ModelMeasures>();
    for (const [name, u] of usage.data) {
      const m = measuresOf(u);
      if (m) map.set(name, m);
    }
    return map;
  }, [usage.data]);
  const filteredModels = useMemo(() => {
    const outputPrice = (v: ModelView) =>
      lowestRate(
        v.leaves.map((l) => prices[l]),
        'output',
      );
    return filterModels(views, filters, measures, outputPrice);
    // `filters` is rebuilt from the URL on every render; its parts are the dependencies.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [views, url.filters, measures, prices]);
  const starred = useStarredFilter('model');
  const starredNames = useFavouriteNames('model');
  const visible = starred.apply(filteredModels, (v) => v.name);
  const linked = views.find((v) => v.name === selected);
  // A Model the filters hide does not stay open beside "No results": the detail follows the list.
  const current = (linked && (!url.active || visible.includes(linked)) ? linked : undefined) ?? visible[0];
  const providerNames = [...new Set(views.flatMap((v) => v.providers))].sort();
  const providerType = (name: string | undefined) => (name ? providers.get(name)?.spec.type : undefined);
  const count = (pred: (v: ModelView) => boolean) => views.filter(pred).length;
  const noProviders = providersSlot.data !== null && providers.size === 0;
  const labels = [...new Set(views.flatMap((v) => v.model.spec.labels ?? []))].sort();
  const modalities = [...new Set(views.map((v) => v.modality))];
  const defaultsOf = (name: string) =>
    Object.entries(routing?.spec.default_models ?? {})
      .filter(([, m]) => m === name)
      .map(([endpoint]) => endpointLabel(endpoint));
  const setTab = (t: Tab) => setTabParam(t === 'models' ? null : t);
  /** Changes several query parameters in one navigation (two in a row would race). */
  const replaceParams = (patch: Record<string, string | null>) => {
    const search = new URLSearchParams(window.location.search);
    for (const [k, v] of Object.entries(patch)) {
      if (v === null || v === '') search.delete(k);
      else search.set(k, v);
    }
    const query = search.toString();
    router.replace(query ? `${pathname}?${query}` : pathname, { scroll: false });
  };
  const openModel = (name: string) => replaceParams({ tab: null, model: name });

  const canAccess = useCan('listAccessRequests');
  const loadAccess = useCatalog((s) => s.loadAccess);
  const gated = views
    .filter((v) => v.lifecycle === 'preview' || v.lifecycle === 'restricted')
    .map((v) => v.name)
    .join(',');
  useEffect(() => {
    if (!canAccess || !gated) return;
    for (const name of gated.split(',')) void loadAccess(name);
  }, [canAccess, gated, loadAccess]);

  usePageShortcut('n', () => setDrawer('add'));
  usePageShortcut('e', () => void exportPricing(views));

  const addModel = (
    <Gate operation="createModel" mode="hide">
      <Button variant="primary" icon="plus" onClick={() => setDrawer('add')}>
        Add model
      </Button>
    </Gate>
  );
  return (
    <>
      <PageHeader
        title="Models"
        description="The model names your apps call, and the providers behind them"
        actions={
          <div className="flex flex-wrap items-center gap-2 max-sm:min-w-[calc(100vw-32px)]">
            <UpdatedAt at={loadedAt} />
            {views.length > 0 ? (
              <Button icon="download" variant="ghost" onClick={() => void exportPricing(views)}>
                Export prices
              </Button>
            ) : null}
            {views.length > 0 ? <PlaygroundLink variant="ghost" /> : null}
            {addModel}
          </div>
        }
      />
      {views.length > 0 ? (
        <ModelsHero
          views={views}
          providerType={providerType}
          provider={filters.provider}
          onProvider={(p) => replaceParams({ provider: filters.provider === p ? null : p, tab: null })}
        />
      ) : null}
      {views.length > 0 ? (
        <Tabs<Tab>
          label="Models views"
          value={tab}
          onChange={setTab}
          items={[
            { value: 'models', label: 'Models', count: views.length, icon: 'cube' },
            {
              value: 'aliases',
              label: 'Aliases',
              count: views.reduce((a, v) => a + v.aliases.length, 0),
              icon: 'link',
            },
            { value: 'routing', label: 'Routing', icon: 'route' },
            { value: 'policy', label: 'Policy & headroom', icon: 'sliders' },
            { value: 'activity', label: 'Activity', icon: 'history' },
          ]}
        />
      ) : null}
      {tab === 'aliases' && views.length > 0 ? <AliasesTab views={views} onOpenModel={openModel} /> : null}
      {tab === 'routing' && views.length > 0 ? <RoutingTab views={views} onOpenModel={openModel} /> : null}
      {tab === 'policy' && views.length > 0 ? <PolicyTab views={views} /> : null}
      {tab === 'activity' && views.length > 0 ? <ActivityTab onOpenModel={openModel} /> : null}
      {tab === 'models' || views.length === 0 ? (
        <>
          {views.length > 0 ? (
            <FilterBar>
              <Input
                data-page-search
                aria-label="Search models"
                icon="search"
                value={filters.query}
                onChange={(e) => url.set({ q: e.target.value })}
                placeholder="Search models"
                className="h-7 w-44"
              />
              <div role="group" aria-label="Status" className="flex items-center">
                <div className="flex flex-wrap gap-1.5">
                  {STATE_CHIPS.map((s) => (
                    <ChipToggle
                      key={s.value}
                      tone={s.tone}
                      pressed={filters.states.includes(s.value)}
                      count={count((v) => hasState(v, s.value))}
                      onChange={(on) =>
                        url.set({
                          status: on
                            ? [...filters.states, s.value]
                            : filters.states.filter((x) => x !== s.value),
                        })
                      }
                    >
                      {s.label}
                    </ChipToggle>
                  ))}
                </div>
              </div>
              <div className="ml-auto flex flex-wrap items-center gap-2">
                {providerNames.length > 1 ? (
                  <div className="w-40 shrink-0">
                    <Combobox
                      aria-label="Provider"
                      value={filters.provider}
                      onChange={(v) => url.set({ provider: v })}
                      emptyOption="All providers"
                      options={providerNames.map((p) => ({
                        value: p,
                        label: `${p} (${count((v) => v.providers.includes(p))})`,
                        icon: <ProviderMark type={providerType(p)} size="md" />,
                      }))}
                      className="h-7"
                    />
                  </div>
                ) : null}
                <div className="w-32 shrink-0">
                  <Select
                    aria-label="Tier"
                    value={filters.tier}
                    onChange={(v) => url.set({ tier: v })}
                    emptyOption="All tiers"
                    options={[
                      ...new Set(views.map((v) => v.tier).filter((t): t is string => Boolean(t))),
                    ].map((t) => ({ value: t, label: `${tierLabel(t)} (${count((v) => v.tier === t)})` }))}
                    className="h-7"
                  />
                </div>
                {modalities.length > 1 || filters.modality ? (
                  <div className="w-36 shrink-0">
                    <Select
                      aria-label="Modality"
                      value={filters.modality}
                      onChange={(v) => url.set({ modality: v })}
                      emptyOption="All modalities"
                      options={(Object.keys(MODALITY_LABEL) as Modality[])
                        .filter((m) => modalities.includes(m))
                        .map((m) => ({
                          value: m,
                          label: `${MODALITY_LABEL[m]} (${count((v) => v.modality === m)})`,
                        }))}
                      className="h-7"
                    />
                  </div>
                ) : null}
                {labels.length > 0 ? (
                  <div className="w-40 shrink-0">
                    <MultiSelect
                      aria-label="Labels"
                      noun="label"
                      placeholder="Labels"
                      value={filters.labels}
                      onChange={(v) => url.set({ label: v })}
                      options={labels.map((l) => ({
                        value: l,
                        label: `${l} (${count((v) => (v.model.spec.labels ?? []).includes(l))})`,
                      }))}
                      className="h-7"
                    />
                  </div>
                ) : null}
                <div className="w-44 shrink-0">
                  <Select
                    aria-label="Sort"
                    value={filters.sort}
                    onChange={(v) => url.set({ sort: v })}
                    options={[
                      { value: 'name', label: 'Sort by name' },
                      { value: 'requests', label: 'Most requests' },
                      { value: 'spend', label: 'Most spend' },
                      { value: 'p95', label: 'Lowest p95 latency' },
                      { value: 'output_price', label: 'Lowest output price' },
                    ]}
                    className="h-7"
                  />
                </div>
                <StarredFilter kind="model" pressed={starred.on} onChange={starred.set} />
                <span className="whitespace-nowrap">
                  <CountNote shown={visible.length} total={views.length} noun="models" />
                </span>
              </div>
            </FilterBar>
          ) : null}
          {status === 'loading' && views.length === 0 ? <LoadingRows /> : null}
          {status === 'error' ? (
            <ErrorState error={error} subject="Models" onRetry={() => void fetch()} />
          ) : null}
          {status === 'ready' && views.length === 0 ? (
            <EmptyState
              title="No models yet"
              description={
                noProviders
                  ? 'Connect a provider first, then add the models your apps send.'
                  : 'Add a model from your providers so apps can send its name.'
              }
              action={
                noProviders ? (
                  <Gate operation="createProvider" mode="hide">
                    <EmptyAction onClick={() => setDrawer('provider')}>Add provider</EmptyAction>
                  </Gate>
                ) : (
                  <Gate operation="createModel" mode="hide">
                    <EmptyAction onClick={() => setDrawer('add')}>Add model</EmptyAction>
                  </Gate>
                )
              }
            />
          ) : null}
          {views.length > 0 ? (
            <MasterDetail
              className="max-lg:grid-cols-1"
              list={
                visible.length === 0 ? (
                  <NoMatch
                    onClear={() => {
                      url.clear();
                      starred.set(false);
                    }}
                  />
                ) : (
                  <SelectableList
                    label="Models"
                    items={visible}
                    keyOf={(v) => v.name}
                    selectedKey={current?.name ?? null}
                    onSelect={(name) => setSelected(name)}
                    toneOf={(v) => STATUS_TONE[statusOf(v)]}
                    renderItem={(v) => (
                      <ModelRow view={v} starred={starredNames.has(v.name)} defaults={defaultsOf(v.name)} />
                    )}
                  />
                )
              }
              detail={
                current ? (
                  <ModelDetail
                    key={current.name}
                    view={current}
                    defaults={defaultsOf(current.name)}
                    onEdit={(focus) => setDrawer(focus === 'lifecycle' ? 'lifecycle' : 'edit')}
                    onOpenModel={openModel}
                  />
                ) : (
                  <EmptyState title="Select a model to see its details" />
                )
              }
            />
          ) : null}
          {views.length > 0 ? <KeyboardHints keys={['search', 'move', 'open', 'export', 'new']} /> : null}
        </>
      ) : null}
      {drawer === 'add' ? (
        <AddModelDrawer
          open
          onClose={() => setDrawer(null)}
          onAdded={(name) => {
            // Show what was added: back to the list, on the new model, with the activity reread.
            const search = new URLSearchParams(window.location.search);
            search.delete('tab');
            search.set('model', name);
            router.replace(`${pathname}?${search.toString()}`, { scroll: false });
            void useCatalog.getState().loadCatalogAudit();
          }}
        />
      ) : null}
      <AddProviderDrawer
        open={drawer === 'provider'}
        onClose={() => setDrawer(null)}
        taken={[...providers.keys()]}
        onAdded={() => {
          void loadProviders().then(() => setDrawer('add'));
        }}
      />
      {(drawer === 'edit' || drawer === 'lifecycle') && current ? (
        <EditModelDrawer
          key={`${current.name}-${drawer}`}
          model={current.model}
          focus={drawer === 'lifecycle' ? 'lifecycle' : undefined}
          onClose={() => setDrawer(null)}
        />
      ) : null}
    </>
  );
}
