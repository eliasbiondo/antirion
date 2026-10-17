'use client';

import Link from 'next/link';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Meter, type ChartTone } from '@/components/ui/charts';
import { Panel } from '@/components/ui/layout';
import { EmptyAction, EmptyState, LoadingRows } from '@/components/ui/states';
import type { BudgetCounter } from '@/lib/api/types';
import { formatCount, formatMoney, formatTokens, plural } from '@/lib/format';

import type { Totals } from '../store';

export interface TeamBudgetRow {
  team: string;
  spent: number;
  amount: number;
  projected: number;
  /** Projected spend as a share of the amount. */
  ratio: number;
  state: BudgetCounter['state'];
  requests: number | null;
  tokens: number | null;
}

/** Each team's monthly dollar budget with its run-rate projection, joined with the team's usage. */
export function teamBudgetRows(
  budgets: Map<string, BudgetCounter>,
  usage: { name: string; totals: Totals }[],
): TeamBudgetRow[] {
  const byTeam = new Map(usage.map((u) => [u.name, u.totals]));
  return [...budgets.entries()]
    .map(([team, c]) => {
      const amount = Number(c.effective_amount);
      const spent = Number(c.spent);
      const projected = Number(c.forecast?.projected_spend ?? c.spent);
      const t = byTeam.get(team);
      return {
        team,
        spent,
        amount,
        projected,
        ratio: amount > 0 ? projected / amount : 0,
        state: c.state,
        requests: t ? t.requests : null,
        tokens: t ? t.input + t.output : null,
      };
    })
    .sort((a, b) => b.ratio - a.ratio);
}

function toneOf(r: TeamBudgetRow): ChartTone {
  if (r.state === 'blocked' || r.ratio > 1) return 'error';
  if (r.state === 'at_risk' || r.ratio > 0.85) return 'warn';
  return 'accent';
}

const TEXT: Record<ChartTone, string> = {
  accent: 'text-fg',
  warn: 'text-warn',
  error: 'text-error',
  success: 'text-success',
  info: 'text-info',
  muted: 'text-muted',
};

/** Team run-rate against the monthly budget: spent and projected, the projected share, and how many go over. */
export function TeamBudgetsPanel({
  rows,
  loading,
  allowed,
}: {
  rows: TeamBudgetRow[];
  loading: boolean;
  allowed: boolean;
}) {
  const budgetsHref = useScopedHref('/budgets/');
  const over = rows.filter((r) => r.ratio > 1).length;
  return (
    <Panel
      title="Team budgets"
      subtitle="This month, projected at the current run-rate"
      actions={
        rows.length > 0 ? <Badge tone={over > 0 ? 'error' : 'neutral'}>{over} projected over</Badge> : null
      }
    >
      {!allowed ? (
        <EmptyState
          size="sm"
          title="You don't have access to budgets"
          description="Team budgets aren't shown."
        />
      ) : loading ? (
        <LoadingRows rows={4} label="Loading team budgets" />
      ) : rows.length === 0 ? (
        <EmptyState
          size="sm"
          title="No team has a monthly budget in dollars"
          action={<EmptyAction href={budgetsHref}>Set one in Budgets</EmptyAction>}
        />
      ) : (
        <ul aria-label="Team budgets" className="flex flex-col gap-2.5">
          {rows.slice(0, 8).map((r) => {
            const tone = toneOf(r);
            return (
              <li
                key={r.team}
                className="grid grid-cols-[minmax(0,176px)_minmax(80px,1fr)_auto_52px] items-center gap-3 text-ui max-sm:grid-cols-[minmax(0,1fr)_auto_44px] max-sm:gap-y-1"
              >
                <span className="min-w-0">
                  <span className="block truncate text-fg">{r.team}</span>
                  <span className="mono block truncate text-caption text-dim">
                    {r.requests !== null
                      ? `${formatCount(r.requests, { compact: true })} req · ${formatTokens(r.tokens)} tok`
                      : 'No requests'}
                  </span>
                </span>
                <Meter
                  className="max-sm:order-last max-sm:col-span-3"
                  value={r.spent}
                  max={r.amount}
                  projected={r.projected}
                  tone={tone}
                  label={`${r.team}: ${formatMoney(r.spent)} of ${formatMoney(r.amount)}, projected ${formatMoney(r.projected)}`}
                />
                <span className="mono text-right text-caption text-muted tabular-nums">
                  <span className="text-fg">{formatMoney(r.spent, { compact: true })}</span> /{' '}
                  {formatMoney(r.amount, { compact: true })}
                </span>
                <span className={`mono text-right text-caption font-semibold tabular-nums ${TEXT[tone]}`}>
                  {Math.round(r.ratio * 100)}%
                </span>
              </li>
            );
          })}
          {rows.length > 8 ? (
            <li className="text-caption text-muted">
              {plural(rows.length - 8, 'more team')} in{' '}
              <Link href={budgetsHref} className="text-accent hover:underline">
                Budgets
              </Link>
            </li>
          ) : null}
        </ul>
      )}
    </Panel>
  );
}
