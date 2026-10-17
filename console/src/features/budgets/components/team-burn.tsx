'use client';

import { useMemo } from 'react';

import { BarList, type ChartTone, Legend, TimeChart } from '@/components/ui/charts';
import { Panel } from '@/components/ui/layout';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { formatMoney } from '@/lib/format';
import { useNow } from '@/lib/hooks';

import { usePeople } from '../hooks';
import { trailingRate } from '../sources';
import { useBudgets } from '../store';
import { type BudgetRow, flatten } from '../types';

const TONES: ChartTone[] = ['accent', 'info', 'success', 'warn', 'muted'];

/**
 * The five teams with the largest projected spend this month: cumulative request costs to today, a
 * dashed line to month end at the trailing 7-day rate of the same costs, and a tick at the right
 * edge for each team's monthly budget.
 */
export function TeamBurn({ tree }: { tree: BudgetRow[] }) {
  const daily = useBudgets((s) => s.teamDaily);
  const at = useNow(60_000);
  const now = new Date(at);
  const days = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 0)).getUTCDate();
  const today = now.getUTCDate();
  const chart = useMemo(() => {
    if (!daily.data) return null;
    const date = new Date(at);
    const days = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 0)).getUTCDate();
    const today = date.getUTCDate();
    const budgetOf = new Map<string, BudgetRow>();
    for (const r of flatten(tree))
      if (
        r.level === 'team' &&
        r.kind === 'budget' &&
        !r.rolling &&
        r.counter.unit === 'usd' &&
        !budgetOf.has(r.owner)
      )
        budgetOf.set(r.owner, r);
    const lines = [...daily.data.entries()].map(([team, list]) => {
      const cumulative: (number | null)[] = [];
      const perDay: number[] = [];
      let sum = 0;
      for (let d = 1; d <= days; d += 1) {
        const cost = list.filter((x) => new Date(x.day).getUTCDate() === d).reduce((a, x) => a + x.cost, 0);
        perDay.push(cost);
        sum += cost;
        cumulative.push(d <= today ? sum : null);
      }
      const spent = cumulative[today - 1] ?? 0;
      const budget = budgetOf.get(team);
      // The projection continues the line it extends: request costs, never the budget counter's
      // rate, which also counts spend adjustments.
      const rate = trailingRate(perDay, today);
      const projection = Array.from({ length: days }, (_, i) =>
        i + 1 >= today ? spent + rate * (i + 1 - today) : null,
      );
      return {
        team,
        // A team is named by its slug where it identifies a series, as in tables and filters.
        name: team,
        cumulative,
        projection,
        end: spent + rate * (days - today),
        budget,
      };
    });
    lines.sort((a, b) => b.end - a.end);
    return lines.slice(0, 5).map((l, i) => ({ ...l, tone: TONES[i] ?? 'muted' }));
  }, [daily.data, tree, at]);

  const labels = Array.from(
    { length: days },
    (_, i) => `${now.toLocaleString('en-US', { month: 'short', timeZone: 'UTC' })} ${i + 1}`,
  );
  return (
    <Panel
      title="Team burn"
      subtitle="The five teams projected to spend the most this month, from request costs"
    >
      {daily.error ? <ErrorState error={daily.error} subject="Spend per team" size="sm" /> : null}
      {!daily.data && !daily.error ? <LoadingRows rows={3} /> : null}
      {chart?.length === 0 ? (
        <EmptyState size="sm" title="No team has spent anything this month yet" />
      ) : null}
      {chart && chart.length > 0 ? (
        <>
          <TimeChart
            kind="line"
            gaps
            title="Cumulative spend per team this month, with projections and budgets"
            labels={labels}
            height={220}
            formatValue={(v) => formatMoney(v, { compact: true })}
            marker={{ index: today - 1, label: 'Today' }}
            series={chart.flatMap((l) => [
              { name: l.name, values: l.cumulative, tone: l.tone },
              { name: `${l.name}, projected`, values: l.projection, tone: l.tone, dashed: true },
            ])}
            ticks={chart
              .filter((l) => l.budget)
              .map((l) => ({ value: l.budget?.amount ?? 0, label: `${l.name} budget`, tone: l.tone }))}
          />
          <Legend
            className="mt-2"
            items={chart.map((l) => ({
              label: l.name,
              tone: l.tone,
              value: `${formatMoney(l.end, { compact: true })}${l.budget ? ` of ${formatMoney(l.budget.amount, { compact: true })}` : ''}`,
            }))}
          />
        </>
      ) : null}
    </Panel>
  );
}

/** People ranked by spend this month. */
export function TopSpenders({ onOpen }: { onOpen: () => void }) {
  const people = usePeople();
  const error = useBudgets((s) => s.people.error);
  const top = (people ?? []).filter((p) => p.cost > 0).slice(0, 6);
  return (
    <Panel
      title="Top spenders"
      subtitle="People by spend this month"
      actions={
        top.length > 0 ? (
          <button type="button" onClick={onOpen} className="text-caption text-accent hover:underline">
            All people
          </button>
        ) : null
      }
    >
      {error ? <ErrorState error={error} subject="Spend per person" size="sm" /> : null}
      {!people && !error ? <LoadingRows rows={3} /> : null}
      {people && top.length === 0 ? (
        <p className="text-ui text-muted">
          No one has spent anything this month yet. Keys owned by a person count toward them.
        </p>
      ) : null}
      {top.length > 0 ? (
        <BarList
          label="Top spenders"
          format={(v) => formatMoney(v)}
          items={top.map((p) => ({
            name: p.displayName,
            value: p.cost,
            tone: p.outlier ? 'warn' : 'accent',
          }))}
        />
      ) : null}
    </Panel>
  );
}
