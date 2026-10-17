import type { HttpHandler } from 'msw';

import type { components } from '@/lib/api/schema';

import {
  alertChannels,
  alertHistory,
  alertRules,
  alerts,
  anomalies,
  globalRoute,
  guardrailRuntime,
  guardrails,
  loginOptions,
  noise,
  safetyEvents,
  tiers,
  usageBuckets,
} from './fixtures';
import { mock } from './http';

type S = components['schemas'];

const now = () => new Date().toISOString();
const ago = (ms: number) => new Date(Date.now() - ms).toISOString();
const meta = (generation: number): S['ResourceMetadata'] => ({
  etag: `"m${generation}"`,
  generation,
  created_at: now(),
  updated_at: now(),
  managed_by: 'api',
});

/** In-memory state the contract-only mocks keep between calls, so writes show up on the next read. */
function createState() {
  return {
    alerts: alerts(),
    history: alertHistory(),
    rules: alertRules(),
    channels: alertChannels(),
    events: safetyEvents(),
    anomalies: anomalies(),
    preferences: {} as S['Preferences'],
    pushSubscriptions: [
      {
        id: 'ps_laptop',
        origin: 'https://fcm.googleapis.com',
        device: 'Chrome on macOS',
        created_at: now(),
        last_success_at: now(),
        last_error: null,
      },
    ] as S['PushSubscription'][],
    tokens: [] as S['PersonalToken'][],
    sessions: [
      {
        id: 's-current',
        created_at: now(),
        last_seen_at: now(),
        expires_at: new Date(Date.now() + 12 * 3_600_000).toISOString(),
        client_ip: '127.0.0.1',
        user_agent: 'This browser',
        method: 'token',
        mfa: false,
        trusted: false,
        current: true,
      },
      {
        id: 's-laptop',
        created_at: now(),
        last_seen_at: now(),
        expires_at: now(),
        client_ip: '10.0.4.12',
        user_agent: 'Firefox on macOS',
        method: 'oidc',
        mfa: true,
        trusted: true,
        current: false,
      },
    ] as S['Session'][],
  };
}

let state = createState();

/** Resets the mocks' in-memory state (between tests). */
export function resetMockState(): void {
  state = createState();
}

function alertById(id: string): S['Alert'] {
  const found = state.alerts.find((a) => a.id === id);
  if (!found) throw new Error(`no alert ${id}`);
  return found;
}

function updateAlert(id: string, patch: Partial<S['Alert']>, entry: S['AlertTimelineEntry']): S['Alert'] {
  const alert = alertById(id);
  const next = { ...alert, ...patch, timeline: [...(alert.timeline ?? []), entry] };
  state.alerts = state.alerts.map((a) => (a.id === id ? next : a));
  return next;
}

/**
 * Handlers for contract operations the development mock answers itself. They run only in
 * development (`pnpm dev:mock`) and in tests; the production build contains none of this.
 */
