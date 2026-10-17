import { create } from 'zustand';

import type {
  Alert,
  AlertRuleResource,
  AuditEntry,
  LatencyHistogram,
  UsageBucket,
  DeploymentHealth,
  DeploymentResource,
  EffectivePrice,
  ModelResource,
  ModelSpec,
  ProviderResource,
  RouteResource,
  Tier,
} from '@/lib/api/types';
import { api } from '@/lib/api/client';
import { createCollectionStore } from '@/lib/collection-store';
import { downloadCsv, downloadText } from '@/lib/csv';
import { asApiError, emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  type ModelCreateInput,
  type ModelUsageRow,
  modelsApi,
  modelsCollection,
  type RoutingSettings,
} from './api';
import type { AccessRequest, CatalogStatus, DeploymentCapabilities } from './catalog';
import { type ModelView, rate } from './types';

const DAY = 86_400_000;

export const useModels = createCollectionStore<ModelResource, ModelCreateInput, ModelSpec>(modelsCollection);

export type ModelUsage = ModelUsageRow;

interface CatalogState {
  deployments: Slot<DeploymentResource[]>;
  providers: Slot<ProviderResource[]>;
  routes: Slot<RouteResource[]>;
  tiers: Slot<Tier[]>;
  health: Slot<DeploymentHealth[]>;
  usage: Slot<Map<string, ModelUsage>>;
  /** The 30 days before the last 30, per Model: the base of the hero's changes. */
  previous: Slot<Map<string, { requests: number; cost: number; errors: number }>>;
  /** 30-day requests per name apps sent (a Model's name or one of its aliases). */
  requested: Slot<Map<string, number>>;
  prices: Record<string, EffectivePrice | null>;
  capabilities: Record<string, DeploymentCapabilities | null>;
  routing: Slot<RoutingSettings>;
  rules: Slot<AlertRuleResource[]>;
  catalogAudit: Slot<AuditEntry[]>;
  /** Where the model and price catalog comes from and when it last changed. */
  catalogStatus: Slot<CatalogStatus>;
  audit: Record<string, Slot<AuditEntry[]>>;
  access: Record<string, Slot<AccessRequest[]>>;
  loadCatalog: () => Promise<void>;
  loadPrevious: () => Promise<void>;
  loadRequested: () => Promise<void>;
  loadCapabilities: (deployments: string[]) => Promise<void>;
  loadRouting: () => Promise<void>;
  loadRules: () => Promise<void>;
  loadCatalogAudit: () => Promise<void>;
  loadAccess: (model: string) => Promise<void>;
  /** Re-reads the providers, after one is added from the Models screen. */
  loadProviders: () => Promise<void>;
  loadUsage: () => Promise<void>;
  loadPrices: (deployments: string[]) => Promise<void>;
  loadAudit: (model: string) => Promise<void>;
}

