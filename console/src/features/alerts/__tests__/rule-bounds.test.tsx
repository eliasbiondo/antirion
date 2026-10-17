import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { TimeChart } from '@/components/ui/charts';
import type { Alert } from '@/lib/api/types';

import { channelErrors, channelForm, EMPTY_RULE, ruleErrors } from '../form';
import { builtInRules, DAY_MS } from '../stats';

const NOW = Date.UTC(2026, 9, 14, 12, 0, 0);
const iso = (t: number) => new Date(t).toISOString();

function alert(id: string, rule: string, fired: number, severity: Alert['severity'] = 'warning'): Alert {
  return {
    id,
    rule,
    fingerprint: 'fp',
    category: 'security',
    severity,
    status: 'firing',
    fired_at: iso(fired),
    summary: 's',
  };
}

describe('rule threshold bounds', () => {
  const rule = { ...EMPTY_RULE, name: 'errors', metric: 'error_rate' as const };

  it('refuses an error rate above 100 % or below 0', () => {
    expect(ruleErrors({ ...rule, threshold: '150' }, null, []).threshold).toBe(
      'Enter a percentage from 0 to 100',
    );
    expect(ruleErrors({ ...rule, threshold: '-1' }, null, []).threshold).toBe(
      'Enter a percentage from 0 to 100',
    );
    expect(ruleErrors({ ...rule, metric: 'p95_latency_ms', threshold: '-1' }, null, []).threshold).toBe(
      'Enter a number from 0 up',
    );
    expect(ruleErrors({ ...rule, threshold: '100' }, null, [])).toEqual({});
    expect(ruleErrors({ ...rule, threshold: '2.5' }, null, [])).toEqual({});
  });

  it('lets a projected budget use go past 100 %', () => {
    expect(ruleErrors({ ...rule, metric: 'budget_projection_ratio', threshold: '150' }, null, [])).toEqual(
      {},
    );
  });
});

describe('Slack channel URL', () => {
  const slack = { ...channelForm(null), name: 'ops', type: 'slack' as const };

  it('must be a link', () => {
    expect(channelErrors({ ...slack, slackUrl: 'not-a-url' }, null, []).slackUrl).toBe(
      'Enter a link that starts with https:// or http://',
    );
    expect(channelErrors({ ...slack, slackUrl: 'https://hooks.slack.com/services/T/B/x' }, null, [])).toEqual(
      {},
    );
    expect(channelErrors(slack, null, []).slackUrl).toBe('Paste the incoming webhook URL from Slack');
  });
});

describe('builtInRules', () => {
  it('lists rule names on alerts that no configured rule carries, most recent first', () => {
    const list = [
      alert('a', 'key_anomaly', NOW - DAY_MS),
      alert('b', 'key_anomaly', NOW - 2 * DAY_MS, 'critical'),
      alert('c', 'budget_forecast', NOW - 3 * DAY_MS),
      alert('d', 'budget_forecast', NOW - 40 * DAY_MS),
      alert('e', 'mine', NOW),
      // The inbox and the history share instances.
      alert('a', 'key_anomaly', NOW - DAY_MS),
    ];
    expect(builtInRules(list, ['mine'], NOW)).toEqual([
      {
        name: 'key_anomaly',
        category: 'security',
        severity: 'warning',
        fires: 2,
        lastFired: iso(NOW - DAY_MS),
      },
      {
        name: 'budget_forecast',
        category: 'security',
        severity: 'warning',
        fires: 1,
        lastFired: iso(NOW - 3 * DAY_MS),
      },
    ]);
  });
});

describe('TimeChart gaps', () => {
  it('reads a missing bucket as no data, not zero, in its data table', () => {
    render(
      <TimeChart
        kind="line"
        title="Error rate"
        labels={['10:00', '10:05', '10:10']}
        series={[{ name: 'Error rate', values: [0.2, null, 0.1], tone: 'accent' }]}
        formatValue={(v) => `${Math.round(v * 100)}%`}
        gaps
      />,
    );
    const row = screen.getByRole('rowheader', { name: '10:05' }).closest('tr');
    expect(row?.textContent).toBe('10:05No data');
  });
});
