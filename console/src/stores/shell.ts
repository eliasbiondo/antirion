import { create } from 'zustand';

import { api, setWriteListener } from '@/lib/api/client';
import type { Status } from '@/lib/api/types';
import { hasGatewayPermission, hasPermission, useSession } from '@/stores/session';

export interface ShellFacts {
  status: Status | null;
  modelCount: number | null;
  providerCount: number | null;
  nodesReady: number | null;
  nodesKnown: number | null;
  requests: number | null;
  flagged: number | null;
  alertsFiring: number | null;
  ingressPerMinute: number | null;
  availability30d: number | null;
  /** Change requests open or approved and not yet applied. */
  pendingChanges: number | null;
  loadedAt: number | null;
}

interface ShellState extends ShellFacts {
  load: (window: {
    from: string;
    to: string;
    organization: string | null;
    tag: string | null;
  }) => Promise<void>;
  /** Reads the pending change requests again (after one is opened, approved or applied). */
  loadPending: () => Promise<void>;
  /** Counts the Models and providers again (after one is created or deleted). */
  loadCatalogCounts: () => Promise<void>;
}

async function pendingCount(): Promise<number | null> {
  const me = useSession.getState().me;
  if (!hasPermission(me, 'config:read', null)) return null;
  const page = await settle(
    api.get('/change_requests', { query: { state: ['open', 'approved'], limit: 200 } }),
  );
  return page ? page.items.length : null;
}

/** A read the principal may make, else null without calling: a refused read only adds to the audit log. */
function readIf<T>(allowed: boolean, read: () => Promise<T>): Promise<T | null> {
  return allowed ? settle(read()) : Promise.resolve(null);
}

/** Whether the principal may read a permission's data in the current organization (or anywhere). */
function may(permission: string, organization: string | null = null): boolean {
  return hasPermission(useSession.getState().me, permission, organization);
}

async function settle<T>(promise: Promise<T>): Promise<T | null> {
  try {
    return await promise;
  } catch {
    return null;
  }
}

/**
 * The shell's facts: version, sidebar counts and the footer. Each comes from its own call and is
 * left blank ("—") when the gateway does not serve it or the principal may not read it.
 */
let latest = 0;

export const useShell = create<ShellState>()((set) => ({
  status: null,
  modelCount: null,
  providerCount: null,
  nodesReady: null,
  nodesKnown: null,
  requests: null,
  flagged: null,
  alertsFiring: null,
  ingressPerMinute: null,
  availability30d: null,
  pendingChanges: null,
  loadedAt: null,

  async loadPending() {
    set({ pendingChanges: await pendingCount() });
  },

  async loadCatalogCounts() {
    const [models, providers] = await Promise.all([
      readIf(hasGatewayPermission(useSession.getState().me, 'models:read'), () =>
        api.get('/models', { query: { limit: 500 } }),
      ),
      readIf(hasGatewayPermission(useSession.getState().me, 'providers:read'), () =>
        api.get('/providers', { query: { limit: 200 } }),
      ),
    ]);
    set({
      ...(models ? { modelCount: models.items.length } : {}),
      ...(providers ? { providerCount: providers.items.length } : {}),
    });
  },

  async load({ from, to, organization, tag }) {
    // Loads overlap when the range changes as a page opens; only the newest one's figures stand.
    latest += 1;
    const mine = latest;
    const scope = {
      ...(organization ? { organization: [organization] } : {}),
      ...(tag ? { tag: [tag] } : {}),
    };
    const now = new Date();
    const lastMinute = new Date(Math.floor(now.getTime() / 60_000) * 60_000);
    const minuteBefore = new Date(lastMinute.getTime() - 60_000);
    const monthAgo = new Date(now.getTime() - 30 * 86_400_000);
    const me = useSession.getState().me;
    const analytics = may('analytics:read', organization);
    const [status, models, providers, nodes, totals, flagged, alerts, ingress, availability, pending] =
      await Promise.all([
        settle(api.get('/status')),
        readIf(hasGatewayPermission(me, 'models:read'), () => api.get('/models', { query: { limit: 500 } })),
        readIf(hasGatewayPermission(me, 'providers:read'), () =>
          api.get('/providers', { query: { limit: 200 } }),
        ),
        readIf(hasGatewayPermission(me, 'cluster:read'), () => api.get('/runtime/nodes')),
        readIf(analytics, () => api.get('/analytics/usage', { query: { from, to, ...scope } })),
        readIf(analytics, () =>
          api.get('/analytics/usage', { query: { from, to, flagged: true, ...scope } }),
        ),
        readIf(may('alerts:read', organization), () =>
          api.get('/alerts', { query: { status: ['firing'], limit: 200 } }),
        ),
        readIf(analytics, () =>
          api.get('/analytics/usage', {
            query: {
              from: minuteBefore.toISOString(),
              to: lastMinute.toISOString(),
              interval: '1m',
              ...scope,
            },
          }),
        ),
        readIf(hasGatewayPermission(me, 'analytics:read'), () =>
          api.get('/reports/availability', {
            query: { from: monthAgo.toISOString(), to: now.toISOString(), group_by: 'gateway' },
          }),
        ),
        pendingCount(),
      ]);
    const sum = (rows: { requests: number }[] | undefined) =>
      rows ? rows.reduce((a, r) => a + r.requests, 0) : null;
    const availabilityRow = availability?.items[0] as { availability?: number } | undefined;
    if (mine !== latest) return;
    set({
      status,
      modelCount: models ? models.items.length : null,
      providerCount: providers ? providers.items.length : null,
      nodesReady: nodes ? nodes.items.filter((n) => n.ready).length : status ? status.nodes.length : null,
      nodesKnown: nodes ? nodes.items.length : status ? status.nodes.length : null,
      requests: sum(totals?.items),
      flagged: sum(flagged?.items),
      alertsFiring: alerts ? alerts.items.length : null,
      ingressPerMinute: sum(ingress?.items),
      availability30d:
        typeof availabilityRow?.availability === 'number' ? availabilityRow.availability : null,
      pendingChanges: pending,
      loadedAt: Date.now(),
    });
  },
}));

setWriteListener((path) => {
  if (/^\/(models|providers)(\/|$)/.test(path)) void useShell.getState().loadCatalogCounts();
});
