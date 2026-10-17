import type { components } from '@/lib/api/schema';

type S = components['schemas'];

/** A deterministic pseudo-random number in [0, 1) for a seed, so mock charts are stable. */
export function noise(seed: number): number {
  const x = Math.sin(seed * 12.9898 + 78.233) * 43758.5453;
  return x - Math.floor(x);
}

const SECONDS: Record<string, number> = {
  '1m': 60,
  '5m': 300,
  '15m': 900,
  '1h': 3600,
  hour: 3600,
  '6h': 21_600,
  '1d': 86_400,
  day: 86_400,
};

export const MODELS = [
  'gpt-4o',
  'gpt-4o-mini',
  'claude-sonnet',
  'claude-haiku',
  'mistral-large',
  'text-embedding-3-large',
];
export const PROVIDERS = ['openai', 'anthropic', 'mistral', 'bedrock'];
export const TEAMS = ['platform', 'support-ai', 'data-science', 'search'];
export const ENVIRONMENTS = ['prod', 'staging', 'dev'];

const DIMENSION: Record<string, string[]> = {
  model: MODELS,
  requested_model: MODELS,
  provider: PROVIDERS,
  team: TEAMS,
  environment: ENVIRONMENTS,
  key: ['ide-backend', 'support-bot', 'batch-eval', 'ci-pipeline'],
  status_class: ['2xx', '4xx', '5xx'],
  organization: ['default'],
};

/** Usage buckets for `[from, to)` at `interval`, optionally grouped, shaped like a working day. */
export function usageBuckets(query: URLSearchParams): S['UsageBucket'][] {
  const to = new Date(query.get('to') ?? Date.now()).getTime();
  const from = new Date(query.get('from') ?? to - 6 * 3_600_000).getTime();
  const interval = query.get('interval');
  const groups = query.getAll('group_by');
  const step =
    (interval ? (SECONDS[interval] ?? 300) : Math.max(60, Math.min(3600, (to - from) / 1000 / 60))) * 1000;
  const starts: number[] = [];
  for (let t = Math.floor(from / step) * step; t < to && starts.length < 1440; t += step) starts.push(t);
  const combos: Record<string, string>[] = groups.reduce<Record<string, string>[]>(
    (acc, g) => acc.flatMap((c) => (DIMENSION[g] ?? ['other']).map((v) => ({ ...c, [g]: v }))),
    [{}],
  );
  const flagged = query.get('flagged') === 'true';
  const out: S['UsageBucket'][] = [];
  for (const start of starts) {
    const hour = new Date(start).getUTCHours();
    const daily = 0.55 + 0.45 * Math.sin(((hour - 6) / 24) * Math.PI * 2);
    combos.forEach((group, gi) => {
      const weight = 1 / (gi + 1.4);
      const base = (step / 60_000) * 40 * daily * weight * (0.8 + 0.4 * noise(start / step + gi));
      const requests = Math.max(0, Math.round(flagged ? base * 0.02 : base));
      const errorRate =
        group.status_class === '5xx'
          ? 1
          : group.status_class === '4xx'
            ? 1
            : group.status_class === '2xx'
              ? 0
              : 0.012 + 0.02 * noise(start + gi);
      const input = requests * Math.round(900 + 600 * noise(start + 7 * gi));
      const output = requests * Math.round(220 + 120 * noise(start + 11 * gi));
      const cost = (input * 2.5 + output * 10) / 1_000_000;
      const p50 = 380 + 300 * noise(start + 3 * gi);
      out.push({
        start: new Date(start).toISOString(),
        group,
        requests,
        errors: Math.round(requests * errorRate),
        input,
        output,
        cost_usd: cost.toFixed(6),
        latency_ms: { p50, p90: p50 * 2.1, p95: p50 * 2.6, p99: p50 * 4.2 },
        ttft_ms: { p50: p50 * 0.4, p95: p50 * 0.9 },
        cache_hits: Math.round(requests * (0.15 + 0.1 * noise(start + 5 * gi))),
        cache_saved_usd: (cost * 0.18).toFixed(6),
        fallbacks: Math.round(requests * 0.01),
        retries: Math.round(requests * 0.02),
        guardrail_blocks: Math.round(requests * 0.002),
        guardrail_redactions: Math.round(requests * 0.006),
        guardrail_flags: Math.round(requests * 0.004),
        unique_keys: 4,
      });
    });
  }
  if (interval) return out;
  const collapsed = new Map<string, S['UsageBucket']>();
  for (const b of out) {
    const k = JSON.stringify(b.group);
    const prev = collapsed.get(k);
    if (!prev) {
      collapsed.set(k, { ...b, start: new Date(from).toISOString() });
      continue;
    }
    collapsed.set(k, {
      ...prev,
      requests: prev.requests + b.requests,
      errors: prev.errors + b.errors,
      input: (prev.input ?? 0) + (b.input ?? 0),
      output: (prev.output ?? 0) + (b.output ?? 0),
      cost_usd: (Number(prev.cost_usd) + Number(b.cost_usd)).toFixed(6),
      cache_hits: (prev.cache_hits ?? 0) + (b.cache_hits ?? 0),
      cache_saved_usd: (Number(prev.cache_saved_usd ?? 0) + Number(b.cache_saved_usd ?? 0)).toFixed(6),
      fallbacks: (prev.fallbacks ?? 0) + (b.fallbacks ?? 0),
      guardrail_blocks: (prev.guardrail_blocks ?? 0) + (b.guardrail_blocks ?? 0),
      guardrail_redactions: (prev.guardrail_redactions ?? 0) + (b.guardrail_redactions ?? 0),
      guardrail_flags: (prev.guardrail_flags ?? 0) + (b.guardrail_flags ?? 0),
    });
  }
  return [...collapsed.values()];
}