/** Everything a Model's row and page need besides the Model: its deployments, routes, health, prices. */
export const useCatalog = create<CatalogState>()((set, get) => ({
  deployments: emptySlot(),
  providers: emptySlot(),
  routes: emptySlot(),
  tiers: emptySlot(),
  health: emptySlot(),
  usage: emptySlot(),
  previous: emptySlot(),
  requested: emptySlot(),
  prices: {},
  capabilities: {},
  routing: emptySlot(),
  rules: emptySlot(),
  catalogAudit: emptySlot(),
  catalogStatus: emptySlot(),
  audit: {},
  access: {},

  async loadCatalog() {
    await Promise.all([
      loadSlot(set, get, 'deployments', modelsApi.deployments),
      loadSlot(set, get, 'providers', modelsApi.providers),
      loadSlot(set, get, 'routes', modelsApi.routes),
      loadSlot(set, get, 'tiers', modelsApi.tiers),
      loadSlot(set, get, 'health', modelsApi.health),
      loadSlot(set, get, 'routing', modelsApi.routing),
      loadSlot(set, get, 'catalogStatus', modelsApi.catalogStatus),
      modelsApi.prices().then(
        (list) =>
          set((s) => ({
            prices: { ...s.prices, ...Object.fromEntries(list.map((p) => [p.deployment, p])) },
          })),
        () => undefined,
      ),
    ]);
    const leaves = (get().deployments.data ?? []).map((d) => d.name);
    void get().loadCapabilities(leaves);
  },

  loadPrevious: () =>
    loadSlot(set, get, 'previous', async () => {
      const to = Date.now() - 30 * DAY;
      const map = await modelsApi.usage(new Date(to - 30 * DAY).toISOString(), new Date(to).toISOString());
      return new Map([...map].map(([k, v]) => [k, { requests: v.requests, cost: v.cost, errors: v.errors }]));
    }),

  loadRequested: () =>
    loadSlot(set, get, 'requested', async () => {
      const to = new Date();
      const rows = await modelsApi.usageBy(
        new Date(to.getTime() - 30 * DAY).toISOString(),
        to.toISOString(),
        ['requested_model'],
      );
      const map = new Map<string, number>();
      for (const r of rows) {
        const name = r.group.requested_model;
        if (name) map.set(name, (map.get(name) ?? 0) + r.requests);
      }
      return map;
    }),

  async loadCapabilities(deployments) {
    const missing = deployments.filter((d) => !(d in get().capabilities)).slice(0, 200);
    for (let i = 0; i < missing.length; i += 8) {
      const part = missing.slice(i, i + 8);
      const results = await Promise.all(
        part.map(async (d) => {
          try {
            return [d, await modelsApi.capabilities(d)] as const;
          } catch {
            return [d, null] as const;
          }
        }),
      );
      set((s) => ({ capabilities: { ...s.capabilities, ...Object.fromEntries(results) } }));
    }
  },

  loadRouting: () => loadSlot(set, get, 'routing', modelsApi.routing, { quiet: true }),
  loadRules: () => loadSlot(set, get, 'rules', modelsApi.alertRules),
  loadCatalogAudit: () =>
    loadSlot(set, get, 'catalogAudit', async () => {
      const parts = await Promise.all(
        ['models/*', 'routes/*', 'deployments/*', 'settings/routing'].map((r) =>
          modelsApi.auditUnder(r, 40).catch(() => [] as AuditEntry[]),
        ),
      );
      return parts
        .flat()
        .sort((a, b) => b.time.localeCompare(a.time))
        .slice(0, 60);
    }),

  async loadAccess(model) {
    set((s) => ({
      access: { ...s.access, [model]: { ...(s.access[model] ?? emptySlot()), status: 'loading' } },
    }));
    try {
      const data = await modelsApi.accessRequests(model);
      set((s) => ({
        access: { ...s.access, [model]: { data, status: 'ready', error: null, at: Date.now() } },
      }));
    } catch (error) {
      set((s) => ({
        access: { ...s.access, [model]: { data: null, status: 'error', error: asApiError(error), at: null } },
      }));
    }
  },

  loadProviders: () => loadSlot(set, get, 'providers', modelsApi.providers, { quiet: true }),

  loadUsage: () =>
    loadSlot(set, get, 'usage', () => {
      const to = new Date();
      return modelsApi.usage(new Date(to.getTime() - 30 * 86_400_000).toISOString(), to.toISOString());
    }),

  async loadPrices(deployments) {
    const missing = deployments.filter((d) => !(d in get().prices)).slice(0, 50);
    const results = await Promise.all(
      missing.map(async (d) => {
        try {
          return [d, await modelsApi.price(d)] as const;
        } catch {
          return [d, null] as const;
        }
      }),
    );
    set((s) => ({ prices: { ...s.prices, ...Object.fromEntries(results) } }));
  },

  async loadAudit(model) {
    set((s) => ({
      audit: { ...s.audit, [model]: { ...(s.audit[model] ?? emptySlot()), status: 'loading' } },
    }));
    try {
      const [own, under] = await Promise.all([
        modelsApi.audit(model),
        modelsApi.auditUnder(`models/${model}/*`).catch(() => [] as AuditEntry[]),
      ]);
      const data = [...own, ...under].sort((a, b) => b.time.localeCompare(a.time));
      set((s) => ({
        audit: { ...s.audit, [model]: { data, status: 'ready', error: null, at: Date.now() } },
      }));
    } catch (error) {
      set((s) => ({
        audit: { ...s.audit, [model]: { data: null, status: 'error', error: asApiError(error), at: null } },
      }));
    }
  },
}));

