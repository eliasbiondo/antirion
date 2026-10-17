import { describe, expect, it } from 'vitest';

import type { AuditEntry, ModelResource, UsageBucket } from '@/lib/api/types';
import { formatTokenPrice } from '@/lib/format';
import { formatUnitAmount } from '@/lib/price';

import { activityFeed, describeAudit } from '../activity';
import {
  capabilityTags,
  catalogSync,
  formatContext,
  leader,
  modalityFor,
  outputSpeed,
  priceLines,
  ruleOfPath,
  shiftDecimal,
  sumBuckets,
  toPerMillion,
  fromPerMillion,
  worstErrorRate,
} from '../catalog';
import { applyModelEdit, fromModelSpec } from '../form';
import { filterModels, EMPTY_MODEL_FILTERS, hasState, toModelFilters, MODEL_FILTER_DEFAULTS } from '../hooks';
import { moveStep, newStep, stepsOf, withChain, withRoute } from '../tree';
import { toModelView } from '../types';
import { aliasNameError, withAlias, withoutAlias } from '../components/alias-dialog';
import { migrationLines } from '../components/migration-panel';
import { bucketOf, rebin } from '../components/model-usage';
import { rowPrice } from '../components/models-screen';

const meta = { etag: '"1"', generation: 1, created_at: '', updated_at: '', managed_by: 'api' as const };
const caps = (entries: Record<string, [unknown, string]>) =>
  Object.fromEntries(
    Object.entries(entries).map(([k, [value, source]]) => [
      k,
      { value, source: source as 'deployment' | 'type_default' },
    ]),
  );

function bucket(
  group: Record<string, string>,
  requests: number,
  extra: Partial<UsageBucket> = {},
): UsageBucket {
  return { start: '2026-10-14T00:00:00Z', group, requests, errors: 0, cost_usd: '0', ...extra };
}

describe('prices in the reader’s unit', () => {
  it('moves the decimal point exactly', () => {
    expect(shiftDecimal('2.5', -3)).toBe('0.0025');
    expect(shiftDecimal('0.0025', 3)).toBe('2.5');
    expect(shiftDecimal('15', 3)).toBe('15000');
    expect(shiftDecimal('0.075', -3)).toBe('0.000075');
  });
  it('converts between per 1K and per 1M', () => {
    expect(toPerMillion('0.0025', '1K')).toBe('2.5');
    expect(toPerMillion('2.50', '1M')).toBe('2.50');
    expect(fromPerMillion('10', '1K')).toBe('0.01');
    expect(fromPerMillion(null, '1K')).toBe('');
  });
  it('states a row’s price for its modality', () => {
    const p = (price: Record<string, string>) => ({ deployment: 'd', price, price_version: null });
    expect(rowPrice([p({ input: '2.5', output: '10' })], 'chat', '1M')).toBe('$2.50 / $10.00 per 1M');
    expect(rowPrice([p({ input: '2.5', output: '10' }), p({ input: '3', output: '9' })], 'chat', '1K')).toBe(
      'from $0.0025 / $0.009 per 1K',
    );
    expect(rowPrice([p({ input: '0.02', output: '0.02' })], 'embeddings', '1M')).toBe('$0.02 per 1M');
    expect(rowPrice([p({ per_request: '0.002' })], 'rerank', '1M')).toBe('$0.002 a request');
    expect(rowPrice([null], 'chat', '1M')).toBeNull();
  });
  it('picks the price lines of a modality', () => {
    expect(priceLines({ input: '1', output: '2' }, 'embeddings').map((l) => l.field)).toEqual(['input']);
    expect(priceLines({ per_request: '0.002' }, 'rerank').map((l) => l.field)).toEqual(['per_request']);
    expect(
      priceLines({ input: '3', output: '15', cache_read: '0.3', cache_write_5m: '3.75' }, 'chat').map(
        (l) => l.field,
      ),
    ).toEqual(['input', 'output', 'cache_read', 'cache_write_5m']);
  });
});