const HOUR = 3_600_000;
const ago = (ms: number) => new Date(Date.now() - ms).toISOString();

export function alerts(): S['Alert'][] {
  return [
    {
      id: 'alr-8821',
      rule: 'provider-p95',
      fingerprint: 'openai.gpt-4o.latency',
      category: 'latency',
      severity: 'critical',
      status: 'firing',
      fired_at: ago(3 * 60_000),
      summary: 'gpt-4o p95 latency 3.2× its baseline',
      scope: { provider: 'openai', model: 'gpt-4o' },
      value: 3812,
      threshold: 1500,
      owner_team: 'platform',
      runbook_url: 'https://runbooks.example/latency',
      impact: { requests: 847, errors: 28, throttled: 0 },
      timeline: [
        { time: ago(3 * 60_000), kind: 'fired', actor: 'monitor', detail: { value: 3812 } },
        {
          time: ago(3 * 60_000 - 2000),
          kind: 'notified',
          actor: 'monitor',
          detail: { channel: 'oncall-slack' },
        },
      ],
    },
    {
      id: 'alr-8819',
      rule: 'rpm-capacity',
      fingerprint: 'bedrock.rpm',
      category: 'capacity',
      severity: 'warning',
      status: 'firing',
      fired_at: ago(7 * 60_000),
      summary: 'bedrock at 96% of its requests-per-minute ceiling',
      scope: { provider: 'bedrock' },
      value: 0.96,
      threshold: 0.95,
      owner_team: 'platform',
      impact: { requests: 1204, errors: 0, throttled: 12 },
      timeline: [{ time: ago(7 * 60_000), kind: 'fired', actor: 'monitor' }],
    },
    {
      id: 'alr-8816',
      rule: 'budget-projection',
      fingerprint: 'team.support-ai.monthly',
      category: 'budget',
      severity: 'warning',
      status: 'acknowledged',
      fired_at: ago(25 * 60_000),
      acknowledged_at: ago(20 * 60_000),
      acknowledged_by: 'ada',
      summary: 'support-ai projected 18% over its monthly budget',
      scope: { team: 'support-ai' },
      value: 1.18,
      threshold: 1,
      owner_team: 'support-ai',
      timeline: [
        { time: ago(25 * 60_000), kind: 'fired', actor: 'monitor' },
        { time: ago(20 * 60_000), kind: 'acknowledged', actor: 'ada' },
      ],
    },
    {
      id: 'alr-8795',
      rule: 'pii-flags',
      fingerprint: 'team.search.pii',
      category: 'pii',
      severity: 'info',
      status: 'resolved',
      fired_at: ago(2 * HOUR),
      resolved_at: ago(HOUR),
      resolved_by: 'monitor',
      summary: 'PII guardrail flagged 11 prompts from search',
      scope: { team: 'search' },
      owner_team: 'search',
      timeline: [
        { time: ago(2 * HOUR), kind: 'fired', actor: 'monitor' },
        { time: ago(HOUR), kind: 'resolved', actor: 'monitor' },
      ],
    },
  ];
}