/** Adds a Model on a new or existing deployment; a deployment created here is removed if the Model is refused. */
export async function addModel(input: {
  name: string;
  spec: ModelSpec;
  deployment: {
    name: string;
    provider: string;
    model: string;
    price?: { input: string; output: string };
  } | null;
  existingDeployment: string | null;
}): Promise<ModelResource> {
  let created: string | null = null;
  if (input.deployment) {
    const dep = await modelsApi.createDeployment(input.deployment.name, {
      provider: input.deployment.provider,
      model: input.deployment.model,
      ...(input.deployment.price ? { price: input.deployment.price } : {}),
    });
    created = dep.name;
    useCatalog.setState((s) => ({
      deployments: { ...s.deployments, data: [...(s.deployments.data ?? []), dep] },
    }));
  }
  const target = input.existingDeployment ?? created;
  try {
    return await useModels
      .getState()
      .create({ name: input.name, spec: { ...input.spec, target: target ?? undefined } });
  } catch (error) {
    if (created) {
      await modelsApi.deleteDeployment(created).catch(() => undefined);
      useCatalog.setState((s) => ({
        deployments: { ...s.deployments, data: (s.deployments.data ?? []).filter((d) => d.name !== created) },
      }));
    }
    throw error;
  }
}

export async function setModelEnabled(name: string, enabled: boolean, reason?: string): Promise<void> {
  const current = useModels.getState().byName(name);
  const updated = await modelsApi.setEnabled(name, enabled, current?.metadata.etag, reason);
  useModels.setState((s) => ({ items: s.items.map((m) => (m.name === name ? updated : m)) }));
}

export async function overridePrice(
  deployment: string,
  price: { input: string; output: string },
  reason?: string,
): Promise<void> {
  const dep = useCatalog.getState().deployments.data?.find((d) => d.name === deployment);
  const updated = await modelsApi.patchDeployment(
    deployment,
    { price: { ...(dep?.spec.price ?? {}), ...price } },
    dep?.metadata.etag,
    reason,
  );
  useCatalog.setState((s) => {
    const prices = Object.fromEntries(Object.entries(s.prices).filter(([d]) => d !== deployment));
    return {
      deployments: {
        ...s.deployments,
        data: (s.deployments.data ?? []).map((d) => (d.name === deployment ? updated : d)),
      },
      prices,
    };
  });
  await useCatalog.getState().loadPrices([deployment]);
}

/** "Export pricing": the gateway's effective-price CSV, or the same columns built from each leaf's price. */
export async function exportPricing(views: ModelView[]): Promise<void> {
  try {
    const csv = await api.get('/pricing/deployments', { query: { format: 'csv' } });
    if (typeof csv === 'string') {
      downloadText('pricing.csv', csv, 'text/csv');
      return;
    }
  } catch {
    // Older gateways answer one deployment at a time.
  }
  const leaves = [...new Set(views.flatMap((v) => v.leaves))];
  await useCatalog.getState().loadPrices(leaves);
  const prices = useCatalog.getState().prices;
  const rows = views.flatMap((v) =>
    v.leaves.map((leaf) => [
      v.name,
      leaf,
      v.providers.join(' '),
      rate(prices[leaf]?.price, 'input'),
      rate(prices[leaf]?.price, 'output'),
      rate(prices[leaf]?.price, 'cache_read'),
    ]),
  );
  downloadCsv(
    'pricing.csv',
    ['model', 'deployment', 'provider', 'input_per_1m', 'output_per_1m', 'cache_read_per_1m'],
    rows,
  );
}

