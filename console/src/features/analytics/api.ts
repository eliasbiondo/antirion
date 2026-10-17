import { api, listAll } from '@/lib/api/client';
import type { components, operations } from '@/lib/api/schema';
import type { LogRecord, UsageBucket } from '@/lib/api/types';
import type { Interval } from '@/lib/time';

export type GroupBy = NonNullable<
  NonNullable<operations['getUsageAnalytics']['parameters']['query']>['group_by']
>[number];

export interface UsageQuery {
  from: string;
  to: string;
  interval?: Interval;
  groupBy?: GroupBy[];
  organization: string | null;
  tag: string | null;
}

export const analyticsApi = {
  /** Every bucket of the window, page by page (a week of hours is more than the default page). */
  usage: (q: UsageQuery): Promise<UsageBucket[]> =>
    listAll(
      (cursor) =>
        api.get('/analytics/usage', {
          query: {
            from: q.from,
            to: q.to,
            limit: 1000,
            ...(cursor ? { cursor } : {}),
            ...(q.interval ? { interval: q.interval } : {}),
            ...(q.groupBy?.length ? { group_by: q.groupBy } : {}),
            ...(q.organization ? { organization: [q.organization] } : {}),
            ...(q.tag ? { tag: [q.tag] } : {}),
          },
        }),
      10,
    ),
  availability: async (from: string, to: string) =>
    (await api.get('/reports/availability', { query: { from, to, group_by: 'provider' } })).items,
};

export type ProviderPressure = components['schemas']['ProviderPressure'];
export type Generation = components['schemas']['GenerationSummary'];

/** The reads the Analytics screen adds to the usage: pressure, annotations, the SLO, fallback trails. */
export const analyticsExtrasApi = {
  pressure: (from: string, to: string, interval: Interval | undefined): Promise<ProviderPressure> =>
    api.get('/reports/provider_pressure', { query: { from, to, ...(interval ? { interval } : {}) } }),
  generations: async (): Promise<Generation[]> =>
    (await api.get('/config/generations', { query: { limit: 200 } })).items,
  /** The latency SLO's threshold of a gateway-wide latency objective, in ms, or null. */
  latencySlo: async (): Promise<{ name: string; thresholdMs: number; objective: number } | null> => {
    const settings = await api.get('/settings/observability');
    const slos = (settings.spec as { slos?: Record<string, Record<string, unknown>> }).slos ?? {};
    for (const [name, slo] of Object.entries(slos)) {
      const threshold = slo.latency_threshold_ms;
      if (slo.kind === 'latency' && typeof threshold === 'number' && !slo.scope)
        return {
          name,
          thresholdMs: threshold,
          objective: typeof slo.objective === 'number' ? slo.objective : 0,
        };
    }
    return null;
  },
  /** Newest records a deployment served in a window, to read the attempt trail of a fallback. */
  served: async (deployment: string, from: string, to: string): Promise<LogRecord[]> =>
    (await api.get('/logs', { query: { deployment: [deployment], from, to, limit: 50 } })).items,
  notify: (team: string, subject: string, message: string, link: string | undefined) =>
    api.post('/notifications:send', {
      body: { to: { team }, subject, message, ...(link ? { link } : {}) },
    }),
};