/**
 * Resolved instances of the last 60 days, for the 30-day figures, the rules' fire counts and the
 * similar fires: the fingerprints repeat those of the open alerts.
 */
export function alertHistory(): S['Alert'][] {
  const kinds: [string, string, string, S['Alert']['severity'], string, string[]][] = [
    [
      'provider-p95',
      'openai.gpt-4o.latency',
      'latency',
      'critical',
      'gpt-4o p95 latency 2.9× its baseline',
      ['oncall-slack', 'pager'],
    ],
    [
      'rpm-capacity',
      'bedrock.rpm',
      'capacity',
      'warning',
      'bedrock at 95% of its requests-per-minute ceiling',
      ['oncall-slack'],
    ],
    [
      'budget-projection',
      'team.support-ai.monthly',
      'budget',
      'warning',
      'support-ai projected 9% over its monthly budget',
      ['finance-email'],
    ],
    ['pii-flags', 'team.search.pii', 'pii', 'info', 'PII guardrail flagged 14 prompts from search', []],
  ];
  const out: S['Alert'][] = [];
  for (let i = 0; i < 46; i += 1) {
    const kind = kinds[i % kinds.length];
    if (!kind) continue;
    const [rule, fingerprint, category, severity, summary, channels] = kind;
    const fired = Date.now() - (2 + i * 31 + Math.round(noise(i) * 20)) * HOUR;
    const lasted = noise(i + 7) < 0.12 ? 40_000 : (5 + Math.round(noise(i + 3) * 70)) * 60_000;
    const resolved = fired + lasted;
    const timeline: S['AlertTimelineEntry'][] = [
      { time: new Date(fired).toISOString(), kind: 'fired', actor: 'monitor' },
      ...channels.map((channel, k) => ({
        time: new Date(fired + 2000 + k * 500).toISOString(),
        kind: 'notified' as const,
        actor: 'monitor',
        detail: { channel },
      })),
      ...(i % 5 === 0
        ? [
            {
              time: new Date(fired + lasted / 2).toISOString(),
              kind: 'note' as const,
              actor: 'ada',
              detail: {
                text: i % 2 ? 'Upstream degraded; failover held.' : 'Traffic spike from batch-eval.',
              },
            },
          ]
        : []),
      { time: new Date(resolved).toISOString(), kind: 'resolved', actor: i % 3 ? 'monitor' : 'ada' },
    ];
    out.push({
      id: `alr-${8700 - i}`,
      rule,
      fingerprint,
      category,
      severity,
      status: 'resolved',
      fired_at: new Date(fired).toISOString(),
      resolved_at: new Date(resolved).toISOString(),
      resolved_by: i % 3 ? 'monitor' : 'ada',
      summary,
      scope: {},
      owner_team: 'platform',
      timeline,
    });
  }
  return out;
}

export function alertRules(): S['AlertRuleResource'][] {
  const meta = (g: number): S['ResourceMetadata'] => ({
    etag: `"r${g}"`,
    generation: g,
    created_at: ago(30 * 24 * HOUR),
    updated_at: ago(2 * 24 * HOUR),
    managed_by: 'api',
  });
  return [
    {
      name: 'provider-p95',
      spec: {
        category: 'latency',
        metric: 'latency_baseline_ratio',
        operator: 'gt',
        threshold: 3,
        window: '5m',
        severity: 'critical',
        channels: ['oncall-slack', 'pager'],
        enabled: true,
      },
      metadata: meta(4),
    },
    {
      name: 'rpm-capacity',
      spec: {
        category: 'capacity',
        metric: 'rpm_utilization',
        operator: 'gte',
        threshold: 0.95,
        window: '1m',
        severity: 'warning',
        channels: ['oncall-slack'],
        enabled: true,
      },
      metadata: meta(5),
    },
    {
      name: 'budget-projection',
      spec: {
        category: 'budget',
        metric: 'budget_projection_ratio',
        operator: 'gt',
        threshold: 1,
        window: '1h',
        severity: 'warning',
        channels: ['finance-email'],
        enabled: true,
      },
      metadata: meta(6),
    },
    {
      name: 'pii-flags',
      spec: {
        category: 'pii',
        metric: 'guardrail_events',
        operator: 'gt',
        threshold: 10,
        window: '15m',
        severity: 'info',
        enabled: true,
      },
      metadata: meta(7),
    },
    {
      name: 'cache-drop',
      spec: {
        category: 'cache',
        metric: 'cache_hit_rate',
        operator: 'lt',
        threshold: 0.1,
        window: '1h',
        severity: 'info',
        enabled: false,
      },
      metadata: meta(8),
    },
  ];
}

