import { create } from 'zustand';

import type {
  Alert,
  AuditEntry,
  DeploymentHealth,
  DeploymentResource,
  Health,
  LogRecord,
  RouteResource,
  Tier,
  UsageBucket,
} from '@/lib/api/types';
import { asApiError, emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  type PoliciesSettings,
  type ResilienceSettings,
  type RouteReach,
  type RouteSimulation,
  type RouteStateEntry,
  routingApi,
  type ServerSettings,
} from './api';
import type { Json } from './types';

const HOUR = 3_600_000;

interface RoutingState {
  routes: Slot<RouteResource[]>;
  deployments: Slot<DeploymentResource[]>;
  health: Slot<DeploymentHealth[]>;
  resilience: Slot<ResilienceSettings>;
  policies: Slot<PoliciesSettings>;
  server: Slot<ServerSettings>;
  gateway: Slot<Health>;
  simulation: Slot<RouteSimulation>;
  tiers: Slot<Tier[]>;
  /** The shown route's nodes on the answering replica: canary and mirror results. */
  state: Slot<RouteStateEntry[]>;
  /** What the shown route meets: its Models and what it does with unmatched requests. */
  reach: RouteReach | null;
  audit: Slot<AuditEntry[]>;
  alerts: Slot<Alert[]>;
  /** Requests per tree position over the last 24 hours. */
  nodeUsage: Slot<UsageBucket[]>;
  /** Blocks by guardrail category over the last 24 hours. */
  categoryUsage: Slot<UsageBucket[]>;
  /** The newest records of the last hour, for the decision time. */
  recent: Slot<LogRecord[]>;
  guardrails: Slot<string[]>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  /** The reads behind the details: impact, editors, alerts, decision time, the change feed. */
  loadDetails: (options?: { quiet?: boolean }) => Promise<void>;
  loadState: (route: string, options?: { quiet?: boolean }) => Promise<void>;
  loadSettings: (options?: { quiet?: boolean }) => Promise<void>;
  saveTree: (route: string, tree: Json, reason?: string) => Promise<void>;
  /** Creates a shared route with this tree (the first rule of a new gateway). */
  createRoute: (route: string, tree: Json) => Promise<void>;
  deleteRoute: (route: string) => Promise<void>;
  simulate: (route: string, tree: Json, hours: number) => Promise<void>;
  clearSimulation: () => void;
  resetAffinity: (route: string) => Promise<number>;
  /** Merges gateway defaults: retries and timeouts (policies), breaker (resilience), admission (server). */
  saveDefaults: (patch: {
    policies?: Record<string, unknown>;
    resilience?: Record<string, unknown>;
    server?: Record<string, unknown>;
  }) => Promise<void>;
}