export const contractHandlers: HttpHandler[] = [
  mock('get', '/analytics/usage', ({ query }) => ({ items: usageBuckets(query), next_cursor: null })),
  mock('get', '/reports/provider_pressure', ({ query }) => {
    const to = query.get('to') ? new Date(query.get('to') ?? '') : new Date();
    const from = query.get('from') ? new Date(query.get('from') ?? '') : new Date(to.getTime() - 86_400_000);
    const step = Math.max(60_000, Math.ceil((to.getTime() - from.getTime()) / 48 / 60_000) * 60_000);
    const starts = Array.from(
      { length: Math.max(1, Math.floor((to.getTime() - from.getTime()) / step)) },
      (_, i) => new Date(from.getTime() + i * step).toISOString(),
    );
    const providers: [string, number, number][] = [
      ['anthropic', 0.62, 300],
      ['openai', 0.45, 600],
      ['bedrock', 0.3, 800],
    ];
    const items = providers.map(([provider, base, limit], p) => ({
      provider,
      buckets: starts.map((start, i) => {
        const v = Math.min(1, Math.max(0.05, base + 0.25 * Math.sin((i + p * 5) / 6) + 0.05 * noise(i + p)));
        return {
          start,
          rpm_utilization: v,
          tpm_utilization: v * 0.7,
          requests_limit: limit,
          tokens_limit: limit * 1000,
        };
      }),
    }));
    const hot = starts.map((start, i) => ({
      start,
      providers: items.filter((it) => (it.buckets[i]?.rpm_utilization ?? 0) >= 0.8).length,
    }));
    return {
      from: from.toISOString(),
      to: to.toISOString(),
      interval: query.get('interval'),
      threshold: 0.8,
      items,
      hot,
    };
  }),
  mock('get', '/analytics/latency_histogram', () => {
    const bounds = Array.from({ length: 12 }, (_, i) => Math.round(100 * 2 ** (i / 2)));
    return {
      bounds,
      counts: bounds.map((_, i) => Math.round(400 * Math.exp(-((i - 4) ** 2) / 6))),
      p50: 420,
      p95: 1620,
      p99: 2900,
    };
  }),
  mock('get', '/reports/availability', () => ({
    items: [{ group: { gateway: 'gateway' }, requests: 1_200_000, availability: 0.99982 }],
    next_cursor: null,
  })),
  mock('get', '/alerts', ({ query }) => {
    const statuses = query.getAll('status');
    const from = query.get('from');
    const pool = from ? [...state.alerts, ...state.history].filter((a) => a.fired_at >= from) : state.alerts;
    return {
      items: pool.filter((a) => statuses.length === 0 || statuses.includes(a.status)),
      next_cursor: null,
    };
  }),
  mock('get', '/alerts/rules', () => ({ items: state.rules, next_cursor: null })),
  mock(
    'post',
    '/alerts/rules',
    ({ body }) => {
      const input = body as S['AlertRuleCreate'];
      const created = { name: input.name, spec: input.spec, metadata: meta(1) };
      state.rules = [...state.rules, created].sort((a, b) => a.name.localeCompare(b.name));
      return created;
    },
    201,
  ),
  mock('put', '/alerts/rules/{rule}', ({ params, body }) => {
    const input = body as S['AlertRuleReplace'];
    const name = params.rule ?? '';
    const was = state.rules.find((r) => r.name === name);
    const next = { name, spec: input.spec, metadata: meta((was?.metadata.generation ?? 0) + 1) };
    state.rules = was ? state.rules.map((r) => (r.name === name ? next : r)) : [...state.rules, next];
    return next;
  }),
  mock('patch', '/alerts/rules/{rule}', ({ params, body }) => {
    const patch = (body as { spec?: Partial<S['AlertRuleResource']['spec']> } | undefined)?.spec ?? {};
    const was = state.rules.find((r) => r.name === params.rule);
    if (!was) throw new Error('no rule');
    const next = { ...was, spec: { ...was.spec, ...patch }, metadata: meta(was.metadata.generation + 1) };
    state.rules = state.rules.map((r) => (r.name === was.name ? next : r));
    return next;
  }),
  mock(
    'delete',
    '/alerts/rules/{rule}',
    ({ params }) => {
      state.rules = state.rules.filter((r) => r.name !== params.rule);
      return undefined;
    },
    204,
  ),
  mock('get', '/alerts/channels', () => ({ items: state.channels, next_cursor: null })),
  mock(
    'post',
    '/alerts/channels',
    ({ body }) => {
      const input = body as S['AlertChannelCreate'];
      const created = { name: input.name, spec: input.spec, metadata: meta(1) };
      state.channels = [...state.channels, created].sort((a, b) => a.name.localeCompare(b.name));
      return created;
    },
    201,
  ),
  mock('put', '/alerts/channels/{channel}', ({ params, body }) => {
    const input = body as S['AlertChannelReplace'];
    const name = params.channel ?? '';
    const was = state.channels.find((c) => c.name === name);
    const next = {
      name,
      spec: input.spec,
      metadata: {
        ...meta((was?.metadata.generation ?? 0) + 1),
        ...(was?.metadata.secret_hints ? { secret_hints: was.metadata.secret_hints } : {}),
      },
    };
    state.channels = was
      ? state.channels.map((c) => (c.name === name ? next : c))
      : [...state.channels, next];
    return next;
  }),
  mock(
    'delete',
    '/alerts/channels/{channel}',
    ({ params }) => {
      state.channels = state.channels.filter((c) => c.name !== params.channel);
      return undefined;
    },
    204,
  ),
  mock('post', '/alerts/channels/{channel}:test', () => ({
    ok: true,
    status_code: 200,
    latency_ms: 182,
    error: null,
  })),
  mock('get', '/runtime/alerts/channels', () => ({
    items: state.channels.map((c, i) => ({
      name: c.name,
      enabled: c.spec.enabled !== false,
      last_delivery_at: i === 2 ? null : ago((i + 1) * 7 * 60_000),
      last_status: i === 2 ? null : i === 1 ? 502 : 200,
      deliveries_24h: i === 2 ? 0 : 14 - i * 5,
      failures_24h: i === 1 ? 2 : 0,
      queued: 0,
    })),
    next_cursor: null,
  })),
  mock('get', '/reports/alerts/{alert_id}/similar', ({ params }) => {
    const alert = [...state.alerts, ...state.history].find((a) => a.id === params.alert_id);
    return {
      items: [...state.alerts, ...state.history]
        .filter((a) => a.fingerprint === alert?.fingerprint)
        .sort((a, b) => b.fired_at.localeCompare(a.fired_at)),
      next_cursor: null,
    };
  }),
  mock('post', '/alerts/{alert_id}:unsnooze', ({ params }) =>
    updateAlert(
      params.alert_id ?? '',
      { snoozed_until: null },
      { time: now(), kind: 'snoozed', actor: 'you', detail: { until: null } },
    ),
  ),
  mock('get', '/alerts/{alert_id}', ({ params }) => alertById(params.alert_id ?? '')),
  mock('post', '/alerts/{alert_id}:acknowledge', ({ params }) =>
    updateAlert(
      params.alert_id ?? '',
      { status: 'acknowledged', acknowledged_at: now(), acknowledged_by: 'you' },
      { time: now(), kind: 'acknowledged', actor: 'you' },
    ),
  ),
  mock('post', '/alerts/{alert_id}:resolve', ({ params }) =>
    updateAlert(
      params.alert_id ?? '',
      { status: 'resolved', resolved_at: now(), resolved_by: 'you' },
      { time: now(), kind: 'resolved', actor: 'you' },
    ),
  ),
  mock('post', '/alerts/{alert_id}:snooze', ({ params, body }) => {
    const minutes =
      Number(((body as { duration?: string } | undefined)?.duration ?? '60m').replace(/\D/g, '')) || 60;
    return updateAlert(
      params.alert_id ?? '',
      { snoozed_until: new Date(Date.now() + minutes * 60_000).toISOString() },
      { time: now(), kind: 'snoozed', actor: 'you' },
    );
  }),
  mock('post', '/alerts/{alert_id}:note', ({ params, body }) =>
    updateAlert(
      params.alert_id ?? '',
      {},
      {
        time: now(),
        kind: 'note',
        actor: 'you',
        detail: { text: (body as { text?: string } | undefined)?.text ?? '' },
      },
    ),
  ),
  mock('post', '/alerts/{alert_id}:escalate', ({ params }) =>
    updateAlert(params.alert_id ?? '', {}, { time: now(), kind: 'escalated', actor: 'you' }),
  ),
  mock('get', '/reports/alerts/{alert_id}/signal', ({ params }) => {
    const alert = alertById(params.alert_id ?? '');
    const fired = new Date(alert.fired_at).getTime();
    const items = Array.from({ length: 40 }, (_, i) => {
      const t = fired - 30 * 60_000 + i * 60_000;
      const rising = t >= fired ? 1.2 + (i - 30) * 0.12 : 0.5 + 0.2 * noise(i);
      const p95 = 1500 * rising;
      return {
        start: new Date(t).toISOString(),
        group: {},
        requests: 40 + Math.round(10 * noise(i + 3)),
        errors: t >= fired ? 3 : 0,
        cost_usd: '0.40',
        latency_ms: { p50: p95 / 2.6, p95, p99: p95 * 1.6 },
      };
    });
    return { items, next_cursor: null };
  }),
  mock('get', '/runtime/alerts/webhooks', () => ({
    items: [
      {
        name: 'ops-events',
        enabled: true,
        last_delivery_at: ago(4 * 60_000),
        last_status: 200,
        deliveries_24h: 31,
        failures_24h: 1,
        queued: 0,
      },
    ],
    next_cursor: null,
  })),
  mock('post', '/alerts/webhooks/{webhook}:test', () => ({
    ok: true,
    status_code: 204,
    latency_ms: 96,
    error: null,
  })),
  mock('get', '/runtime/slos', () => ({
    items: [
      {
        name: 'availability',
        kind: 'availability' as const,
        objective: 0.999,
        attained: 0.9994,
        error_budget_remaining: 0.67,
        burn_rate_1h: 0.8,
        window: '30d',
      },
    ],
    next_cursor: null,
  })),
  mock('get', '/guardrails/events', () => ({ items: state.events, next_cursor: null })),
  mock('get', '/guardrails/events/{event_id}', ({ params }) => {
    const e = state.events.find((x) => x.id === params.event_id);
    if (!e) throw new Error('no event');
    return e;
  }),
  mock('post', '/guardrails/events/{event_id}:review', ({ params, body }) => {
    const review = (body as { state?: S['SafetyEvent']['review_state'] } | undefined)?.state ?? 'reviewed';
    state.events = state.events.map((e) =>
      e.id === params.event_id ? { ...e, review_state: review, reviewed_by: 'you', reviewed_at: now() } : e,
    );
    const e = state.events.find((x) => x.id === params.event_id);
    if (!e) throw new Error('no event');
    return e;
  }),
  mock('get', '/guardrails', () => ({ items: guardrails(), next_cursor: null })),
  mock('get', '/runtime/guardrails', () => ({ items: guardrailRuntime(), next_cursor: null })),
  mock('get', '/reports/guardrails/summary', () => ({
    items: ['pii', 'prompt_injection', 'secret', 'toxic', 'custom', 'routing_block'].map((c, i) => ({
      group: { category: c },
      events: 40 - i * 6,
    })),
    next_cursor: null,
  })),
  mock('get', '/routing/tiers', () => ({ items: tiers(), next_cursor: null })),
  mock('get', '/catalog', () => ({
    bundled: { version: '2026-10-01', price_version: '2026-09-01', entries: 42 },
    runtime: {
      mode: 'extend' as const,
      version: '2026-10-12',
      entries: 3,
      updated_at: new Date(Date.now() - 3 * 86_400_000).toISOString(),
      generation: 12,
    },
    items: [],
  })),
  mock('post', '/auth/device:check', ({ body }) => ({
    user_code: (body as { user_code?: string } | undefined)?.user_code ?? 'BCDF-GHJK',
    status: 'pending' as const,
    requested_at: new Date(Date.now() - 20_000).toISOString(),
    expires_at: new Date(Date.now() + 580_000).toISOString(),
    client_id: 'antirion-cli',
    client_ip: '203.0.113.7',
    organization: null,
    sso: false,
  })),
  mock('post', '/routes/{name}:simulate', () => ({
    evaluated: 18_420,
    matched_by_rule: [
      { path: 'rules[0]', name: 'block-confidential', count: 112 },
      { path: 'rules[1]', name: 'copilot-least-busy', count: 3_210 },
      { path: 'rules[2]', name: 'long-context', count: 488, unknown: 12 },
      { path: 'default', name: null, count: 14_610 },
    ],
    by_leaf: [
      { path: 'default/0', deployment: 'openai-gpt-4o', count: 11_690 },
      { path: 'default/1', deployment: 'anthropic-claude-sonnet', count: 2_920 },
    ],
    blocked: 112,
    no_route: 0,
  })),
  mock('get', '/anomalies', () => ({
    items: state.anomalies.filter((a) => a.state === 'open'),
    next_cursor: null,
  })),
  mock('post', '/anomalies/{anomaly_id}:dismiss', ({ params }) => {
    state.anomalies = state.anomalies.map((a) =>
      a.id === params.anomaly_id ? { ...a, state: 'dismissed' } : a,
    );
    const a = state.anomalies.find((x) => x.id === params.anomaly_id);
    if (!a) throw new Error('no anomaly');
    return a;
  }),
  mock('get', '/runtime/budgets/events', () => ({
    items: [
      {
        time: now(),
        kind: 'threshold_crossed' as const,
        level: 'team',
        owner: 'support-ai',
        name: 'monthly',
        actor: null,
        detail: { ratio: 0.8 },
      },
      {
        time: now(),
        kind: 'adjusted' as const,
        level: 'team',
        owner: 'platform',
        name: 'monthly',
        actor: 'ada',
        detail: {},
      },
    ],
    next_cursor: null,
  })),
  mock('get', '/budgets/overage_requests', () => ({ items: [], next_cursor: null })),
  mock('get', '/cache/entries', () => ({
    items: Array.from({ length: 8 }, (_, i) => ({
      key: `ck${(0x4e7b5969 + i * 7919).toString(16)}${'0'.repeat(24)}`,
      mode: i % 3 === 0 ? ('semantic' as const) : ('exact' as const),
      model: ['gpt-4o', 'claude-sonnet', 'gpt-4o-mini'][i % 3] ?? 'gpt-4o',
      deployment: 'openai-gpt-4o',
      scope: 'project',
      size_bytes: 2048 + i * 911,
      hits: 120 - i * 13,
      created_at: now(),
      expires_at: new Date(Date.now() + (i + 1) * 900_000).toISOString(),
      last_hit_at: now(),
      similarity_threshold: i % 3 === 0 ? 0.95 : null,
    })),
    next_cursor: null,
    enumerable: true,
  })),
  mock('get', '/cache/invalidation_rules', () => ({
    items: [
      {
        name: 'flush-on-disable',
        spec: {
          enabled: true,
          trigger: { event: 'model_disabled' as const },
          action: { drop: 'model' as const },
        },
        metadata: { etag: '"i1"', generation: 2, created_at: now(), updated_at: now(), managed_by: 'api' },
      },
      {
        name: 'nightly-refresh',
        spec: {
          enabled: false,
          trigger: { event: 'schedule' as const, schedule: '0 3 * * *' },
          action: { drop: 'older_than' as const, older_than: '24h' },
        },
        metadata: { etag: '"i2"', generation: 3, created_at: now(), updated_at: now(), managed_by: 'api' },
      },
    ],
    next_cursor: null,
  })),
  mock('get', '/logs/views', () => ({ items: [], next_cursor: null })),
  mock('get', '/auth/scim/status', () => ({
    enabled: false,
    users: 0,
    groups: 0,
    last_request_at: null,
    last_error: null,
  })),
  mock('get', '/runtime/audit/sinks', () => ({ items: [], next_cursor: null })),
];

