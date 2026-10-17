import { describe, expect, it } from 'vitest';

import type { BudgetCounter } from '@/lib/api/types';

import { grantExpiry } from '../components/overage-dialog';
import { recostRange } from '../components/cost-tools';
import {
  budgetSchema,
  EMPTY_BUDGET,
  fromDeclaration,
  modelsCovered,
  toDeclarations,
  toOverride,
} from '../form';
import { peopleFacts } from '../people';
import {
  buildTree,
  flatten,
  isCounted,
  parsePartition,
  partitionLabel,
  project,
  stateOf,
  topIssue,
} from '../types';

const counter = (
  level: BudgetCounter['level'],
  owner: string,
  spent: string,
  amount = '100',
  mode: 'hard' | 'soft' = 'soft',
): BudgetCounter => ({
  level,
  owner,
  name: 'monthly',
  partition: null,
  unit: 'usd',
  mode,
  amount,
  spent,
  reserved: '0',
  remaining: '0',
  period_start: '2026-06-01T00:00:00Z',
  resets_at: '2026-07-01T00:00:00Z',
});
const NOW = new Date('2026-06-16T00:00:00Z').getTime();
const FORECAST = {
  rate_per_day: '1',
  projected_spend: '120',
  projected_ratio: 1.2,
  runs_out_at: '2026-06-28T00:00:00Z',
  basis_from: '2026-06-09T00:00:00Z',
  basis_to: '2026-06-16T00:00:00Z',
};

describe('budget forecasts', () => {
  it('projects linearly over the period', () => {
    const { projected, elapsed } = project(counter('team', 't', '50'), NOW);
    expect(elapsed).toBeCloseTo(0.5, 2);
    expect(projected).toBeCloseTo(100, 0);
  });
  it('states over, blocked, at risk and on pace', () => {
    expect(stateOf(counter('team', 't', '120'), 240)).toBe('over');
    expect(stateOf(counter('team', 't', '120', '100', 'hard'), 240)).toBe('blocked');
    expect(stateOf(counter('team', 't', '60'), 120)).toBe('at_risk');
    expect(stateOf(counter('team', 't', '10'), 20)).toBe('on_pace');
  });
  it('nests projects under their team and picks the top issue', () => {
    const parentOf = (level: string, owner: string) =>
      level === 'project' && owner === 'p' ? { level: 'team', owner: 't' } : null;
    const tree = buildTree(
      [counter('team', 't', '10'), counter('project', 'p', '90', '100', 'hard')],
      parentOf,
      NOW,
    );
    expect(tree).toHaveLength(1);
    expect(tree[0]?.children[0]?.owner).toBe('p');
    expect(flatten(tree)).toHaveLength(2);
    expect(topIssue(tree)?.owner).toBe('p');
  });
});

describe('budget form', () => {
  it('declares a soft budget with a hard cap twin', () => {
    const d = toDeclarations({ ...EMPTY_BUDGET, owner: 'core', amount: '500', hardCap: '110' });
    expect(d.monthly).toMatchObject({ unit: 'usd', amount: '500', mode: 'soft', alert_at: [0.5, 0.8, 1] });
    expect(d.monthly_cap).toMatchObject({ amount: '550', mode: 'hard' });
  });
  it('removes the cap twin when no cap is set', () => {
    expect(toDeclarations({ ...EMPTY_BUDGET, owner: 'core', amount: '500' }).monthly_cap).toBeNull();
  });
  it('rejects decreasing thresholds and a cap on a hard budget', () => {
    expect(
      budgetSchema.safeParse({ ...EMPTY_BUDGET, owner: 'core', amount: '5', alerts: '80, 50' }).success,
    ).toBe(false);
    expect(
      budgetSchema.safeParse({
        ...EMPTY_BUDGET,
        owner: 'core',
        amount: '5',
        enforcement: 'hard',
        hardCap: '110',
      }).success,
    ).toBe(false);
  });
});

describe('partitions and Unassigned', () => {
  const seat = (user: string, spent: string, amount = '3'): BudgetCounter => ({
    ...counter('organization', 'acme', spent, amount, 'hard'),
    name: 'seats',
    partition: `user=${user}`,
  });
  const parentOf = (level: string, owner: string) =>
    level === 'team' && owner ? { level: 'organization', owner: 'acme' } : null;
  it('groups a partitioned budget and places seats under the team budget or Unassigned', () => {
    const tree = buildTree(
      [
        counter('organization', 'acme', '10'),
        counter('team', 'core', '5'),
        seat('ada', '1', '6'),
        seat('bob', '4'),
      ],
      { parentOf, teamOfUser: (u) => (u === 'ada' ? 'core' : 'ghost') },
      NOW,
    );
    const rows = flatten(tree);
    const group = rows.find((r) => r.kind === 'partitioned');
    expect(group?.partitions).toBe(2);
    expect(group?.spent).toBeCloseTo(5);
    expect(group?.amount).toBeCloseTo(9);
    const ada = rows.find((r) => r.partition?.value === 'ada');
    expect(rows.find((r) => r.id === ada?.parentId)?.owner).toBe('core');
    const unassigned = rows.find((r) => r.kind === 'unassigned');
    expect(unassigned?.children.map((r) => r.partition?.value)).toEqual(['bob']);
    expect(rows.filter(isCounted)).toHaveLength(4);
  });
  it('reads partitions and labels the shared counter', () => {
    expect(parsePartition('user=ada')).toEqual({ attr: 'user', value: 'ada' });
    expect(parsePartition(null)).toBeNull();
    expect(partitionLabel({ attr: 'user', value: '' })).toBe('Without a person');
  });
  it("uses the gateway's state and forecast when it reports them", () => {
    const c = {
      ...counter('team', 't', '10'),
      state: 'at_risk' as const,
      forecast: FORECAST,
    };
    const [row] = buildTree([c], () => null, NOW);
    expect(row?.state).toBe('at_risk');
    expect(row?.projected).toBe(120);
    expect(row?.ratePerDay).toBe(1);
    expect(row?.runsOutAt).toBe('2026-06-28T00:00:00Z');
  });
  it('reads a counter without a forecast or an end as a rolling budget', () => {
    const c: BudgetCounter = { ...counter('team', 't', '30'), resets_at: null, state: 'on_pace' };
    const [row] = buildTree([c], () => null, NOW);
    expect(row?.rolling).toBe(true);
    expect(row?.projected).toBe(30);
    expect(row?.runsOutAt).toBeNull();
    expect(row?.ratePerDay).toBeNull();
    const lifetime = buildTree(
      [{ ...c, forecast: { ...FORECAST, projected_spend: '30', runs_out_at: null } }],
      () => null,
      NOW,
    );
    expect(lifetime[0]?.rolling).toBe(false);
  });
});

