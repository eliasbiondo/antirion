import { describe, expect, it } from 'vitest';
import { parse } from 'yaml';

import type { AuditEntry, LogRecord, UsageBucket } from '@/lib/api/types';

import { blankDraft, describeDraft, draftFrom, targetsLocked, toPredicate, toRule, toTarget } from '../draft';
import { decisionLatency, lastEditors, outcomeOf, resultsByRule, ruleImpact } from '../insights';
import { blankHedging, durationMs, validateExtras } from '../resilience';
import {
  advancedOf,
  afterDeletingRule,
  describePredicate,
  duplicateRule,
  rulesOf,
  settingsOf,
  strategyOf,
} from '../types';
import { toYaml } from '../yaml';

describe('routing rules', () => {
  const tree = {
    type: 'conditional',
    rules: [
      {
        name: 'support',
        when: {
          all: [
            { field: 'identity.team', eq: 'support' },
            { field: 'request.input_tokens', gt: 1000 },
          ],
        },
        target: { type: 'fallback', children: ['a', 'b'] },
      },
    ],
    default: 'a',
  };

  it('reads the rule list of a conditional root', () => {
    const parsed = rulesOf(tree);
    expect(parsed?.rules[0]?.name).toBe('support');
    expect(rulesOf({ type: 'weighted', children: [] })).toBeNull();
  });

  it('describes predicates in words', () => {
    expect(describePredicate(tree.rules[0]?.when)).toBe('team is support AND input tokens is over 1000');
    expect(describePredicate({ sample: 0.1 })).toBe('10% of requests');
    expect(describePredicate({ field: 'metadata.tier', eq: 'gold', sample: 0.25 })).toBe(
      'metadata tier is gold AND 25% of requests',
    );
    expect(describePredicate({ field: 'guard.pii.passed', eq: false })).toBe('guardrail pii passed is false');
  });

  it('round-trips a rule through the wizard draft', () => {
    const rule = rulesOf(tree)?.rules[0];
    if (!rule) throw new Error('no rule');
    const draft = draftFrom(rule);
    expect(draft.strategy).toBe('fallback');
    expect(draft.conditions).toHaveLength(2);
    expect(toRule(draft)).toEqual({
      name: 'support',
      when: tree.rules[0]?.when,
      target: { type: 'fallback', children: ['a', 'b'] },
    });
    expect(describeDraft(draft)).toBe('When team is support AND input tokens is over 1000, try a, then b.');
  });

  it('writes a block rule on the selectors the gateway accepts', () => {
    const draft = {
      ...blankDraft(),
      name: 'deny',
      strategy: 'block' as const,
      conditions: [
        { kind: 'tag' as const, op: 'contains' as const, value: 'secret', key: '', guardField: 'passed' },
      ],
    };
    expect(toRule(draft)).toEqual({
      name: 'deny',
      when: { field: 'identity.tags', contains: 'secret' },
      target: {
        type: 'block',
        status: 403,
        message: 'Request blocked by routing policy.',
        category: 'custom',
      },
    });
  });

  it('writes metadata, guardrail and sample conditions', () => {
    const draft = {
      ...blankDraft(),
      join: 'any' as const,
      sample: '10',
      conditions: [
        { kind: 'metadata' as const, op: 'eq' as const, value: 'gold', key: 'tier', guardField: 'passed' },
        { kind: 'guardrail' as const, op: 'eq' as const, value: 'false', key: 'pii', guardField: 'passed' },
      ],
    };
    expect(toPredicate(draft)).toEqual({
      any: [
        { field: 'metadata.tier', eq: 'gold' },
        { field: 'guard.pii.passed', eq: false },
      ],
      sample: 0.1,
    });
    const back = draftFrom({
      index: 0,
      name: 'x',
      description: '',
      enabled: true,
      tags: [],
      when: toPredicate(draft),
      target: 'a',
    });
    expect(back.sample).toBe('10');
    expect(back.conditions.map((c) => [c.kind, c.key])).toEqual([
      ['metadata', 'tier'],
      ['guardrail', 'pii'],
    ]);
    expect(toPredicate({ conditions: [], join: 'all', sample: '5' })).toEqual({ sample: 0.05 });
  });

  it('writes a canary with its ramp, error budget and stickiness', () => {
    const draft = {
      ...blankDraft(),
      name: 'o4',
      conditions: [],
      strategy: 'canary' as const,
      targets: [
        { deployment: 'gpt', weight: 1, role: '' },
        { deployment: 'o4', weight: 1, role: '' },
      ],
      canary: {
        steps: [
          { weight: '5', after: '0s' },
          { weight: '50', after: '1h' },
        ],
        budget: true,
        maxErrorRate: '2',
        window: '5m',
        action: 'rollback' as const,
      },
      sticky: { kind: 'session' as const, name: '' },
    };
    const target = toRule(draft).target;
    expect(target).toEqual({
      type: 'weighted',
      children: [
        { id: 'stable', deployment: 'gpt', weight: 95 },
        { id: 'canary', deployment: 'o4', weight: 5 },
      ],
      canary: {
        child: 'canary',
        steps: [
          { weight: 5, after: '0s' },
          { weight: 50, after: '1h' },
        ],
        error_budget: { max_error_rate: 0.02, window: '5m', action: 'rollback' },
      },
      sticky_by: 'session',
    });
    expect(strategyOf(target)).toBe('canary');
    const back = draftFrom({
      index: 0,
      name: 'o4',
      description: '',
      enabled: true,
      tags: [],
      when: undefined,
      target,
    });
    expect(back.canary.maxErrorRate).toBe('2');
    expect(back.targets.map((t) => t.role)).toEqual(['stable', 'canary']);
    expect(back.sticky.kind).toBe('session');
  });

  it('writes a shadow, a tier and per-rule timeouts, attempts and triggers', () => {
    const base = { ...blankDraft(), name: 's', conditions: [] };
    expect(
      toRule({
        ...base,
        strategy: 'shadow',
        targets: [{ deployment: 'gpt', weight: 1, role: '' }],
        shadow: { target: 'sonnet', percent: '10', sampleBy: 'session' },
      }).target,
    ).toEqual({
      type: 'deployment',
      deployment: 'gpt',
      mirror: { target: 'sonnet', fraction: 0.1, sample_by: 'session' },
    });
    expect(
      toRule({
        ...base,
        strategy: 'tier',
        tier: { tier: 'fast', strategy: 'least_latency', includePreview: true },
      }).target,
    ).toEqual({
      type: 'tier',
      tier: 'fast',
      strategy: 'least_latency',
      include_preview: true,
    });
    const fallback = toRule({
      ...base,
      strategy: 'fallback',
      targets: [
        { deployment: 'a', weight: 1, role: 'primary' },
        { deployment: 'b', weight: 1, role: '' },
      ],
      limits: { attempt: '8s', deadline: '', maxAttempts: '3' },
      triggers: { server_error: true, timeout: false, rate_limited: true, overloaded: true },
    }).target;
    expect(fallback).toEqual({
      type: 'fallback',
      children: [{ id: 'primary', deployment: 'a' }, 'b'],
      policies: {
        resilience: {
          timeouts: { attempt: '8s' },
          retry: { max_attempts: 3, classes: { timeout: { actions: ['retry_same'] } } },
        },
      },
    });
    expect(settingsOf(fallback)).toEqual([
      { label: 'Attempt timeout', value: '8s' },
      { label: 'Attempts at most', value: '3' },
      { label: 'Doesn’t move on after', value: 'timeouts' },
    ]);
    const back = draftFrom({
      index: 0,
      name: 's',
      description: '',
      enabled: true,
      tags: [],
      when: undefined,
      target: fallback,
    });
    expect(back.triggers.timeout).toBe(false);
    expect(toRule({ ...back, triggers: { ...back.triggers, timeout: true } }).target).toEqual({
      type: 'fallback',
      children: [{ id: 'primary', deployment: 'a' }, 'b'],
      policies: { resilience: { timeouts: { attempt: '8s' }, retry: { max_attempts: 3 } } },
    });
  });

  it('keeps what the wizard does not edit: advanced settings and nested targets', () => {
    const bandit = {
      type: 'weighted',
      algorithm: 'thompson',
      experiment: 'arms',
      metric: 'thumbs',
      children: [
        { id: 'a', deployment: 'a', weight: 1 },
        { id: 'b', deployment: 'b', weight: 1 },
      ],
    };
    const rule = {
      index: 0,
      name: 'x',
      description: '',
      enabled: true,
      tags: [],
      when: undefined,
      target: bandit,
    };
    expect(
      toRule({ ...draftFrom(rule), limits: { attempt: '', deadline: '', maxAttempts: '2' } }).target,
    ).toEqual({
      ...bandit,
      policies: { resilience: { retry: { max_attempts: 2 } } },
    });
    expect(advancedOf(bandit)).toEqual([
      { label: 'Adaptive split', value: 'learns from thumbs over every weighted target' },
    ]);
    const nested = { type: 'fallback', children: [{ type: 'least_cost', children: ['a', 'b'] }, 'c'] };
    const draft = draftFrom({ ...rule, target: nested });
    expect(targetsLocked(draft)).toBe(true);
    expect(toRule(draft).target).toEqual(nested);
  });

  it('duplicates a rule below it, turned off, with a free name', () => {
    const rules = [
      { name: 'a', target: 'x' },
      { name: 'a (copy)', target: 'y' },
    ];
    const next = duplicateRule(rules, 0);
    expect(next.map((r) => (r as { name: string }).name)).toEqual(['a', 'a (copy 2)', 'a (copy)']);
    expect((next[1] as { enabled: boolean }).enabled).toBe(false);
  });
});

