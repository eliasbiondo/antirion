import type { BudgetCounter } from '@/lib/api/types';
import type { Scope } from '@/stores/scope';

const DAY = 86_400_000;

/** Whether a counter's period is a calendar month. */
export function isMonthlyCounter(c: BudgetCounter): boolean {
  if (!c.resets_at) return false;
  const days = (new Date(c.resets_at).getTime() - new Date(c.period_start).getTime()) / DAY;
  return days >= 27.5 && days <= 31.5;
}

/** The month so far as a fraction of the month. */
export function monthFraction(now: number): number {
  const d = new Date(now);
  const start = Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1);
  const end = Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1);
  return Math.min(1, Math.max(1 / (end - start), (now - start) / (end - start)));
}

/**
 * The monthly dollar budgets that count a scope's month: the gateway's own (or the organization's
 * when the scope is one); without it, every organization's; without those, the top-level team
 * budgets. Overview, Analytics and Budgets count the month with the same ones.
 */
export function monthCounters(counters: BudgetCounter[] | null, scope: Scope): BudgetCounter[] {
  const list = (counters ?? []).filter(
    (c) => c.unit === 'usd' && c.partition === null && isMonthlyCounter(c),
  );
  const at = (level: BudgetCounter['level']) => list.filter((c) => c.level === level);
  if (scope.kind === 'organization') {
    const own = list.filter((c) => c.level === 'organization' && c.owner === scope.name);
    return own.length > 0 ? own : at('team');
  }
  for (const level of ['gateway', 'organization', 'team'] as const)
    if (at(level).length > 0) return at(level);
  return [];
}

export interface MonthProjection {
  /** Spent this month, as the counters count it (or as usage does without them). */
  spent: number;
  /** The month-end projection. */
  projected: number;
  /** The budgets' amount, when the month is counted by budgets. */
  amount: number | null;
  source: 'counters' | 'usage';
}

/**
 * The one month-end projection every screen shows: the counted budgets' own forecasts
 * (`forecast.projected_spend`, which the gateway keeps flat for a blocked budget), else the month's
 * usage at its rate so far. So the same moment reads the same number everywhere.
 */
export function monthProjection(
  counters: BudgetCounter[] | null,
  scope: Scope,
  usageSpend: number | null,
  now: number,
): MonthProjection | null {
  const elapsed = monthFraction(now);
  const counted = monthCounters(counters, scope);
  if (counted.length > 0) {
    let spent = 0;
    let projected = 0;
    let amount = 0;
    for (const c of counted) {
      const s = Number(c.spent);
      const reported = c.forecast ? Number(c.forecast.projected_spend) : NaN;
      spent += s;
      projected += Number.isFinite(reported) ? reported : s / elapsed;
      amount += Number(c.effective_amount ?? c.amount);
    }
    return { spent, projected: Math.max(spent, projected), amount, source: 'counters' };
  }
  if (usageSpend === null) return null;
  return { spent: usageSpend, projected: usageSpend / elapsed, amount: null, source: 'usage' };
}