describe('capabilities', () => {
  it('reads the modality from stated operations, else the upstream id', () => {
    const all = caps({ operations: [['generate', 'embed'], 'type_default'] });
    expect(modalityFor(all, 'text-embedding-3-small')).toBe('embeddings');
    expect(modalityFor(all, 'gpt-4o')).toBe('chat');
    expect(modalityFor(caps({ operations: [['rerank'], 'deployment'] }), 'x')).toBe('rerank');
    expect(modalityFor(undefined, 'rerank-english-v3.0')).toBe('rerank');
  });
  it('lists only stated capability tags', () => {
    expect(capabilityTags(caps({ vision: [true, 'type_default'], tools: [true, 'deployment'] }))).toEqual([
      'Tools',
    ]);
  });
  it('writes context windows short', () => {
    expect(formatContext(128000)).toBe('128K');
    expect(formatContext(8191)).toBe('8.2K');
    expect(formatContext(1_000_000)).toBe('1M');
    expect(formatContext(null)).toBe('—');
  });
});

describe('usage', () => {
  it('sums buckets with request-weighted percentiles', () => {
    const t = sumBuckets([
      bucket({}, 10, { latency_ms: { p95: 100 }, input: 5, cancelled: 1 }),
      bucket({}, 30, { latency_ms: { p95: 200 }, input: 5 }),
    ]);
    expect(t.requests).toBe(40);
    expect(t.latencyP95).toBe(175);
    expect(t.input).toBe(10);
    expect(t.cancelled).toBe(1);
  });
  it('names the leading Model and the worst error rate', () => {
    const usage = new Map([
      ['a', { requests: 100, errors: 1, cost: 30 }],
      ['b', { requests: 50, errors: 10, cost: 10 }],
      ['c', { requests: 3, errors: 3, cost: 0 }],
    ]);
    expect(leader(usage)).toEqual({ name: 'a', share: 0.75 });
    expect(worstErrorRate(usage)).toEqual({ name: 'b', rate: 0.2 });
  });
  it('maps a route path to its rule', () => {
    expect(ruleOfPath('chat-shared/r1/0/gpt-4o', 'chat-shared')).toBe(1);
    expect(ruleOfPath('chat-shared/default/0/x', 'chat-shared')).toBe('default');
    expect(ruleOfPath('chat-shared/top/r0/x', 'chat-shared', 'top')).toBe(0);
    expect(ruleOfPath('other/r0/x', 'chat-shared')).toBeNull();
  });
  it('rebins a histogram to twelve buckets and finds percentiles', () => {
    const h = {
      bounds: Array.from({ length: 25 }, (_, i) => i * 10),
      counts: Array.from({ length: 24 }, () => 1),
    };
    const r = rebin(h);
    expect(r.counts).toHaveLength(12);
    expect(r.counts.reduce((a, b) => a + b, 0)).toBe(24);
    expect(bucketOf(r.bounds, 25)).toBe(1);
  });
  it('lists who still calls a deprecated Model', () => {
    const now = Date.parse('2026-10-15T00:00:00Z');
    const lines = migrationLines(
      [
        { ...bucket({ key: 'k1' }, 5), start: '2026-09-20T00:00:00Z' },
        { ...bucket({ key: 'k2' }, 3), start: '2026-10-14T00:00:00Z' },
      ],
      [bucket({ requested_model: 'old-alias' }, 4), bucket({ requested_model: 'model' }, 9)],
      ['old-alias', 'unused'],
      now,
    );
    expect(lines.map((l) => [l.kind, l.name, l.lastWeek])).toEqual([
      ['alias', 'old-alias', 4],
      ['key', 'k2', 3],
      ['key', 'k1', 0],
      ['alias', 'unused', 0],
    ]);
  });
});

