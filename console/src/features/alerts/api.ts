import { api, listAll } from '@/lib/api/client';
import type { Alert, AlertChannelResource, AlertRuleResource, UsageBucket } from '@/lib/api/types';
import type { components } from '@/lib/api/schema';

import { DAY_MS } from './stats';

export type SloStatus = components['schemas']['SloStatus'];
export type DeliveryStatus = components['schemas']['DeliveryTargetStatus'];
export type TestResult = components['schemas']['TestResult'];
export type RuleSpec = AlertRuleResource['spec'];
export type ChannelSpec = AlertChannelResource['spec'];
export type AvailabilityRow = components['schemas']['AvailabilityRow'];

/** One `observability.alerts.webhooks` entry as the observability settings carry it. */
export interface WebhookSpec {
  url: string;
  events: string[];
  enabled?: boolean;
  signing_secret?: string | null;
  timeout?: string;
  max_retry_duration?: string;
  headers?: Record<string, string>;
  queue_capacity?: number;
}

export interface Webhooks {
  items: Record<string, WebhookSpec>;
  etag: string | undefined;
}

/** The scope dimensions a usage read can filter by (the route is not one). */
const USAGE_SCOPE = ['provider', 'deployment', 'model', 'organization', 'team', 'project', 'key'] as const;

