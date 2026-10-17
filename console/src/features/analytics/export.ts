import { exportCsv } from '@/lib/csv';

import type { ProviderPressure } from './api';
import type { Totals } from './store';
import type { TeamBudgetRow } from './components/team-budgets-panel';

type Group = { name: string; totals: Totals };

const ratio = (a: number, b: number) => (b > 0 ? a / b : null);

/** The CSV exports of the Analytics tables: the rows each table is built from, every row. */
export const analyticsCsv = {
  models: (rows: Group[], suffix: string) =>
    exportCsv(
      `analytics-models-${suffix}`,
      [
        { header: 'Model', value: (r: Group) => r.name },
        { header: 'Requests', value: (r) => r.totals.requests },
        { header: 'Errors', value: (r) => r.totals.errors },
        { header: 'Spend (USD)', value: (r) => r.totals.cost.toFixed(6) },
        { header: 'Input tokens', value: (r) => r.totals.input },
        { header: 'Output tokens', value: (r) => r.totals.output },
        {
          header: 'p95 latency (ms)',
          value: (r) => (r.totals.p95 === null ? null : Math.round(r.totals.p95)),
        },
        {
          header: 'Cost per request (USD)',
          value: (r) => ratio(r.totals.cost, r.totals.requests)?.toFixed(6),
        },
      ],
      rows,
    ),
  providers: (rows: Group[], suffix: string) =>
    exportCsv(
      `analytics-providers-${suffix}`,
      [
        { header: 'Provider', value: (r: Group) => r.name },
        { header: 'Requests', value: (r) => r.totals.requests },
        {
          header: 'Success rate',
          value: (r) => ratio(r.totals.requests - r.totals.errors, r.totals.requests)?.toFixed(4),
        },
        {
          header: 'p95 latency (ms)',
          value: (r) => (r.totals.p95 === null ? null : Math.round(r.totals.p95)),
        },
        {
          header: 'p99 latency (ms)',
          value: (r) => (r.totals.p99 === null ? null : Math.round(r.totals.p99)),
        },
        { header: 'Spend (USD)', value: (r) => r.totals.cost.toFixed(6) },
      ],
      rows,
    ),
  teams: (rows: Group[], suffix: string) =>
    exportCsv(
      `analytics-teams-${suffix}`,
      [
        { header: 'Team', value: (r: Group) => r.name },
        { header: 'Requests', value: (r) => r.totals.requests },
        { header: 'Cache hits', value: (r) => r.totals.cacheHits },
        { header: 'Cache hit rate', value: (r) => ratio(r.totals.cacheHits, r.totals.requests)?.toFixed(4) },
        { header: 'Saved by caching (USD)', value: (r) => r.totals.saved.toFixed(6) },
        { header: 'Spend (USD)', value: (r) => r.totals.cost.toFixed(6) },
      ],
      rows,
    ),
  teamBudgets: (rows: TeamBudgetRow[]) =>
    exportCsv(
      'analytics-team-budgets',
      [
        { header: 'Team', value: (r: TeamBudgetRow) => r.team },
        { header: 'Spent (USD)', value: (r) => r.spent.toFixed(2) },
        { header: 'Budget (USD)', value: (r) => r.amount.toFixed(2) },
        { header: 'Projected (USD)', value: (r) => r.projected.toFixed(2) },
        { header: 'Projected share', value: (r) => r.ratio.toFixed(4) },
        { header: 'State', value: (r) => r.state },
      ],
      rows,
    ),
  pressure: (p: ProviderPressure, suffix: string) =>
    exportCsv(
      `analytics-provider-pressure-${suffix}`,
      [
        {
          header: 'Provider',
          value: (r: { provider: string; b: ProviderPressure['items'][number]['buckets'][number] }) =>
            r.provider,
        },
        { header: 'Bucket start', value: (r) => r.b.start },
        { header: 'Request quota used', value: (r) => r.b.rpm_utilization },
        { header: 'Token quota used', value: (r) => r.b.tpm_utilization },
        { header: 'Requests per minute limit', value: (r) => r.b.requests_limit },
        { header: 'Tokens per minute limit', value: (r) => r.b.tokens_limit },
      ],
      p.items.flatMap((i) => i.buckets.map((b) => ({ provider: i.provider, b }))),
    ),
  dailySpend: (rows: { label: string; spent: number | null }[]) =>
    exportCsv(
      'analytics-daily-spend',
      [
        { header: 'Day', value: (r: { label: string; spent: number | null }) => r.label },
        { header: 'Spend (USD)', value: (r) => (r.spent === null ? null : r.spent.toFixed(6)) },
      ],
      rows.filter((r) => r.spent !== null),
    ),
};