describe('rolling windows, downgrades and partitions in the form', () => {
  it('declares a rolling budget with the sliding window and clears it again', () => {
    const rolling = toDeclarations({
      ...EMPTY_BUDGET,
      owner: 'core',
      amount: '50',
      period: 'rolling',
      window: '7d',
      cacheBypass: true,
    });
    expect(rolling.monthly).toMatchObject({
      period: '7d',
      algorithm: 'sliding_window',
      cache_hits_bypass_budgets: true,
    });
    const monthly = toDeclarations({ ...EMPTY_BUDGET, owner: 'core', amount: '50' });
    expect(monthly.monthly).toMatchObject({
      period: 'monthly',
      algorithm: null,
      precision: null,
      cache_hits_bypass_budgets: null,
      downgrade: null,
    });
    expect(
      fromDeclaration(
        'team',
        'core',
        'r',
        { amount: '5', period: '24h', algorithm: 'sliding_window' },
        undefined,
      ),
    ).toMatchObject({ period: 'rolling', window: '24h' });
  });
  it('downgrades to a Model or a tier, and needs the Model', () => {
    const d = toDeclarations({
      ...EMPTY_BUDGET,
      owner: 'core',
      amount: '5',
      enforcement: 'downgrade',
      downgradeTo: 'model',
      downgradeModel: 'gpt-4o-mini',
    });
    expect(d.monthly?.downgrade).toEqual({ at: 0.9, to_model: 'gpt-4o-mini', to_tier: null });
    expect(
      fromDeclaration(
        'team',
        'core',
        'monthly',
        { amount: '5', downgrade: { at: 0.8, to_model: 'small' } },
        undefined,
      ),
    ).toMatchObject({
      enforcement: 'downgrade',
      downgradeTo: 'model',
      downgradeModel: 'small',
      downgradeAt: '80',
    });
    expect(
      budgetSchema.safeParse({
        ...EMPTY_BUDGET,
        owner: 'core',
        amount: '5',
        enforcement: 'downgrade',
        downgradeModel: '',
      }).success,
    ).toBe(false);
  });
  it('sets and removes a partition value of its own', () => {
    expect(toOverride('seats', 'ada', '6')).toEqual({ seats: { overrides: { ada: '6' } } });
    expect(toOverride('seats', 'ada', null)).toEqual({ seats: { overrides: { ada: null } } });
  });
  it('words the Models a budget covers', () => {
    expect(modelsCovered(undefined).label).toBe('All Models');
    expect(
      modelsCovered({ match: { models: ['gpt-4o*'] }, exclude: { models: ['gpt-4o-mini'] } }).label,
    ).toBe('gpt-4o* (not gpt-4o-mini)');
  });
});

describe('overage, re-costing and people', () => {
  it('ends an approval at the period end at the latest', () => {
    expect(grantExpiry('2026-06-20', '2026-07-01T00:00:00Z')).toBe('2026-06-20T23:59:59Z');
    expect(grantExpiry('2026-07-05', '2026-07-01T00:00:00Z')).toBe('2026-07-01T00:00:00Z');
    expect(grantExpiry('2026-07-05', null)).toBe('2026-07-05T23:59:59Z');
  });
  it('re-costs whole days', () => {
    expect(recostRange('2026-06-01', '2026-06-03')).toEqual({
      from: '2026-06-01T00:00:00Z',
      to: '2026-06-04T00:00:00Z',
    });
  });
  it('marks people spending over twice their team mean', () => {
    const user = (name: string, team: string, kind: 'human' | 'service' = 'human') =>
      ({ name, spec: { kind, primary_team: team }, metadata: {} }) as never;
    const people = peopleFacts(
      [user('a', 't'), user('b', 't'), user('c', 't'), user('d', 't', 'service')],
      new Map([
        ['a', { requests: 10, cost: 10 }],
        ['b', { requests: 10, cost: 1 }],
        ['c', { requests: 10, cost: 1 }],
      ]),
    );
    expect(people[0]?.name).toBe('a');
    expect(people[0]?.outlier).toBe(true);
    expect(people[0]?.perRequest).toBe(1);
    expect(people.find((p) => p.name === 'b')?.outlier).toBe(false);
    expect(people.find((p) => p.name === 'd')).toMatchObject({ service: true, perRequest: null });
  });
});
