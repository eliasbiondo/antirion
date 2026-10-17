import { describe, expect, it } from 'vitest';

import type { Alert, BudgetCounter, LogRecord, UsageBucket } from '@/lib/api/types';

import type { ProviderPressure } from '../api';
import { pressureRows } from '../components/pressure-panel';
import { reviewParams, scopeText } from '../components/signals-panel';
import { teamBudgetRows } from '../components/team-budgets-panel';
import { mostCommonTransition } from '../extras';
import { totals } from '../store';

const bucket = (start: string, rpm: number | null, tpm: number | null) => ({
  start,
  rpm_utilization: rpm,
  tpm_utilization: tpm,
  requests_limit: rpm === null ? null : 600,
  tokens_limit: tpm === null ? null : 100_000,
});

describe('analytics extras', () => {
  it('reads pressure per provider: newest reading, peak, the quota it is, and hot at the threshold', () => {
    const p: ProviderPressure = {
      from: '2026-10-14T00:00:00Z',
      to: '2026-10-14T00:03:00Z',
      interval: '1m',
      threshold: 0.8,
      items: [
        {
          provider: 'a',
          buckets: [
            bucket('2026-10-14T00:00:00Z', 0.9, 0.1),
            bucket('2026-10-14T00:01:00Z', 0.5, 0.6),
            bucket('2026-10-14T00:02:00Z', null, null),
          ],
        },
        { provider: 'b', buckets: [bucket('2026-10-14T00:00:00Z', 0.2, null)] },
      ],
      hot: [],
    };
    const rows = pressureRows(p);
    expect(rows[0]).toMatchObject({
      provider: 'a',
      current: 0.6,
      unit: 'tokens',
      used: 60_000,
      limit: 100_000,
      peak: 0.9,
      hot: true,
    });
    expect(rows[1]).toMatchObject({ provider: 'b', current: 0.2, unit: 'requests', used: 120, hot: false });
  });

  it('joins team budgets with team usage and sorts by the projected share', () => {
    const counter = (owner: string, spent: string, projected: string): BudgetCounter =>
      ({
        owner,
        spent,
        effective_amount: '100',
        forecast: { projected_spend: projected },
        state: 'on_pace',
      }) as unknown as BudgetCounter;
    const t = totals([
      { start: 'x', group: {}, requests: 7, errors: 0, cost_usd: '1', input: 10, output: 5 },
    ]);
    const rows = teamBudgetRows(
      new Map([
        ['a', counter('a', '40', '80')],
        ['b', counter('b', '90', '130')],
      ]),
      t ? [{ name: 'a', totals: t }] : [],
    );
    expect(rows.map((r) => [r.team, r.ratio, r.requests, r.tokens])).toEqual([
      ['b', 1.3, null, null],
      ['a', 0.8, 7, 15],
    ]);
  });

  it('finds the most common fallback from usage by deployment and the attempt trails', async () => {
    const usage = [
      { group: { deployment: 'x' }, fallbacks: 2 },
      { group: { deployment: 'y' }, fallbacks: 9 },
    ] as unknown as UsageBucket[];
    const rec = (...deps: string[]) =>
      ({ attempts: deps.map((deployment) => ({ deployment })) }) as unknown as LogRecord;
    let asked = '';
    const found = await mostCommonTransition(usage, (to) => {
      asked = to;
      return Promise.resolve([rec('a', 'y'), rec('b', 'y'), rec('a', 'a', 'y'), rec('y')]);
    });
    expect(asked).toBe('y');
    expect(found).toEqual({ from: 'a', to: 'y', count: 2, served: 9 });
    expect(await mostCommonTransition([], () => Promise.resolve([]))).toBeNull();
  });

  it("opens the request log on a signal's scope and window", () => {
    const alert = {
      fired_at: '2026-10-14T12:00:00Z',
      resolved_at: null,
      scope: { provider: 'openai', route: 'r' },
    } as unknown as Alert;
    const now = Date.parse('2026-10-14T12:30:00Z');
    const params = reviewParams(alert, null, now);
    expect(params.provider).toBe('openai');
    expect(params.route).toBeUndefined();
    expect(params.range).toBe(`${Date.parse('2026-10-14T11:30:00Z')}-${now}`);
  });

  it("reviews a budget signal's owner, and words its scope without internal ids", () => {
    const alert = {
      fired_at: '2026-10-14T12:00:00Z',
      resolved_at: null,
      scope: { budget: 'project/zendesk-assistant/monthly' },
    } as unknown as Alert;
    expect(reviewParams(alert, null, Date.parse('2026-10-14T12:30:00Z')).project).toBe('zendesk-assistant');
    expect(scopeText({ budget: 'project/zendesk-assistant/monthly' })).toBe(
      'project zendesk-assistant · budget monthly',
    );
    expect(scopeText({ key: 'copilot-prod', anomaly: '01a141cb-bf5c' })).toBe('key copilot-prod');
  });
});
