import { api, listAll } from '@/lib/api/client';
import type {
  BudgetCounter,
  BudgetEvent,
  DeploymentResource,
  KeyResource,
  ModelResource,
  OrganizationResource,
  OverageRequest,
  ProjectResource,
  TeamResource,
  UserResource,
} from '@/lib/api/types';

function path(c: Pick<BudgetCounter, 'level' | 'owner' | 'name'>) {
  return { level: c.level, owner: c.owner, name: c.name };
}

/** Requests and spend of one group of an analytics read. */
export interface Spend {
  requests: number;
  cost: number;
}

type Level = 'organization' | 'team' | 'project' | 'key';

/** Pages through a usage read and returns its buckets. */
async function usage(query: Record<string, unknown>) {
  return listAll(
    (cursor) =>
      api.get('/analytics/usage', {
        query: { limit: 1000, ...query, ...(cursor ? { cursor } : {}) },
      }),
    20,
  );
}

export const budgetsApi = {
  counters: (): Promise<BudgetCounter[]> =>
    listAll((cursor) => api.get('/runtime/budgets', { query: { limit: 200, cursor } })),
  teams: (): Promise<TeamResource[]> =>
    listAll((cursor) => api.get('/teams', { query: { limit: 200, cursor } })),
  projects: (): Promise<ProjectResource[]> =>
    listAll((cursor) => api.get('/projects', { query: { limit: 200, cursor } })),
  organizations: (): Promise<OrganizationResource[]> =>
    listAll((cursor) => api.get('/organizations', { query: { limit: 200, cursor } })),
  keys: (): Promise<KeyResource[]> =>
    listAll((cursor) => api.get('/keys', { query: { limit: 200, cursor } })),
  users: (): Promise<UserResource[]> =>
    listAll((cursor) => api.get('/users', { query: { limit: 200, cursor } })),
  models: (): Promise<ModelResource[]> =>
    listAll((cursor) => api.get('/models', { query: { limit: 200, cursor } })),
  events: async (): Promise<BudgetEvent[]> =>
    (await api.get('/runtime/budgets/events', { query: { limit: 50 } })).items,
  overageRequests: async (): Promise<OverageRequest[]> =>
    (await api.get('/budgets/overage_requests', { query: { state: 'open' } })).items,
  approveOverage: (id: string) =>
    api.post('/budgets/overage_requests/{request_id}:approve', { path: { request_id: id } }),
  denyOverage: (id: string) =>
    api.post('/budgets/overage_requests/{request_id}:deny', { path: { request_id: id } }),
  adjust: (c: BudgetCounter, amount: string, reason: string) =>
    api.post('/runtime/budgets/{level}/{owner}/{name}:adjust', {
      path: path(c),
      body: { amount, reason, ...(c.partition !== null ? { partition: c.partition } : {}) },
    }),
  reset: (c: BudgetCounter, reason: string) =>
    api.post('/runtime/budgets/{level}/{owner}/{name}:reset', {
      path: path(c),
      ...(c.partition !== null ? { query: { partition: c.partition } } : {}),
      body: { reason },
      reason,
    }),
  grant: (c: BudgetCounter, amount: string, expiresAt: string, reason: string) =>
    api.post('/runtime/budgets/{level}/{owner}/{name}:grant_overage', {
      path: path(c),
      body: {
        amount,
        expires_at: expiresAt,
        reason,
        ...(c.partition !== null ? { partition: c.partition } : {}),
      },
    }),
  revokeGrant: (c: BudgetCounter, id: string, reason?: string) =>
    api.delete('/runtime/budgets/{level}/{owner}/{name}/grants/{grant_id}', {
      path: { ...path(c), grant_id: id },
      ...(c.partition !== null ? { query: { partition: c.partition } } : {}),
      reason,
    }),
  /** Prices again the spend admitted in `[from, to)`; answers the corrections appended. */
  recost: async (from: string, to: string): Promise<number> =>
    (await api.post('/reports/spend:recost', { body: { from, to } })).lines_appended,
  notify: (user: string, subject: string, message: string, link?: string) =>
    api.post('/notifications:send', {
      body: { to: { users: [user] }, subject, message, ...(link ? { link } : {}) },
    }),
  dailySpend: async (
    from: string,
    to: string,
    organization: string | null,
  ): Promise<{ day: string; cost: number }[]> => {
    const result = await api.get('/analytics/usage', {
      query: { from, to, interval: '1d', ...(organization ? { organization: [organization] } : {}) },
    });
    return result.items.map((b) => ({ day: b.start, cost: Number(b.cost_usd) }));
  },
  /**
   * Request costs per UTC day charged to the given budget owners: one usage read per level, grouped
   * by it, keeping the owners asked for (a gateway-level budget counts every request).
   */
  async countedDaily(
    from: string,
    to: string,
    owners: { level: string; owner: string }[],
    organization: string | null,
  ): Promise<{ day: string; cost: number }[]> {
    const byDay = new Map<string, number>();
    const add = (day: string, cost: number) => byDay.set(day, (byDay.get(day) ?? 0) + cost);
    const scope = organization ? { organization: [organization] } : {};
    const levels = [...new Set(owners.map((o) => o.level))];
    await Promise.all(
      levels.map(async (level) => {
        if (level === 'gateway') {
          for (const b of await usage({ from, to, interval: '1d', ...scope }))
            add(b.start, Number(b.cost_usd));
          return;
        }
        const wanted = new Set(owners.filter((o) => o.level === level).map((o) => o.owner));
        const items = await usage({ from, to, interval: '1d', group_by: [level], ...scope });
        for (const b of items) {
          const owner = (b.group as Record<string, string | undefined>)[level];
          if (owner !== undefined && wanted.has(owner)) add(b.start, Number(b.cost_usd));
        }
      }),
    );
    return [...byDay].map(([day, cost]) => ({ day, cost }));
  },
  /** Spend adjustments made on the given budget owners since `from`, each with its time. */
  async adjustmentsSince(
    from: string,
    owners: { level: string; owner: string }[],
  ): Promise<{ time: string; amount: number }[]> {
    const wanted = new Set(owners.map((o) => `${o.level}/${o.owner}`));
    const events = await listAll(
      (cursor) =>
        api.get('/runtime/budgets/events', {
          query: { limit: 200, from, kind: ['adjusted'], ...(cursor ? { cursor } : {}) },
        }),
      10,
    );
    return events
      .filter((e) => wanted.has(`${e.level}/${e.owner}`))
      .map((e) => ({
        time: e.time,
        amount: Number((e.detail as { amount?: unknown } | undefined)?.amount ?? 0),
      }))
      .filter((a) => Number.isFinite(a.amount));
  },
  /** Daily spend per team since `from`, for the team burn chart. */
  async teamDaily(
    from: string,
    to: string,
    organization: string | null,
  ): Promise<Map<string, { day: string; cost: number }[]>> {
    const items = await usage({
      from,
      to,
      interval: '1d',
      group_by: ['team'],
      ...(organization ? { organization: [organization] } : {}),
    });
    const out = new Map<string, { day: string; cost: number }[]>();
    for (const b of items) {
      const team = b.group.team;
      if (!team) continue;
      const list = out.get(team) ?? [];
      list.push({ day: b.start, cost: Number(b.cost_usd) });
      out.set(team, list);
    }
    return out;
  },
  /**
   * Spend by Model of every organization, team, project and key since `from`, keyed by
   * `<level>/<owner>`: the Models a budget's owner used and the largest by cost.
   */
  async modelsUsed(from: string, to: string): Promise<Map<string, Map<string, Spend>>> {
    const levels: Level[] = ['organization', 'team', 'project', 'key'];
    const reads = await Promise.all(
      levels.map((level) =>
        usage({ from, to, group_by: [level, 'model'] }).then((items) => ({ level, items })),
      ),
    );
    const out = new Map<string, Map<string, Spend>>();
    for (const { level, items } of reads) {
      for (const b of items) {
        const owner = b.group[level];
        const model = b.group.model;
        if (!owner || !model) continue;
        const id = `${level}/${owner}`;
        const byModel = out.get(id) ?? new Map<string, Spend>();
        const prev = byModel.get(model) ?? { requests: 0, cost: 0 };
        byModel.set(model, { requests: prev.requests + b.requests, cost: prev.cost + Number(b.cost_usd) });
        out.set(id, byModel);
      }
    }
    return out;
  },
  /** Requests and spend per person since `from`. */
  async people(from: string, to: string, organization: string | null): Promise<Map<string, Spend>> {
    const items = await usage({
      from,
      to,
      group_by: ['user'],
      ...(organization ? { organization: [organization] } : {}),
    });
    const out = new Map<string, Spend>();
    for (const b of items) {
      const user = b.group.user;
      if (!user) continue;
      const prev = out.get(user) ?? { requests: 0, cost: 0 };
      out.set(user, { requests: prev.requests + b.requests, cost: prev.cost + Number(b.cost_usd) });
    }
    return out;
  },
  /** Requests a budget refused in the last hour for one owner (all of its budgets). */
  async refusedLastHour(level: string, owner: string): Promise<number | null> {
    if (!['organization', 'team', 'project', 'key'].includes(level)) return null;
    const to = new Date();
    const from = new Date(to.getTime() - 3_600_000);
    const result = await api.get('/analytics/usage', {
      query: {
        from: from.toISOString(),
        to: to.toISOString(),
        code: ['budget_exceeded'],
        [level]: [owner],
      },
    });
    return result.items.reduce((a, b) => a + b.requests, 0);
  },
  /** Gateway-wide price settings: the list prices' service-tier multipliers and batch multiplier. */
  pricing: async () => api.get('/settings/pricing'),
  /** Writes list prices (`pricing.models`, keyed `<provider_type>/<upstream_model>`) as a merge patch. */
  patchPricing: (models: Record<string, Record<string, unknown> | null>, etag: string | undefined) =>
    api.patch('/settings/pricing', { body: { spec: { models } }, ifMatch: etag }),
  /** Effective prices of every deployment: the upstream models whose list prices take tiers. */
  deploymentPrices: () =>
    listAll((cursor) => api.get('/pricing/deployments', { query: { limit: 200, cursor } })),
  /** Every deployment, for the tiers deployments set in their own price. */
  deployments: (): Promise<DeploymentResource[]> =>
    listAll((cursor) => api.get('/deployments', { query: { limit: 200, cursor } })),
  /** Reads the owner's budget declarations, the source of the edit drawer. */
  async declaration(
    level: string,
    owner: string,
  ): Promise<{ budgets: Record<string, unknown>; etag: string } | null> {
    const resource =
      level === 'organization'
        ? await api.get('/organizations/{name}', { path: { name: owner } })
        : level === 'team'
          ? await api.get('/teams/{name}', { path: { name: owner } })
          : level === 'project'
            ? await api.get('/projects/{name}', { path: { name: owner } })
            : level === 'key'
              ? await api.get('/keys/{name}', { path: { name: owner } })
              : null;
    if (!resource) return null;
    const budgets: Record<string, unknown> = resource.spec.policies?.budgets ?? {};
    return { budgets, etag: resource.metadata.etag };
  },
  /** Writes budget declarations (`null` removes one) as one merge patch on their owner. */
  async declare(
    level: string,
    owner: string,
    budgets: Record<string, Record<string, unknown> | null>,
    etag: string | undefined,
    reason?: string,
  ) {
    const body = { spec: { policies: { budgets } } };
    const opts = { path: { name: owner }, body, ifMatch: etag, reason };
    if (level === 'organization') return api.patch('/organizations/{name}', opts);
    if (level === 'team') return api.patch('/teams/{name}', opts);
    if (level === 'project') return api.patch('/projects/{name}', opts);
    if (level === 'key') return api.patch('/keys/{name}', opts);
    throw new Error(`budgets at level ${level} are edited in the configuration`);
  },
};
