'use client';

import { useEffect, useMemo } from 'react';

import { useCan } from '@/components/ui/gate';
import type { DeploymentResource, EffectivePrice, ProviderResource, RouteResource } from '@/lib/api/types';
import { usePolling } from '@/lib/hooks';

import { sumBuckets } from './catalog';
import { useCatalog, useModels } from './store';
import { type Lifecycle, type Modality, type ModelView, toModelView } from './types';

export function useModelsScreenData(): void {
  const fetch = useModels((s) => s.fetch);
  const loadCatalog = useCatalog((s) => s.loadCatalog);
  const loadUsage = useCatalog((s) => s.loadUsage);
  const loadPrevious = useCatalog((s) => s.loadPrevious);
  const loadRequested = useCatalog((s) => s.loadRequested);
  const loadRules = useCatalog((s) => s.loadRules);
  const loadCatalogAudit = useCatalog((s) => s.loadCatalogAudit);
  const canUsage = useCan('getUsageAnalytics');
  const canRules = useCan('listAlertRules');
  const canAudit = useCan('listAudit');
  useEffect(() => {
    void fetch();
    void loadCatalog();
    void loadUsage();
    if (canUsage) {
      void loadPrevious();
      void loadRequested();
    }
    if (canRules) void loadRules();
    if (canAudit) void loadCatalogAudit();
  }, [
    fetch,
    loadCatalog,
    loadUsage,
    loadPrevious,
    loadRequested,
    loadRules,
    loadCatalogAudit,
    canUsage,
    canRules,
    canAudit,
  ]);
  usePolling(() => void fetch({ quiet: true }));
}

export function useDeploymentMap(): Map<string, DeploymentResource> {
  const deployments = useCatalog((s) => s.deployments.data);
  return useMemo(() => new Map((deployments ?? []).map((d) => [d.name, d])), [deployments]);
}

export function useProviderMap(): Map<string, ProviderResource> {
  const providers = useCatalog((s) => s.providers.data);
  return useMemo(() => new Map((providers ?? []).map((p) => [p.name, p])), [providers]);
}

export function useRouteMap(): Map<string, RouteResource> {
  const routes = useCatalog((s) => s.routes.data);
  return useMemo(() => new Map((routes ?? []).map((r) => [r.name, r])), [routes]);
}

export function useModelViews(): ModelView[] {
  const items = useModels((s) => s.items);
  const deployments = useDeploymentMap();
  const routes = useRouteMap();
  const capabilities = useCatalog((s) => s.capabilities);
  return useMemo(
    () => items.map((m) => toModelView(m, deployments, routes, capabilities)),
    [items, deployments, routes, capabilities],
  );
}

export type ModelSort = 'name' | 'requests' | 'spend' | 'p95' | 'output_price';
export type StateFilter = Lifecycle | 'enabled' | 'disabled';

/** The list's filters, as they are kept in the URL (`model` is the selected Model, not a filter). */
export const MODEL_FILTER_DEFAULTS = {
  q: '',
  provider: '',
  status: [] as string[],
  tier: '',
  modality: '',
  label: [] as string[],
  sort: 'name',
};

export interface ModelFilters {
  provider: string;
  states: StateFilter[];
  tier: string;
  modality: Modality | '';
  labels: string[];
  query: string;
  sort: ModelSort;
}

export const EMPTY_MODEL_FILTERS: ModelFilters = {
  provider: '',
  states: [],
  tier: '',
  modality: '',
  labels: [],
  query: '',
  sort: 'name',
};

const SORTS: ModelSort[] = ['name', 'requests', 'spend', 'p95', 'output_price'];

/** The URL's filter values as typed filters (an unknown sort falls back to name). */
export function toModelFilters(f: typeof MODEL_FILTER_DEFAULTS): ModelFilters {
  return {
    provider: f.provider,
    states: f.status as StateFilter[],
    tier: f.tier,
    modality: f.modality as Modality | '',
    labels: f.label,
    query: f.q,
    sort: SORTS.includes(f.sort as ModelSort) ? (f.sort as ModelSort) : 'name',
  };
}

export interface ModelMeasures {
  requests: number;
  cost: number;
  /** Request-weighted mean of the daily p95 latencies (ms), or null without traffic. */
  p95: number | null;
}

/** A Model's 30-day measures for sorting, from its daily buckets. */
export function measuresOf(
  usage: { requests: number; cost: number; buckets?: Parameters<typeof sumBuckets>[0] } | undefined,
): ModelMeasures | undefined {
  if (!usage) return undefined;
  return {
    requests: usage.requests,
    cost: usage.cost,
    p95: usage.buckets ? sumBuckets(usage.buckets).latencyP95 : null,
  };
}

/** Whether a model carries a status chip's state: its lifecycle, or on/off. */
export function hasState(v: Pick<ModelView, 'lifecycle' | 'enabled'>, state: StateFilter): boolean {
  return v.lifecycle === state || (v.enabled ? 'enabled' : 'disabled') === state;
}

export function filterModels(
  views: ModelView[],
  f: ModelFilters,
  usage: Map<string, ModelMeasures> | null,
  outputPrice: (v: ModelView) => number | null,
): ModelView[] {
  const q = f.query.trim().toLowerCase();
  const out = views.filter((v) => {
    if (f.provider && !v.providers.includes(f.provider)) return false;
    if (f.tier && v.tier !== f.tier) return false;
    if (f.modality && v.modality !== f.modality) return false;
    if (f.labels.length > 0 && !f.labels.every((l) => (v.model.spec.labels ?? []).includes(l))) return false;
    if (f.states.length > 0 && !f.states.some((s) => hasState(v, s))) return false;
    if (!q) return true;
    const hay = [
      v.name,
      v.displayName,
      ...(v.model.spec.labels ?? []),
      v.model.spec.description ?? '',
      ...v.aliases,
      ...v.upstream,
    ]
      .join(' ')
      .toLowerCase();
    return hay.includes(q);
  });
  const by = (v: ModelView) => {
    const m = usage?.get(v.name);
    if (f.sort === 'requests') return -(m?.requests ?? 0);
    if (f.sort === 'spend') return -(m?.cost ?? 0);
    if (f.sort === 'p95') return m?.p95 ?? Number.MAX_SAFE_INTEGER;
    if (f.sort === 'output_price') return outputPrice(v) ?? Number.MAX_SAFE_INTEGER;
    return 0;
  };
  return f.sort === 'name' ? out : [...out].sort((a, b) => by(a) - by(b));
}

/** The effective price of a deployment: undefined while unknown, null when the catalog has none. */
export function useEffectivePrice(deployment: string | undefined): EffectivePrice | null | undefined {
  const loadPrices = useCatalog((s) => s.loadPrices);
  const price = useCatalog((s) => (deployment ? s.prices[deployment] : undefined));
  useEffect(() => {
    if (deployment) void loadPrices([deployment]);
  }, [deployment, loadPrices]);
  return price;
}
