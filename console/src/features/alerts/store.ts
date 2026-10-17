import { create } from 'zustand';

import type { Alert, AlertChannelResource, AlertRuleResource, UsageBucket } from '@/lib/api/types';
import { asApiError, emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  alertsApi,
  type AvailabilityRow,
  type ChannelSpec,
  type DeliveryStatus,
  type RuleSpec,
  type SloStatus,
  type TestResult,
  type Webhooks,
  type WebhookSpec,
} from './api';

/** Days of instances read for the 30-day figures and the 30 days before them. */
export const HISTORY_DAYS = 60;
/** The history is read again at most this often by the page's polling. */
const HISTORY_REFRESH_MS = 5 * 60_000;

type Action = 'acknowledge' | 'resolve' | 'escalate' | 'unsnooze' | { snooze: string } | { note: string };

interface AlertsState {
  alerts: Slot<Alert[]>;
  history: Slot<Alert[]>;
  rules: Slot<AlertRuleResource[]>;
  channels: Slot<AlertChannelResource[]>;
  channelStatus: Slot<DeliveryStatus[]>;
  webhooks: Slot<Webhooks>;
  webhookStatus: Slot<DeliveryStatus[]>;
  slos: Slot<SloStatus[]>;
  availability: Slot<AvailabilityRow[]>;
  signals: Record<string, Slot<UsageBucket[]>>;
  similar: Record<string, Slot<Alert[]>>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  loadSignal: (id: string) => Promise<void>;
  loadSimilar: (id: string) => Promise<void>;
  act: (id: string, action: Action) => Promise<void>;
  setRuleEnabled: (name: string, enabled: boolean) => Promise<void>;
  saveRule: (name: string, spec: RuleSpec, existing: AlertRuleResource | null) => Promise<void>;
  deleteRule: (rule: AlertRuleResource) => Promise<void>;
  saveChannel: (name: string, spec: ChannelSpec, existing: AlertChannelResource | null) => Promise<void>;
  deleteChannel: (channel: AlertChannelResource) => Promise<void>;
  testChannel: (name: string) => Promise<TestResult>;
  saveWebhook: (name: string, spec: Partial<WebhookSpec>) => Promise<void>;
  deleteWebhook: (name: string) => Promise<void>;
  testWebhook: (name: string) => Promise<TestResult>;
}

/** Puts an updated instance in both lists. */
function replaceIn(list: Alert[] | null, updated: Alert): Alert[] | null {
  return list ? list.map((a) => (a.id === updated.id ? updated : a)) : list;
}

/**
 * Alert instances (the inbox and 60 days of history), rules, channels, event webhooks, delivery
 * status and objectives, with the actions on an instance and the editors' writes.
 */