describe('routing figures', () => {
  it('maps route-node usage to rules', () => {
    const rows = [
      { group: { route_node: 'g/r0' }, requests: 10, guardrail_blocks: 10 },
      { group: { route_node: 'g/r1/0/a' }, requests: 30, fallbacks: 2 },
      { group: { route_node: 'g/default' }, requests: 60 },
      { group: { route_node: 'other/r0' }, requests: 5 },
    ] as unknown as UsageBucket[];
    const { byRule, total } = ruleImpact(rows, 'g');
    expect(total).toBe(100);
    expect(byRule.get(0)).toEqual({ requests: 10, blocks: 10, fallbacks: 0 });
    expect(byRule.get(1)?.fallbacks).toBe(2);
    expect(byRule.get('default')?.requests).toBe(60);
  });

  it('finds each rule’s last editor from route writes', () => {
    const route = (rules: unknown[]) => ({ tree: { type: 'conditional', rules } });
    const audit = [
      {
        resource: 'routes/g',
        outcome: 'success',
        time: '2026-10-02T00:00:00Z',
        actor: { type: 'operator', id: 'ana' },
        changes: [
          {
            op: 'update',
            path: 'routes.g',
            before: route([
              { name: 'a', target: 'x' },
              { name: 'b', target: 'y' },
            ]),
            after: route([
              { name: 'a', target: 'z' },
              { name: 'b', target: 'y' },
            ]),
          },
        ],
      },
      {
        resource: 'routes/g',
        outcome: 'success',
        time: '2026-10-01T00:00:00Z',
        actor: { type: 'token', id: 'bootstrap' },
        changes: [
          {
            op: 'create',
            path: 'routes.g',
            before: null,
            after: route([
              { name: 'a', target: 'x' },
              { name: 'b', target: 'y' },
            ]),
          },
        ],
      },
    ] as unknown as AuditEntry[];
    const editors = lastEditors(audit, 'g');
    expect(editors.get('a')?.actor).toBe('ana');
    expect(editors.get('b')?.actor).toBe('bootstrap');
  });

  it('reads decision time and outcomes from records', () => {
    const rec = (route: number, extra: Partial<LogRecord> = {}) =>
      ({
        timing: { stage_ms: { resolve: 0.1, route } },
        status_code: 200,
        attempt_count: 1,
        response_path: 'upstream',
        code: null,
        stage: null,
        ...extra,
      }) as unknown as LogRecord;
    const d = decisionLatency(Array.from({ length: 100 }, (_, i) => rec(i / 100)));
    expect(d.count).toBe(100);
    expect(d.p95).toBeCloseTo(1.04, 5);
    expect(outcomeOf(rec(0, { code: 'guardrail_blocked', status_code: 451 }))).toBe('block');
    expect(outcomeOf(rec(0, { response_path: 'fallback', attempt_count: 2 }))).toBe('fallback');
    expect(outcomeOf(rec(0, { attempt_count: 2 }))).toBe('retry');
  });
});