/** What a Model's page reads about it besides the catalog: its usage by team, key and name, latency and alerts. */
export interface ModelInsight {
  teams: UsageBucket[];
  keys: UsageBucket[];
  requested: UsageBucket[];
  histogram: LatencyHistogram | null;
  alerts: Alert[];
}

interface InsightState {
  slots: Record<string, Slot<ModelInsight>>;
  load: (model: string, options: { alerts: boolean; quiet?: boolean }) => Promise<void>;
}

export const useModelInsight = create<InsightState>()((set, get) => ({
  slots: {},
  async load(model, { alerts, quiet }) {
    const previous = get().slots[model] ?? emptySlot<ModelInsight>();
    if (!quiet || previous.data === null)
      set((s) => ({ slots: { ...s.slots, [model]: { ...previous, status: 'loading', error: null } } }));
    const to = new Date();
    const from = new Date(to.getTime() - 30 * DAY).toISOString();
    const until = to.toISOString();
    try {
      const [teams, keys, requested, histogram, fired] = await Promise.all([
        modelsApi.modelUsage(model, from, until, 'team'),
        modelsApi.modelUsage(model, from, until, 'key'),
        modelsApi.modelUsage(model, from, until, 'requested_model'),
        modelsApi.histogram(model, from, until).catch(() => null),
        alerts ? modelsApi.alerts(from).catch(() => [] as Alert[]) : Promise.resolve([] as Alert[]),
      ]);
      const data: ModelInsight = {
        teams,
        keys,
        requested,
        histogram,
        alerts: fired.filter((a) => a.scope?.model === model),
      };
      set((s) => ({
        slots: { ...s.slots, [model]: { data, status: 'ready', error: null, at: Date.now() } },
      }));
    } catch (error) {
      set((s) => ({
        slots: {
          ...s.slots,
          [model]: { ...(s.slots[model] ?? previous), status: 'error', error: asApiError(error) },
        },
      }));
    }
  },
}));

function storeModel(updated: ModelResource): void {
  useModels.setState((s) => ({ items: s.items.map((m) => (m.name === updated.name ? updated : m)) }));
}

/** Replaces a Model's spec (aliases, chain, access…), with its ETag. */
export async function saveModelSpec(name: string, spec: ModelSpec, reason?: string): Promise<ModelResource> {
  const current = useModels.getState().byName(name);
  const updated = await modelsApi.replace(name, spec, current?.metadata.etag, reason);
  storeModel(updated);
  return updated;
}

/** Merges part of a Model's spec (its lifecycle, its access list). */
export async function patchModelSpec(
  name: string,
  spec: Partial<ModelSpec>,
  reason?: string,
): Promise<ModelResource> {
  const current = useModels.getState().byName(name);
  const updated = await modelsApi.patch(name, spec, current?.metadata.etag, reason);
  storeModel(updated);
  return updated;
}

/** Approves or denies an access request; an approval changes the Model's access list, so it is read again. */
export async function decideAccess(
  model: string,
  id: string,
  verb: 'approve' | 'deny',
  reason?: string,
): Promise<void> {
  if (verb === 'approve') await modelsApi.approveAccess(model, id, reason);
  else await modelsApi.denyAccess(model, id, reason);
  await Promise.all([
    useCatalog.getState().loadAccess(model),
    verb === 'approve' ? useModels.getState().fetch({ quiet: true }) : Promise.resolve(),
  ]);
}

/** Merges a change into the routing settings (default Models, custom tiers), with their ETag. */
export async function patchRouting(spec: Record<string, unknown>, reason?: string): Promise<void> {
  const etag = useCatalog.getState().routing.data?.metadata.etag;
  const updated = await modelsApi.patchRouting(spec, etag, reason);
  useCatalog.setState((s) => ({ routing: { ...s.routing, data: updated, at: Date.now() } }));
  await loadTiersQuietly();
}

async function loadTiersQuietly(): Promise<void> {
  try {
    const tiers = await modelsApi.tiers();
    useCatalog.setState((s) => ({ tiers: { ...s.tiers, data: tiers, status: 'ready', at: Date.now() } }));
  } catch {
    // The tier list catches up on the next full read.
  }
}