export const useAlerts = create<AlertsState>()((set, get) => {
  const keyed = async (
    key: 'signals' | 'similar',
    id: string,
    fn: () => Promise<Alert[] | UsageBucket[]>,
  ): Promise<void> => {
    set((s) => ({ [key]: { ...s[key], [id]: { ...(s[key][id] ?? emptySlot()), status: 'loading' } } }));
    try {
      const data = await fn();
      set((s) => ({ [key]: { ...s[key], [id]: { data, status: 'ready', error: null, at: Date.now() } } }));
    } catch (error) {
      set((s) => ({
        [key]: { ...s[key], [id]: { data: null, status: 'error', error: asApiError(error), at: null } },
      }));
    }
  };
  const reload = (key: 'rules' | 'channels' | 'webhooks' | 'channelStatus' | 'webhookStatus') => {
    const fns = {
      rules: alertsApi.rules,
      channels: alertsApi.channels,
      webhooks: alertsApi.webhooks,
      channelStatus: alertsApi.channelStatus,
      webhookStatus: alertsApi.webhookStatus,
    };
    return loadSlot(set, get, key, fns[key] as never, { quiet: true });
  };
  return {
    alerts: emptySlot(),
    history: emptySlot(),
    rules: emptySlot(),
    channels: emptySlot(),
    channelStatus: emptySlot(),
    webhooks: emptySlot(),
    webhookStatus: emptySlot(),
    slos: emptySlot(),
    availability: emptySlot(),
    signals: {},
    similar: {},

    async load(options) {
      const history = get().history;
      const stale = !history.at || Date.now() - history.at > HISTORY_REFRESH_MS || !options?.quiet;
      await Promise.all([
        loadSlot(set, get, 'alerts', alertsApi.list, options),
        loadSlot(set, get, 'rules', alertsApi.rules, options),
        loadSlot(set, get, 'channels', alertsApi.channels, options),
        loadSlot(set, get, 'channelStatus', alertsApi.channelStatus, options),
        loadSlot(set, get, 'webhookStatus', alertsApi.webhookStatus, options),
        loadSlot(set, get, 'webhooks', alertsApi.webhooks, options),
        loadSlot(set, get, 'slos', alertsApi.slos, options),
        stale ? loadSlot(set, get, 'history', () => alertsApi.history(HISTORY_DAYS), options) : null,
      ]);
      const slo = get().slos.data?.find((s) => s.kind === 'availability');
      const days = slo ? Math.max(1, Math.min(90, Math.round(parseDays(slo.window)))) : 0;
      if (days && stale) {
        await loadSlot(set, get, 'availability', () => alertsApi.availabilityByDay(days), options);
      }
    },

    loadSignal: (id) => keyed('signals', id, () => alertsApi.signal(id)),
    loadSimilar: (id) => keyed('similar', id, () => alertsApi.similar(id)),

    async act(id, action) {
      const updated =
        action === 'acknowledge'
          ? await alertsApi.acknowledge(id)
          : action === 'resolve'
            ? await alertsApi.resolve(id)
            : action === 'escalate'
              ? await alertsApi.escalate(id)
              : action === 'unsnooze'
                ? await alertsApi.unsnooze(id)
                : 'snooze' in action
                  ? await alertsApi.snooze(id, action.snooze)
                  : await alertsApi.note(id, action.note);
      set((s) => ({
        alerts: { ...s.alerts, data: replaceIn(s.alerts.data, updated) },
        history: { ...s.history, data: replaceIn(s.history.data, updated) },
      }));
    },

    async setRuleEnabled(name, enabled) {
      const rule = get().rules.data?.find((r) => r.name === name);
      await alertsApi.setRuleEnabled(name, enabled, rule?.metadata.etag);
      await reload('rules');
    },

    async saveRule(name, spec, existing) {
      if (existing) await alertsApi.replaceRule(name, spec, existing.metadata.etag);
      else await alertsApi.createRule(name, spec);
      await reload('rules');
    },

    async deleteRule(rule) {
      await alertsApi.deleteRule(rule.name, rule.metadata.etag);
      await reload('rules');
    },

    async saveChannel(name, spec, existing) {
      if (existing) await alertsApi.replaceChannel(name, spec, existing.metadata.etag);
      else await alertsApi.createChannel(name, spec);
      await Promise.all([reload('channels'), reload('channelStatus')]);
    },

    async deleteChannel(channel) {
      await alertsApi.deleteChannel(channel.name, channel.metadata.etag);
      await reload('channels');
    },

    async testChannel(name) {
      const result = await alertsApi.testChannel(name);
      await reload('channelStatus');
      return result;
    },

    async saveWebhook(name, spec) {
      const current = await alertsApi.webhooks();
      await alertsApi.patchWebhooks({ [name]: spec }, current.etag);
      await Promise.all([reload('webhooks'), reload('webhookStatus')]);
    },

    async deleteWebhook(name) {
      const current = await alertsApi.webhooks();
      await alertsApi.patchWebhooks({ [name]: null }, current.etag);
      await reload('webhooks');
    },

    async testWebhook(name) {
      const result = await alertsApi.testWebhook(name);
      await reload('webhookStatus');
      return result;
    },
  };
});

/** "30d" → 30; anything unreadable → 30. */
function parseDays(window: string): number {
  const m = /^(\d+)\s*(d|h|w)$/.exec(window.trim());
  if (!m) return 30;
  const n = Number(m[1]);
  return m[2] === 'h' ? n / 24 : m[2] === 'w' ? n * 7 : n;
}

export function severityTone(severity: string): 'error' | 'warn' | 'info' {
  return severity === 'critical' ? 'error' : severity === 'warning' ? 'warn' : 'info';
}