/**
 * Demo data for endpoints the gateway serves but an empty test gateway has no data for
 * (a shared route with rules), so `pnpm dev:mock` shows every screen populated.
 */
export const demoHandlers: HttpHandler[] = [
  mock('get', '/routes', () => ({ items: [globalRoute()], next_cursor: null })),
  mock('get', '/routes/{name}', () => globalRoute()),
];

/**
 * The operator-identity operations. A gateway with the console's identity endpoints serves them,
 * so only the unit tests use these.
 */
export const identityHandlers: HttpHandler[] = [
  mock('get', '/auth/login_options', ({ query }) => loginOptions(query.get('organization'))),
  mock('get', '/auth/me/preferences', () => state.preferences),
  mock('put', '/auth/me/preferences', ({ body }) => {
    state.preferences = body ?? {};
    return state.preferences;
  }),
  mock('get', '/auth/me/push', () => ({
    public_key: 'BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4',
    subscriptions: state.pushSubscriptions,
  })),
  mock('post', '/auth/me/push:test', () => ({
    results: state.pushSubscriptions.map((s) => ({
      id: s.id,
      origin: s.origin,
      ok: true,
      status: 201,
      error: null,
    })),
  })),
  mock(
    'delete',
    '/auth/me/push/subscriptions/{subscription_id}',
    ({ params }) => {
      state.pushSubscriptions = state.pushSubscriptions.filter((s) => s.id !== params.subscription_id);
      return undefined;
    },
    204,
  ),
  mock('get', '/auth/sessions', () => ({ items: state.sessions, next_cursor: null })),
  mock('get', '/auth/me/tokens', () => ({ items: state.tokens, next_cursor: null })),
  mock(
    'post',
    '/auth/me/tokens',
    ({ body }) => {
      const input = (body ?? {}) as S['PersonalTokenCreate'];
      const created: S['PersonalTokenCreated'] = {
        kid: `pt${Date.now().toString(36)}`,
        description: input.description ?? '',
        access: input.access ?? 'full',
        created_at: now(),
        expires_at: input.expires_at,
        last_used_at: null,
        token: `ak-antirion-mock-${Math.random().toString(36).slice(2)}`,
      };
      const { token: _token, ...listed } = created;
      state.tokens = [...state.tokens, listed];
      return created;
    },
    201,
  ),
  mock('get', '/auth/me/mfa', () => ({ items: [], recovery_codes_remaining: 0 })),
  mock('get', '/invitations', () => ({ items: [], next_cursor: null })),
];