describe('YAML export', () => {
  it('writes YAML that reads back as the same document', () => {
    const doc = {
      routes: {
        global: {
          tree: {
            type: 'conditional',
            rules: [
              {
                name: 'yes',
                when: { field: 'identity.team', eq: 'on' },
                target: { deployment: 'a: b', weight: 1 },
              },
              { name: '', target: ['x', 1, true, null] },
            ],
            default: {},
          },
        },
      },
      models: { 'gpt-4o': { route: 'global', aliases: [] } },
    };
    expect(parse(toYaml(doc))).toEqual(doc);
  });
});

describe('adaptive split in the wizard', () => {
  const node = {
    type: 'weighted',
    algorithm: 'thompson',
    experiment: 'support-trial',
    metric: 'thumbs',
    children: [
      { id: 'fast', deployment: 'a', weight: 1 },
      { id: 'smart', deployment: 'b', weight: 1 },
    ],
    min_samples_per_arm: 20,
    stop_after: { delta: 0.05 },
  };

  it('reads a bandit node back into the wizard and writes the same node', () => {
    const draft = draftFrom({
      name: 'trial',
      description: '',
      enabled: true,
      tags: [],
      when: undefined,
      target: node,
    } as never);
    expect(draft.strategy).toBe('adaptive');
    expect(draft.adaptive).toEqual({
      experiment: 'support-trial',
      metric: 'thumbs',
      minSamples: '20',
      stop: true,
      delta: '5',
    });
    expect(toTarget(draft)).toEqual(node);
    expect(describeDraft(draft)).toContain('learn from thumbs feedback which of a / b answers best');
  });

  it('names the arms when no role is given and keeps the defaults out', () => {
    const draft = {
      ...blankDraft(),
      name: 'new',
      strategy: 'adaptive' as const,
      targets: [
        { deployment: 'a', weight: 1, role: '' },
        { deployment: 'b', weight: 1, role: '' },
      ],
      adaptive: { experiment: 'e', metric: 'm', minSamples: '10', stop: false, delta: '5' },
    };
    expect(toTarget(draft)).toEqual({
      type: 'weighted',
      algorithm: 'thompson',
      experiment: 'e',
      metric: 'm',
      children: [
        { id: 'arm-1', deployment: 'a', weight: 1 },
        { id: 'arm-2', deployment: 'b', weight: 1 },
      ],
    });
  });
});

