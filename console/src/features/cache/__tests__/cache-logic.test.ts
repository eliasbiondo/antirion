import { describe, expect, it } from 'vitest';

import type { AuditEntry, UsageBucket } from '@/lib/api/types';

import { toActivity } from '../activity';
import type { EffectivePrice } from '../api';
import {
  cachedReadDiscount,
  deploymentsOf,
  describeCache,
  ignoreFieldError,
  keyStrategyOf,
  modesOf,
  overridesOf,
  prefixRows,
} from '../policy';
import { bucketsOf } from '../store';
import { describeRule, draftErrors, emptyDraft, PRESETS, specOf, validCron } from '../rules';

function price(deployment: string, p: Record<string, string>): [string, EffectivePrice] {
  return [deployment, { deployment, price: p, price_version: 'v' }];
}

function bucket(group: Record<string, string | null>, extra: Partial<UsageBucket>): UsageBucket {
  return { start: '2026-10-15T00:00:00Z', group, requests: 1, errors: 0, cost_usd: '0', ...extra };
}

function audit(e: Partial<AuditEntry> & Pick<AuditEntry, 'action'>): AuditEntry {
  return {
    id: Math.random().toString(36),
    time: '2026-10-15T08:00:00Z',
    actor: { type: 'operator', id: 'ada' },
    resource: null,
    outcome: 'success',
    status_code: 200,
    generation_before: null,
    generation_after: null,
    request_id: 'r',
    source_ip: '127.0.0.1',
    changes: null,
    ...e,
  };
}