describe('fallback chains', () => {
  it('reads a fallback root as steps and writes them back', () => {
    const spec = { target: { type: 'fallback', id: 'top', children: ['a', { route: 'shared' }] } };
    const steps = stepsOf(spec);
    expect(steps.map((s) => [s.kind, s.value])).toEqual([
      ['deployment', 'a'],
      ['route', 'shared'],
    ]);
    const next = withChain(spec, [...moveStep(steps, 1, -1), newStep('tier', 'fast')]);
    expect(next.target).toEqual({
      id: 'top',
      type: 'fallback',
      children: [{ route: 'shared' }, 'a', { tier: 'fast' }],
    });
  });
  it('makes one step the target itself and leaves a shared route', () => {
    const spec = { route: 'shared', tier: 'fast' };
    expect(stepsOf(spec).map((s) => s.kind)).toEqual(['route']);
    expect(withChain(spec, [newStep('deployment', 'd')])).toEqual({ tier: 'fast', target: 'd' });
    expect(withRoute({ target: 'd', tier: 'fast' }, 'shared')).toEqual({ tier: 'fast', route: 'shared' });
  });
  it('points a Model at a shared route from the edit form', () => {
    const spec = { target: 'dep', tier: 'fast' };
    expect(applyModelEdit(spec, { ...fromModelSpec(spec), serves: 'route', route: 'shared' }).route).toBe(
      'shared',
    );
    expect(
      applyModelEdit(spec, { ...fromModelSpec(spec), serves: 'route', route: 'shared' }).target,
    ).toBeUndefined();
    const routed = { route: 'shared' };
    expect(
      applyModelEdit(routed, { ...fromModelSpec(routed), serves: 'own', deployment: 'd' }),
    ).toMatchObject({
      target: 'd',
    });
  });
});

describe('aliases', () => {
  const taken = { models: ['gpt-4o'], aliases: ['fast'] };
  it('checks alias names as the gateway does', () => {
    expect(aliasNameError('gpt-4o', taken)).toMatch(/model already/);
    expect(aliasNameError('fast', taken)).toMatch(/Another alias/);
    expect(aliasNameError('fast', taken, 'fast')).toBeNull();
    expect(aliasNameError('a*', taken)).toMatch(/\*/);
    expect(aliasNameError('chat-default', taken)).toBeNull();
  });
  it('renames an alias keeping its parameters, and removes one', () => {
    const spec = { target: 'd', aliases: { old: { params: { set: { temperature: 0 } }, owner: 'x' } } };
    const next = withAlias(spec, 'old', 'new', { description: 'Hi', owner: '' });
    expect(next.aliases).toEqual({ new: { params: { set: { temperature: 0 } }, description: 'Hi' } });
    expect(withoutAlias(next, 'new')).toEqual({ target: 'd' });
  });
});

describe('filters and activity', () => {
  const model = (name: string, labels: string[]): ModelResource => ({
    name,
    spec: { target: `${name}-dep`, labels },
    metadata: meta,
  });
  const deps = new Map(
    ['a', 'b'].map((n) => [
      `${n}-dep`,
      {
        name: `${n}-dep`,
        spec: { provider: 'p', model: n === 'a' ? 'gpt-4o' : 'text-embedding-3' },
        metadata: meta,
      },
    ]),
  );
  const views = [model('a', ['chat', 'eu']), model('b', ['eu'])].map((m) => toModelView(m, deps, new Map()));
  it('filters by modality and labels, and sorts by p95', () => {
    expect(
      filterModels(views, { ...EMPTY_MODEL_FILTERS, modality: 'embeddings' }, null, () => null).map(
        (v) => v.name,
      ),
    ).toEqual(['b']);
    expect(
      filterModels(views, { ...EMPTY_MODEL_FILTERS, labels: ['chat', 'eu'] }, null, () => null).map(
        (v) => v.name,
      ),
    ).toEqual(['a']);
    const measures = new Map([
      ['a', { requests: 1, cost: 0, p95: 900 }],
      ['b', { requests: 1, cost: 0, p95: 100 }],
    ]);
    expect(
      filterModels(views, { ...EMPTY_MODEL_FILTERS, sort: 'p95' }, measures, () => null).map((v) => v.name),
    ).toEqual(['b', 'a']);
    expect(toModelFilters({ ...MODEL_FILTER_DEFAULTS, sort: 'bogus' }).sort).toBe('name');
  });
  it('describes catalog changes in words', () => {
    const entry = (over: Partial<AuditEntry>): AuditEntry => ({
      id: 'e',
      time: '2026-10-14T00:00:00Z',
      actor: { type: 'operator', id: 'ada' },
      action: 'models.update',
      resource: 'models/gpt-4o',
      outcome: 'success',
      status_code: 200,
      generation_before: 1,
      generation_after: 2,
      request_id: 'r',
      source_ip: '127.0.0.1',
      changes: null,
      ...over,
    });
    const lifecycle = entry({
      changes: [
        {
          op: 'update',
          path: 'models.gpt-4o',
          before: { tier: 'fast' },
          after: { tier: 'fast', lifecycle: { state: 'deprecated' } },
        },
      ] as AuditEntry['changes'],
    });
    expect(describeAudit(lifecycle)?.text).toBe('Model gpt-4o deprecated');
    const approved = entry({
      id: 'f',
      action: 'POST access_requests:approve',
      resource: 'models/o4/access_requests/ar_1',
      changes: { scope: 'team/x' } as unknown as AuditEntry['changes'],
    });
    expect(describeAudit(approved)).toMatchObject({
      kind: 'access',
      text: 'Access to o4 approved for team/x',
    });
    expect(describeAudit(entry({ resource: 'keys/k' }))).toBeNull();
    expect(activityFeed([lifecycle, approved, lifecycle], [])).toHaveLength(2);
  });
});

