import { describe, expect, it } from 'vitest';

import type { BudgetCounter } from '@/lib/api/types';

import { monthCounters, monthProjection } from '../projection';

const NOW = Date.parse('2026-10-16T12:00:00Z');

function counter(level: BudgetCounter['level'], owner: string, spent: number, projected: number | null) {
  return {
    level,
    owner,
    name: 'monthly',
    partition: null,
    unit: 'usd',
    mode: 'hard',
    amount: '1000',
    effective_amount: '1000',
    spent: String(spent),
    reserved: '0',
    remaining: String(1000 - spent),
    period_start: '2026-10-01T00:00:00Z',
    resets_at: '2026-11-01T00:00:00Z',
    ...(projected === null
      ? {}
      : {
          forecast: {
            rate_per_day: '1',
            projected_spend: String(projected),
            projected_ratio: null,
            runs_out_at: null,
            basis_from: '2026-10-09T00:00:00Z',
            basis_to: '2026-10-16T00:00:00Z',
          },
        }),
  } as BudgetCounter;
}

describe('one month-end projection', () => {
  it("reads the counted budgets' own forecasts, the same for every screen", () => {
    const counters = [counter('organization', 'default', 60, 121.67), counter('team', 'platform', 40, 90)];
    const gateway = { kind: 'gateway' } as const;
    expect(monthCounters(counters, gateway).map((c) => c.owner)).toEqual(['default']);
    expect(monthProjection(counters, gateway, 62.8, NOW)).toMatchObject({
      spent: 60,
      projected: 121.67,
      amount: 1000,
      source: 'counters',
    });
    expect(monthProjection(counters, { kind: 'organization', name: 'default' }, null, NOW)?.projected).toBe(
      121.67,
    );
  });

  it('falls back to the month at its rate without budgets, and never projects below what is spent', () => {
    const half = monthProjection([], { kind: 'gateway' }, 50, NOW);
    expect(half?.source).toBe('usage');
    expect(half?.projected).toBeGreaterThan(50);
    expect(
      monthProjection([counter('organization', 'default', 80, 10)], { kind: 'gateway' }, null, NOW)
        ?.projected,
    ).toBe(80);
    expect(monthProjection(null, { kind: 'gateway' }, null, NOW)).toBeNull();
  });
});