describe('cache policy helpers', () => {
  it('treats the response cache, similar prompts and provider prompt caching as separate modes', () => {
    expect(modesOf({})).toEqual({ exact: false, semantic: false, prompt: false });
    expect(modesOf({ prompt_caching: { auto_inject: true } })).toEqual({
      exact: false,
      semantic: false,
      prompt: true,
    });
    // Similar-prompt matching sits on the response cache: off with it.
    expect(modesOf({ enabled: false, semantic: { enabled: true } }).semantic).toBe(false);
    expect(modesOf({ enabled: true, semantic: { enabled: true } })).toEqual({
      exact: true,
      semantic: true,
      prompt: false,
    });
  });

  it('names key strategies by their preset, in any order, and custom lists as custom', () => {
    expect(keyStrategyOf({}).id).toBe('full');
    expect(keyStrategyOf({ key: { ignore_fields: ['/prediction'] } }).id).toBe('prediction');
    expect(
      keyStrategyOf({ key: { ignore_fields: ['/tags', '/session_id', '/trace_id', '/request_id'] } }).id,
    ).toBe('tracing');
    expect(keyStrategyOf({ key: { ignore_fields: ['/x'] } })).toMatchObject({
      id: 'custom',
      label: 'Custom (1 ignored)',
    });
  });

  it('never lets the key drop a field that changes the answer', () => {
    expect(ignoreFieldError('/request_id')).toBeNull();
    expect(ignoreFieldError('/metadata/trace')).toBeNull();
    expect(ignoreFieldError('/model')).toMatch(/always part of the key/);
    for (const f of ['/messages/0', '/temperature', '/tools', '/seed', '/response_format', '/input'])
      expect(ignoreFieldError(f)).toMatch(/changes the answer/);
    expect(ignoreFieldError('request_id')).toMatch(/JSON Pointer/);
  });

  it('finds a Model’s deployments through its target and its shared route', () => {
    const routes = new Map([
      [
        'shared',
        {
          name: 'shared',
          spec: {
            tree: {
              type: 'fallback',
              children: [{ deployment: 'b' }, { type: 'deployment', deployment: 'c' }],
            },
          },
          metadata: {},
        },
      ],
    ]) as never;
    const model = {
      name: 'm',
      spec: { route: 'shared', target: { deployment: 'a' } },
      metadata: {},
    } as never;
    expect(deploymentsOf(model, routes).sort()).toEqual(['a', 'b', 'c']);
  });

  it('computes the cached-read discount from effective prices, as a range over deployments', () => {
    const prices = new Map([
      price('a', { input: '3', cache_read: '0.3' }),
      price('b', { input: '2.5', cache_read: '1.25' }),
      price('c', { input: '1' }),
    ]);
    expect(cachedReadDiscount(['a'], prices)).toEqual({ min: 0.9, max: 0.9 });
    const both = cachedReadDiscount(['a', 'b', 'c'], prices);
    expect(both?.min).toBeCloseTo(0.5);
    expect(both?.max).toBeCloseTo(0.9);
    expect(cachedReadDiscount(['c'], prices)).toBeNull();
  });

  it('adds up prompt-cache use per prefix and prices what cached reads saved', () => {
    const prices = new Map([price('claude', { input: '3', cache_read: '0.3' })]);
    const rows = prefixRows(
      [
        bucket({ cache_prefix: null, deployment: 'claude' }, { requests: 9, input_cache_read: 999 }),
        bucket({ cache_prefix: 'p1', deployment: 'claude' }, { requests: 2, input_cache_read: 1_000_000 }),
        bucket({ cache_prefix: 'p1', deployment: 'claude' }, { requests: 1, input_cache_write: 500 }),
        bucket({ cache_prefix: 'p2', deployment: 'unpriced' }, { requests: 1, input_cache_read: 10 }),
      ],
      prices,
    );
    expect(rows.map((r) => r.prefix)).toEqual(['p1', 'p2']);
    expect(rows[0]).toMatchObject({ requests: 3, read: 1_000_000, write: 500, deployments: ['claude'] });
    expect(rows[0]?.saved).toBeCloseTo(2.7);
    expect(rows[1]?.saved).toBeNull();
  });

  it('lists overrides with what they set and the level the rest comes from', () => {
    const out = overridesOf({
      organizations: [{ name: 'acme', cache: { ttl: '6h' } }],
      teams: [
        { name: 'search', organization: 'acme', cache: undefined },
        { name: 'support', organization: 'acme', cache: { read_only: true } },
      ],
      projects: [{ name: 'web', team: 'search', cache: undefined }],
      keys: [
        { name: 'k1', project: 'web', cache: { ttl: '5m' } },
        { name: 'k2', project: 'web', cache: {} },
      ],
      routes: [],
    });
    expect(out).toEqual([
      { level: 'organization', name: 'acme', inherits: 'the gateway default', settings: ['Keep 6 h'] },
      { level: 'team', name: 'support', inherits: 'organization acme', settings: ['Read only'] },
      { level: 'key', name: 'k1', inherits: 'organization acme', settings: ['Keep 5 min'] },
    ]);
  });

  it('describes fail-closed and per-item embedding settings in words', () => {
    expect(describeCache({ fail_mode: 'closed', embeddings: { per_item: true } })).toEqual([
      'Refuses requests when the cache is down',
      'Embeddings cached per item',
    ]);
  });
});