export function alertChannels(): S['AlertChannelResource'][] {
  const meta: S['ResourceMetadata'] = {
    etag: '"c1"',
    generation: 3,
    created_at: ago(90 * 24 * HOUR),
    updated_at: ago(9 * 24 * HOUR),
    managed_by: 'api',
  };
  return [
    {
      name: 'oncall-slack',
      spec: { type: 'slack', min_severity: 'warning', slack: { webhook_url: '[REDACTED]' } },
      metadata: { ...meta, secret_hints: { 'slack.webhook_url': '…x9Qa' } },
    },
    {
      name: 'pager',
      spec: { type: 'pagerduty', min_severity: 'critical', pagerduty: { routing_key: '[REDACTED]' } },
      metadata: meta,
    },
    {
      name: 'finance-email',
      spec: { type: 'email', email: { to: ['finance@example.com'] } },
      metadata: meta,
    },
  ];
}

const CATEGORIES = ['pii', 'prompt_injection', 'secret', 'toxic', 'custom', 'routing_block'];

export function safetyEvents(): S['SafetyEvent'][] {
  const rows: S['SafetyEvent'][] = [];
  for (let i = 0; i < 24; i += 1) {
    const category = CATEGORIES[i % CATEGORIES.length] ?? 'pii';
    const action = category === 'pii' ? 'redact' : category === 'toxic' ? 'flag' : 'block';
    rows.push({
      id: `sf-${9482 - i}`,
      log_id: `log-${9482 - i}`,
      request_id: `req-${(9482 - i).toString(16)}`,
      time: ago(i * 17 * 60_000 + 60_000),
      guardrail:
        category === 'pii'
          ? 'pii-gateway'
          : category === 'prompt_injection'
            ? 'injection-classifier'
            : category === 'secret'
              ? 'secret-scanner'
              : `${category}-guard`,
      guardrail_type: category === 'pii' ? 'pii' : category === 'secret' ? 'secrets' : 'classifier',
      stage: 'input',
      action,
      category,
      severity: i % 7 === 0 ? 'critical' : i % 3 === 0 ? 'high' : i % 2 === 0 ? 'medium' : 'low',
      confidence: 0.82 + 0.17 * noise(i),
      entities:
        category === 'pii'
          ? [
              { type: 'email', count: 2 },
              { type: 'phone', count: 1 },
            ]
          : [],
      fingerprint: `fp-${category}-${i % 5}`,
      team: TEAMS[i % TEAMS.length] ?? null,
      key: 'support-bot',
      end_user: `user-${100 + i}`,
      model: MODELS[i % MODELS.length] ?? null,
      provider: PROVIDERS[i % PROVIDERS.length] ?? null,
      latency_ms: 4 + 20 * noise(i + 1),
      review_state: i < 3 ? 'open' : i % 4 === 0 ? 'false_positive' : 'reviewed',
    });
  }
  return rows;
}