describe('feedback metrics an adaptive split learns from', () => {
  it('keeps booleans and ranged numbers', async () => {
    const { learnable } = await import('../components/adaptive-fields');
    expect(
      learnable({
        thumbs: { type: 'boolean' },
        score: { type: 'number', min: 0, max: 5 },
        open: { type: 'number' },
        comment: { type: 'text' },
      }),
    ).toEqual(['score', 'thumbs']);
  });
});

describe('pools, learned scorers, hedging and response checks in the wizard', () => {
  const rule = (target: unknown) => ({
    index: 0,
    name: 'x',
    description: '',
    enabled: true,
    tags: [],
    when: undefined,
    target,
  });

  it('reads a pool with an external picker and adapter affinity and writes the same node', () => {
    const pool = {
      type: 'pool',
      children: ['a', 'b'],
      metrics: { interval: '500ms' },
      scheduler: { queue_slack: 4, lora: true },
      picker: 'external',
      picker_service: { url: 'http://picker:9002', timeout: '200ms' },
      picker_on_error: 'builtin',
    };
    const draft = draftFrom(rule(pool));
    expect(strategyOf(pool)).toBe('pool');
    expect(draft.pool).toEqual({
      picker: 'external',
      url: 'http://picker:9002',
      timeout: '200ms',
      onError: 'builtin',
      lora: true,
    });
    expect(toTarget(draft)).toEqual(pool);
    const builtin = toTarget({ ...draft, pool: { ...draft.pool, picker: 'builtin', lora: false } });
    expect(builtin).toEqual({
      type: 'pool',
      children: ['a', 'b'],
      metrics: { interval: '500ms' },
      scheduler: { queue_slack: 4 },
    });
    expect(describeDraft(draft)).toContain('the endpoint picker at http://picker:9002 chooses');
  });

  it('writes a cost-quality node with the learned scorer, and keeps another scorer as written', () => {
    const draft = {
      ...blankDraft(),
      name: 'cheap-first',
      strategy: 'cost_quality' as const,
      costQuality: {
        ...blankDraft().costQuality,
        strong: 'big',
        weak: 'small',
        by: 'fraction' as const,
        percent: '30',
        modelFile: '/etc/mf.json',
        embeddingModel: 'embed',
        strongName: 'gpt-4',
        weakName: 'mixtral',
        onError: 'weak' as const,
      },
    };
    const node = toTarget(draft);
    expect(node).toEqual({
      type: 'cost_quality',
      strong: 'big',
      weak: 'small',
      strong_fraction: 0.3,
      scorer: {
        kind: 'matrix_factorization',
        model_file: '/etc/mf.json',
        embedding_model: 'embed',
        models: { strong: 'gpt-4', weak: 'mixtral' },
      },
      on_error: 'weak',
    });
    expect(draftFrom(rule(node)).costQuality).toEqual(draft.costQuality);
    const webhook = {
      type: 'cost_quality',
      strong: 'big',
      weak: 'small',
      threshold: 0.6,
      scorer: { kind: 'external', url: 'https://score' },
      calibration: { window: 500 },
    };
    const kept = draftFrom(rule(webhook));
    expect(kept.costQuality.scorer).toBe('kept');
    expect(toTarget({ ...kept, costQuality: { ...kept.costQuality, percent: '70' } })).toEqual({
      ...webhook,
      threshold: 0.7,
    });
    expect(strategyOf({ ...webhook, strong: { type: 'fallback', children: ['a'] } })).toBe('other');
  });

  it('writes hedging and response checks into the rule’s resilience and reads them back', () => {
    const draft = {
      ...blankDraft(),
      name: 'hedged',
      strategy: 'fallback' as const,
      targets: [
        { deployment: 'a', weight: 1, role: '' },
        { deployment: 'b', weight: 1, role: '' },
      ],
      hedging: { enabled: true, delay: '200ms', maxExtra: '2' as const },
      predicates: [
        { kind: 'header' as const, header: 'x-fail', equals: '1', reasons: '' },
        { kind: 'finish_reason' as const, header: '', equals: '', reasons: 'length, content_filter' },
        { kind: 'empty_content' as const, header: '', equals: '', reasons: '' },
      ],
    };
    const node = toTarget(draft);
    expect(node).toEqual({
      type: 'fallback',
      children: ['a', 'b'],
      policies: {
        resilience: {
          hedging: { enabled: true, delay: '200ms', max_extra: 2 },
          retry: {
            retry_on_response: [
              { header: 'x-fail', equals: '1' },
              { finish_reason: ['length', 'content_filter'] },
              { empty_content: true },
            ],
          },
        },
      },
    });
    const back = draftFrom(rule(node));
    expect(back.hedging).toEqual(draft.hedging);
    expect(back.predicates).toEqual(draft.predicates);
    expect(describeDraft(draft)).toContain('hedged after 200ms');
    expect(toTarget({ ...back, hedging: { ...back.hedging, enabled: false }, predicates: [] })).toEqual({
      type: 'fallback',
      children: ['a', 'b'],
    });
  });

  it('checks hedging delays and response checks', () => {
    expect(validateExtras({ enabled: true, delay: '5ms', maxExtra: '1' }, [], '')).toEqual({
      'hedging.delay': 'At least 10ms',
    });
    expect(
      Object.keys(
        validateExtras(
          blankHedging(),
          [{ kind: 'header', header: 'bad header', equals: '1', reasons: '' }],
          '',
        ),
      ),
    ).toEqual(['predicates.0']);
    expect(durationMs('1.5s')).toBe(1500);
  });

  it('maps a pool’s picker state to its rule', () => {
    const picker = {
      url: 'http://picker:9002',
      timeout: '300ms',
      on_error: 'reject' as const,
      state: 'ok' as const,
      asks: 3,
      failures: 0,
      last_asked_at: '2026-10-15T10:00:00Z',
      last_pick: { at: '2026-10-15T10:00:00Z', deployments: ['b'], served: 'b' },
      last_failure: null,
    };
    const results = resultsByRule(
      [{ path: 'global/r1', type: 'pool', children: [], replica: 'n1', picker }],
      'global',
    );
    expect(results.get(1)?.picker?.last_pick?.served).toBe('b');
  });
});

describe('deleting a rule', () => {
  const rule = (name: string) => ({ name, when: { field: 'identity.team', eq: name }, target: 'a' });
  it('keeps the other rules', () => {
    const tree = { type: 'conditional', rules: [rule('x'), rule('y')], default: 'b' };
    expect(afterDeletingRule(tree, tree.rules, 0)).toEqual({
      kind: 'tree',
      tree: { type: 'conditional', rules: [rule('y')], default: 'b' },
    });
  });
  it('leaves the default alone when the last rule goes, and the route when there is none', () => {
    const tree = { type: 'conditional', rules: [rule('x')], default: { deployment: 'b' } };
    expect(afterDeletingRule(tree, tree.rules, 0)).toEqual({ kind: 'default', tree: { deployment: 'b' } });
    const bare = { type: 'conditional', rules: [rule('x')] };
    expect(afterDeletingRule(bare, bare.rules, 0)).toEqual({ kind: 'route' });
  });
});