describe('clearing rules', () => {
  it('checks cron expressions', () => {
    expect(validCron('0 3 * * *')).toBe(true);
    expect(validCron('*/15 * * * 1-5')).toBe(true);
    expect(validCron('0 3 * *')).toBe(false);
    expect(validCron('every night')).toBe(false);
  });

  it('asks for what each trigger and action needs', () => {
    const d = { ...emptyDraft(), name: 'r' };
    expect(draftErrors({ ...d, event: 'schedule', schedule: '' }, true).schedule).toBeDefined();
    expect(draftErrors({ ...d, event: 'metadata_changed' }, true).metadataKey).toBeDefined();
    expect(draftErrors({ ...d, drop: 'older_than', olderThan: '0h' }, true).olderThan).toBeDefined();
    expect(draftErrors({ ...d, drop: 'tag', value: '' }, true).value).toBeDefined();
    // A model, deployment or route drop takes its value from the trigger that names one.
    expect(draftErrors({ ...d, event: 'model_disabled', drop: 'model', value: '' }, true)).toEqual({});
    expect(draftErrors({ ...d, event: 'manual', drop: 'model', value: '' }, true).value).toBeDefined();
    expect(draftErrors({ ...d, drop: 'metadata', value: 'nokey' }, true).value).toMatch(/key=value/);
    expect(draftErrors({ ...d, name: '-bad' }, true).name).toBeDefined();
    expect(draftErrors({ ...d, name: '-bad' }, false).name).toBeUndefined();
  });

  it('writes only the members a trigger and action take', () => {
    const nightly = PRESETS.find((p) => p.id === 'nightly')?.draft;
    expect(nightly && specOf(nightly)).toEqual({
      enabled: true,
      description: 'Every night, drop responses cached more than 12 hours ago.',
      trigger: { event: 'schedule', schedule: '0 3 * * *' },
      action: { drop: 'older_than', older_than: '12h' },
    });
    expect(
      specOf({
        ...emptyDraft(),
        event: 'schedule',
        schedule: ' 0 1 * * * ',
        drop: 'tag',
        value: 'faq',
        olderThan: '1d',
      }),
    ).toEqual({
      enabled: true,
      trigger: { event: 'schedule', schedule: '0 1 * * *' },
      action: { drop: 'tag', value: 'faq' },
    });
  });

  it('says a rule in words', () => {
    expect(describeRule({ trigger: { event: 'model_disabled' }, action: { drop: 'model' } })).toBe(
      'When a model is turned off, clear that model’s responses.',
    );
    expect(
      describeRule({
        trigger: { event: 'schedule', schedule: '0 3 * * *' },
        action: { drop: 'older_than', older_than: '12h' },
      }),
    ).toBe('Every night at 03:00 UTC, clear responses older than 12 h.');
    expect(describeRule({ trigger: { event: 'manual' }, action: { drop: 'end_user', value: 'u-1' } })).toBe(
      'When run by hand, clear the responses of end user u-1.',
    );
  });
});

describe('cache activity', () => {
  it('turns cache audit entries into sentences and drops unrelated Model edits', () => {
    const items = toActivity([
      audit({ action: 'cache.invalidate', resource: 'cache/gateway' }),
      audit({ action: 'cache.invalidate', resource: 'cache/model/gpt-4o' }),
      audit({
        action: 'cache.rule_fired',
        resource: 'cache/invalidation_rules/off',
        actor: { type: 'system', id: 'cache_invalidation_rules' },
        changes: { trigger: 'model_disabled', dropped: 3 } as never,
      }),
      audit({
        action: 'cache.invalidation_rules.patch',
        resource: 'cache/invalidation_rules/nightly',
        changes: [{ op: 'update', path: 'x', before: { enabled: true }, after: { enabled: false } }],
      }),
      audit({
        action: 'models.update',
        resource: 'models/m',
        changes: [{ op: 'update', path: 'models.m', before: { enabled: true }, after: { enabled: false } }],
      }),
      audit({
        action: 'models.update',
        resource: 'models/m',
        changes: [
          {
            op: 'update',
            path: 'models.m',
            before: { policies: { cache: { enabled: true, ttl: '1h' } } },
            after: { policies: { cache: { enabled: true, ttl: '15m' } } },
          },
        ],
      }),
      audit({ action: 'cache.delete', resource: 'cache/entries/abc', kind: 'read' }),
    ]);
    expect(items.map((i) => i.text)).toEqual([
      'Cleared the whole cache',
      'Cleared the cache of model gpt-4o',
      'Rule off fired (model disabled), removing 3 responses',
      'Turned off clearing rule nightly',
      'Changed caching of model m: keep 15 min',
    ]);
    expect(items[2]?.actor).toBe('Automatic');
  });
});

describe('requests sent to a provider', () => {
  it('leaves out requests refused before routing, which name no provider', () => {
    const row = (provider: string | null, requests: number, hits: number) => ({
      start: '2026-10-16T10:00:00Z',
      group: { provider },
      requests,
      errors: 0,
      cost_usd: '0',
      cache_hits: hits,
    });
    expect(bucketsOf([row('openai', 30, 5), row(null, 12, 0)] as never)).toEqual([
      { start: '2026-10-16T10:00:00Z', requests: 42, hits: 5, sent: 25, saved: 0, cost: 0 },
    ]);
  });
});