export function guardrails(): S['GuardrailResource'][] {
  const meta: S['ResourceMetadata'] = {
    etag: '"g1"',
    generation: 2,
    created_at: ago(60 * 24 * HOUR),
    updated_at: ago(4 * 24 * HOUR),
    managed_by: 'api',
  };
  return [
    { name: 'pii-gateway', spec: { type: 'pii', action: 'redact', enabled: true }, metadata: meta },
    { name: 'secret-scanner', spec: { type: 'secrets', action: 'block', enabled: true }, metadata: meta },
    {
      name: 'injection-classifier',
      spec: { type: 'classifier', action: 'block', enabled: true },
      metadata: meta,
    },
    { name: 'toxic-guard', spec: { type: 'moderation', action: 'flag', enabled: true }, metadata: meta },
    { name: 'custom-guard', spec: { type: 'regex', action: 'flag', enabled: false }, metadata: meta },
  ] as S['GuardrailResource'][];
}

export function guardrailRuntime(): S['GuardrailRuntime'][] {
  return guardrails().map((g, i) => ({
    name: g.name,
    type: g.spec.type,
    action: g.spec.action ?? 'block',
    enabled: g.spec.enabled !== false,
    evaluations_24h: 24_000 - i * 1200,
    hits_24h: 180 - i * 30,
    hits_by_hour: Array.from({ length: 24 }, (_, h) => Math.round(8 * noise(h + i * 31))),
    false_positive_rate: 0.02 + 0.03 * noise(i),
  }));
}

export function tiers(): S['Tier'][] {
  return [
    { name: 'frontier', builtin: true, description: 'The most capable Models', models: 1 },
    { name: 'balanced', builtin: true, description: 'Quality at a moderate price', models: 2 },
    { name: 'fast', builtin: true, description: 'Low latency and cost', models: 2 },
    { name: 'specialized', builtin: true, description: 'Embeddings, rerank and other tasks', models: 1 },
  ];
}

export function globalRoute(): S['RouteResource'] {
  return {
    name: 'global',
    spec: {
      tree: {
        type: 'conditional',
        rules: [
          {
            name: 'block-confidential',
            when: { field: 'request.tags', contains: 'confidential' },
            target: {
              type: 'block',
              status: 451,
              message: 'Confidential traffic is not allowed',
              category: 'policy',
            },
          },
          {
            name: 'copilot-least-busy',
            when: { field: 'identity.team', eq: 'support-ai' },
            target: { type: 'least_busy', children: ['openai-gpt-4o-mini', 'anthropic-claude-haiku'] },
          },
          {
            name: 'long-context',
            when: { field: 'request.input_tokens', gt: 100_000 },
            target: { type: 'fallback', children: ['anthropic-claude-sonnet', 'openai-gpt-4o'] },
          },
        ],
        default: {
          type: 'weighted',
          children: [
            { deployment: 'openai-gpt-4o', weight: 80 },
            { deployment: 'anthropic-claude-sonnet', weight: 20 },
          ],
        },
      },
    },
    metadata: {
      etag: '"route-global"',
      generation: 9,
      created_at: ago(40 * 24 * HOUR),
      updated_at: ago(6 * HOUR),
      managed_by: 'api',
    },
  };
}

export function loginOptions(organization: string | null): S['LoginOptions'] {
  return {
    methods: [
      { type: 'oidc', id: 'workspace', display_name: 'Google Workspace' },
      { type: 'oidc', id: 'entra', display_name: 'Microsoft Entra' },
      { type: 'local', id: 'local' },
      { type: 'token', id: 'token' },
    ],
    password_reset: true,
    organization,
    version: '0.1.0',
    ticker: {
      requests_24h: 1_284_220,
      error_rate: 0.0042,
      p95_ms: 1840,
      active_keys: 41,
      as_of: new Date().toISOString(),
    },
    trusted_device: { enabled: true, max_age_days: 30 },
    password: { min_length: 12, max_length: 128, reset_ttl_seconds: 3600 },
    setup_required: false,
    device_authorization: true,
  };
}

export function anomalies(): S['Anomaly'][] {
  return [
    {
      id: 'an-1',
      key: 'support-bot',
      owner: 'ada',
      kind: 'country',
      severity: 'warning',
      first_seen: ago(2 * HOUR),
      last_seen: ago(HOUR),
      count: 42,
      state: 'open',
    },
    {
      id: 'an-2',
      key: 'batch-eval',
      owner: null,
      kind: 'spike',
      severity: 'info',
      first_seen: ago(3 * 24 * HOUR),
      last_seen: ago(3 * 24 * HOUR),
      count: 1,
      state: 'open',
    },
  ];
}