describe('catalog sync status', () => {
  const bundled = { version: '2026-10-01', price_version: '2026-09-01', entries: 40 };
  it('shows the bundled prices until a runtime catalog is set', () => {
    const sync = catalogSync({ bundled, runtime: null, items: [] });
    expect(sync.text).toBe('Prices 2026-09-01');
    expect(sync.at).toBeNull();
  });
  it('shows when the runtime catalog last changed', () => {
    const sync = catalogSync({
      bundled,
      runtime: {
        mode: 'replace',
        version: 'v7',
        entries: 1,
        updated_at: '2026-10-14T10:00:00Z',
        generation: 4,
      },
      items: [],
    });
    expect(sync.text).toBe('Catalog v7');
    expect(sync.at).toBe('2026-10-14T10:00:00Z');
    expect(sync.detail).toContain('1 entry, replaces the bundled one');
  });
});

describe('prices per 1K, output speed and status counts', () => {
  it('never rounds a per-1K price to a wrong or zero value', () => {
    expect(formatUnitAmount('0.15', '1K')).toBe('$0.00015');
    expect(formatUnitAmount('0.02', '1K')).toBe('$0.00002');
    expect(formatUnitAmount('9', '1K')).toBe('$0.009');
    expect(formatUnitAmount('2.5', '1M')).toBe('$2.50');
    expect(formatUnitAmount(null, '1K')).toBe('—');
    expect(formatTokenPrice('0.15', '1K')).toBe('$0.00015 / 1K');
    const p = (price: Record<string, string>) => ({ deployment: 'd', price, price_version: null });
    expect(rowPrice([p({ input: '0.15', output: '0.6' })], 'chat', '1K')).toBe('$0.00015 / $0.0006 per 1K');
  });

  it('shows an output speed only when it is one', () => {
    expect(outputSpeed(943_600.868)).toBeNull();
    expect(outputSpeed(1234.4)).toBe('1,234');
    expect(outputSpeed(null)).toBeNull();
    expect(outputSpeed(0)).toBeNull();
  });

  it('matches a status chip by lifecycle or on/off', () => {
    expect(hasState({ lifecycle: 'preview', enabled: true }, 'preview')).toBe(true);
    expect(hasState({ lifecycle: 'preview', enabled: true }, 'enabled')).toBe(true);
    expect(hasState({ lifecycle: 'ga', enabled: false }, 'disabled')).toBe(true);
    expect(hasState({ lifecycle: 'ga', enabled: false }, 'enabled')).toBe(false);
  });
});