/** Routes, the deployments they reach, breaker health, the defaults and the routing figures. */
export const useRouting = create<RoutingState>()((set, get) => ({
  routes: emptySlot(),
  deployments: emptySlot(),
  health: emptySlot(),
  resilience: emptySlot(),
  policies: emptySlot(),
  server: emptySlot(),
  gateway: emptySlot(),
  simulation: emptySlot(),
  tiers: emptySlot(),
  state: emptySlot(),
  reach: null,
  audit: emptySlot(),
  alerts: emptySlot(),
  nodeUsage: emptySlot(),
  categoryUsage: emptySlot(),
  recent: emptySlot(),
  guardrails: emptySlot(),

  async load(options) {
    await Promise.all([
      loadSlot(set, get, 'routes', routingApi.routes, options),
      loadSlot(set, get, 'deployments', routingApi.deployments, options),
      loadSlot(set, get, 'health', routingApi.health, options),
      loadSlot(set, get, 'resilience', routingApi.resilience, options),
      loadSlot(set, get, 'tiers', routingApi.tiers, options),
      loadSlot(set, get, 'gateway', routingApi.gatewayHealth, options),
    ]);
  },

  async loadDetails(options) {
    const to = new Date();
    const day = new Date(to.getTime() - 24 * HOUR).toISOString();
    await Promise.all([
      loadSlot(set, get, 'audit', routingApi.audit, options),
      loadSlot(
        set,
        get,
        'alerts',
        () => routingApi.alerts(new Date(to.getTime() - 7 * 24 * HOUR).toISOString()),
        options,
      ),
      loadSlot(
        set,
        get,
        'nodeUsage',
        () => routingApi.usageBy(day, to.toISOString(), ['route_node']),
        options,
      ),
      loadSlot(
        set,
        get,
        'categoryUsage',
        () => routingApi.usageBy(day, to.toISOString(), ['guardrail_category']),
        options,
      ),
      loadSlot(
        set,
        get,
        'recent',
        () => routingApi.recent(new Date(to.getTime() - HOUR).toISOString(), 200),
        options,
      ),
      loadSlot(set, get, 'guardrails', routingApi.guardrails, options),
    ]);
  },

  async loadState(route, options) {
    await loadSlot(
      set,
      get,
      'state',
      async () => {
        const { items, reach } = await routingApi.state(route);
        set({ reach });
        return items;
      },
      options,
    );
  },

  async loadSettings(options) {
    await Promise.all([
      loadSlot(set, get, 'policies', routingApi.policies, options),
      loadSlot(set, get, 'server', routingApi.server, options),
    ]);
  },

  async saveTree(route, tree, reason) {
    const current = get().routes.data?.find((r) => r.name === route);
    if (!current) throw new Error(`route ${route} is not loaded`);
    const updated = await routingApi.save(route, { ...current.spec, tree }, current.metadata.etag, reason);
    set((s) => ({
      routes: { ...s.routes, data: (s.routes.data ?? []).map((r) => (r.name === route ? updated : r)) },
    }));
    void get().loadDetails({ quiet: true });
  },

  async deleteRoute(route) {
    await routingApi.remove(route);
    set((s) => ({ routes: { ...s.routes, data: (s.routes.data ?? []).filter((r) => r.name !== route) } }));
    void get().loadDetails({ quiet: true });
  },

  async createRoute(route, tree) {
    const created = await routingApi.save(route, { tree }, undefined);
    set((s) => ({
      routes: { ...s.routes, data: [...(s.routes.data ?? []).filter((r) => r.name !== route), created] },
    }));
  },

  async simulate(route, tree, hours) {
    const current = get().routes.data?.find((r) => r.name === route);
    if (!current) return;
    const to = new Date();
    const from = new Date(to.getTime() - hours * HOUR);
    set({ simulation: { ...emptySlot(), status: 'loading' } });
    try {
      const data = await routingApi.simulate(
        route,
        { ...current.spec, tree },
        from.toISOString(),
        to.toISOString(),
      );
      set({ simulation: { data, status: 'ready', error: null, at: Date.now() } });
    } catch (error) {
      set({ simulation: { data: null, status: 'error', error: asApiError(error), at: null } });
    }
  },

  clearSimulation() {
    set({ simulation: emptySlot() });
  },

  async resetAffinity(route) {
    const deleted = await routingApi.resetAffinity(route);
    void get().loadState(route, { quiet: true });
    return deleted;
  },

  async saveDefaults(patch) {
    const s = get();
    if (patch.policies) {
      const updated = await routingApi.patchPolicies(patch.policies, s.policies.data?.metadata.etag);
      set({ policies: { data: updated, status: 'ready', error: null, at: Date.now() } });
    }
    if (patch.resilience) {
      const updated = await routingApi.patchResilience(patch.resilience, s.resilience.data?.metadata.etag);
      set({ resilience: { data: updated, status: 'ready', error: null, at: Date.now() } });
    }
    if (patch.server) {
      const updated = await routingApi.patchServer(patch.server, s.server.data?.metadata.etag);
      set({ server: { data: updated, status: 'ready', error: null, at: Date.now() } });
    }
    void get().loadDetails({ quiet: true });
  },
}));
