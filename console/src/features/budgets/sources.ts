/**
 * Budgets read spend from two places: the budget counters (what admission checks, spend
 * adjustments included) and request costs from usage analytics (the charts). These helpers keep
 * each view on one source and say when the two differ.
 */

/** Spend per day over the trailing week to `today` (1-based day of the month), from daily costs. */
export function trailingRate(perDay: number[], today: number): number {
  const days = Math.min(7, today);
  if (days <= 0) return 0;
  let sum = 0;
  for (let d = today - days; d < today; d += 1) sum += perDay[d] ?? 0;
  return sum / days;
}

/**
 * How much more (or less) the budget counters hold than the request costs of the same period, or
 * null when the two agree (within a dollar or 1 %).
 */
export function counterGap(counterSpent: number, requestSpent: number): number | null {
  const gap = counterSpent - requestSpent;
  return Math.abs(gap) < Math.max(1, Math.abs(counterSpent) * 0.01) ? null : gap;
}

/** A budget counter the hero counts: its level and owner. */
export interface CountedOwner {
  level: string;
  owner: string;
}

/**
 * The running total of the counted budgets this month, as their counters hold it: each day's request
 * costs charged to them plus the spend adjustments made on them that day (a step on that day),
 * up to today; and the projection from today's total to the counters' own month-end projection.
 * `costs` and `adjustments` are per day of the month, 0-based.
 */
export function countedSeries(
  costs: readonly number[],
  adjustments: readonly number[],
  days: number,
  today: number,
  projectedEnd: number,
): { actual: (number | null)[]; projected: (number | null)[]; spent: number } {
  const actual: (number | null)[] = [];
  let sum = 0;
  for (let d = 0; d < days; d += 1) {
    sum += d < today ? (costs[d] ?? 0) + (adjustments[d] ?? 0) : 0;
    actual.push(d < today ? sum : null);
  }
  const spent = actual[today - 1] ?? 0;
  const end = Math.max(spent, projectedEnd);
  const remaining = days - today;
  const projected = Array.from({ length: days }, (_, d) =>
    d + 1 < today ? null : remaining > 0 ? spent + ((end - spent) * (d + 1 - today)) / remaining : spent,
  );
  return { actual, projected, spent };
}