export const alertsApi = {
  list: async (): Promise<Alert[]> => (await api.get('/alerts', { query: { limit: 200 } })).items,
  /** Every instance fired since `days` ago, for the 30-day figures and their comparison. */
  history: (days: number): Promise<Alert[]> =>
    listAll(
      (cursor) =>
        api.get('/alerts', {
          query: {
            from: new Date(Date.now() - days * DAY_MS).toISOString(),
            limit: 1000,
            ...(cursor ? { cursor } : {}),
          },
        }),
      20,
    ),
  get: (id: string): Promise<Alert> => api.get('/alerts/{alert_id}', { path: { alert_id: id } }),
  acknowledge: (id: string) => api.post('/alerts/{alert_id}:acknowledge', { path: { alert_id: id } }),
  resolve: (id: string) => api.post('/alerts/{alert_id}:resolve', { path: { alert_id: id } }),
  snooze: (id: string, duration: string) =>
    api.post('/alerts/{alert_id}:snooze', { path: { alert_id: id }, body: { duration } }),
  unsnooze: (id: string) => api.post('/alerts/{alert_id}:unsnooze', { path: { alert_id: id } }),
  note: (id: string, text: string) =>
    api.post('/alerts/{alert_id}:note', { path: { alert_id: id }, body: { text } }),
  escalate: (id: string) => api.post('/alerts/{alert_id}:escalate', { path: { alert_id: id } }),
  signal: async (id: string): Promise<UsageBucket[]> =>
    (await api.get('/reports/alerts/{alert_id}/signal', { path: { alert_id: id } })).items,
  similar: async (id: string): Promise<Alert[]> =>
    (await api.get('/reports/alerts/{alert_id}/similar', { path: { alert_id: id }, query: { limit: 50 } }))
      .items,

  rules: (): Promise<AlertRuleResource[]> =>
    listAll((cursor) => api.get('/alerts/rules', { query: { limit: 200, cursor } })),
  createRule: (name: string, spec: RuleSpec) => api.post('/alerts/rules', { body: { name, spec } }),
  replaceRule: (name: string, spec: RuleSpec, etag: string | undefined) =>
    api.put('/alerts/rules/{rule}', { path: { rule: name }, body: { spec }, ifMatch: etag }),
  deleteRule: (name: string, etag: string | undefined) =>
    api.delete('/alerts/rules/{rule}', { path: { rule: name }, ifMatch: etag }),
  setRuleEnabled: (name: string, enabled: boolean, etag: string | undefined) =>
    api.patch('/alerts/rules/{rule}', { path: { rule: name }, body: { spec: { enabled } }, ifMatch: etag }),

  channels: (): Promise<AlertChannelResource[]> =>
    listAll((cursor) => api.get('/alerts/channels', { query: { limit: 200, cursor } })),
  createChannel: (name: string, spec: ChannelSpec) => api.post('/alerts/channels', { body: { name, spec } }),
  replaceChannel: (name: string, spec: ChannelSpec, etag: string | undefined) =>
    api.put('/alerts/channels/{channel}', { path: { channel: name }, body: { spec }, ifMatch: etag }),
  deleteChannel: (name: string, etag: string | undefined) =>
    api.delete('/alerts/channels/{channel}', { path: { channel: name }, ifMatch: etag }),
  testChannel: (name: string): Promise<TestResult> =>
    api.post('/alerts/channels/{channel}:test', { path: { channel: name } }),
  channelStatus: (): Promise<DeliveryStatus[]> =>
    listAll((cursor) => api.get('/runtime/alerts/channels', { query: { limit: 200, cursor } })),

  /** The event webhooks, read from the observability settings with their ETag. */
  webhooks: async (): Promise<Webhooks> => {
    const r = await api.get('/settings/observability');
    const spec = r.spec as { alerts?: { webhooks?: Record<string, WebhookSpec> } };
    return { items: spec.alerts?.webhooks ?? {}, etag: r.metadata.etag };
  },
  /** Adds, changes (merge patch) or, with `null`, removes event webhooks. */
  patchWebhooks: (changes: Record<string, Partial<WebhookSpec> | null>, etag: string | undefined) =>
    api.patch('/settings/observability', {
      body: { spec: { alerts: { webhooks: changes } } },
      ifMatch: etag,
    }),
  testWebhook: (name: string): Promise<TestResult> =>
    api.post('/alerts/webhooks/{webhook}:test', { path: { webhook: name } }),
  rotateWebhookSecret: (name: string) =>
    api.post('/alerts/webhooks/{webhook}:rotate_secret', {
      path: { webhook: name },
      body: { overlap: '1h' },
    }),
  webhookStatus: (): Promise<DeliveryStatus[]> =>
    listAll((cursor) => api.get('/runtime/alerts/webhooks', { query: { limit: 200, cursor } })),

  slos: async (): Promise<SloStatus[]> => (await api.get('/runtime/slos')).items,
  /** The gateway's availability per day over the objective's window, for the burn line. */
  availabilityByDay: async (days: number): Promise<AvailabilityRow[]> =>
    (
      await api.get('/reports/availability', {
        query: {
          from: new Date(Date.now() - days * DAY_MS).toISOString(),
          to: new Date().toISOString(),
          interval: '1d',
          group_by: 'gateway',
        },
      })
    ).items,

  /**
   * Usage for a rule preview: the rule's scope as filters, by status class for the 5xx rate. The
   * gateway answers at most 1,440 buckets per read, so a long range is read in parts.
   */
  previewUsage: async (opts: {
    from: number;
    to: number;
    interval: '5m' | '15m' | '1h';
    scope: RuleSpec['scope'];
    byStatus: boolean;
    signal?: AbortSignal;
  }): Promise<UsageBucket[]> => {
    const filters: Record<string, string[]> = {};
    for (const dim of USAGE_SCOPE) {
      const v = opts.scope?.[dim];
      if (v) filters[dim] = [v];
    }
    const step = { '5m': 300_000, '15m': 900_000, '1h': 3_600_000 }[opts.interval];
    const chunk = 1440 * step;
    const out: UsageBucket[] = [];
    for (let start = Math.floor(opts.from / step) * step; start < opts.to; start += chunk) {
      const end = Math.min(opts.to, start + chunk);
      out.push(
        ...(await listAll(
          (cursor) =>
            api.get('/analytics/usage', {
              query: {
                from: new Date(start).toISOString(),
                to: new Date(end).toISOString(),
                interval: opts.interval,
                limit: 1000,
                ...(cursor ? { cursor } : {}),
                ...(opts.byStatus ? { group_by: ['status_class' as const] } : {}),
                ...filters,
              },
              ...(opts.signal ? { signal: opts.signal } : {}),
            }),
          20,
        )),
      );
    }
    return out;
  },
};
