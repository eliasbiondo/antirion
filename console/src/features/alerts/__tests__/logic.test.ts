import { describe, expect, it } from 'vitest';

import type { Alert, AlertChannelResource, UsageBucket } from '@/lib/api/types';

import { activityWords, thresholdInput, thresholdValue } from '../describe';
import { covers, eventWords } from '../events';
import {
  channelErrors,
  channelForm,
  channelSpec,
  REDACTED,
  ruleErrors,
  ruleForm,
  ruleSpec,
  suggestName,
} from '../form';
import { foldBuckets, previewInterval, simulate } from '../preview';
import {
  budgetBurn,
  DAY_MS,
  fireBuckets,
  meanTimeToResolve,
  noiseRatio,
  notificationsSince,
  ruleStats,
} from '../stats';
import { RULE_TEMPLATES } from '../templates';

const NOW = Date.UTC(2026, 9, 14, 12, 0, 0);
const iso = (t: number) => new Date(t).toISOString();

function alert(over: Partial<Alert> & { fired: number; lasted?: number }): Alert {
  const { fired, lasted, ...rest } = over;
  return {
    id: `a-${fired}`,
    rule: 'r',
    fingerprint: 'fp',
    category: 'errors',
    severity: 'warning',
    status: lasted === undefined ? 'firing' : 'resolved',
    fired_at: iso(fired),
    resolved_at: lasted === undefined ? null : iso(fired + lasted * 1000),
    summary: 's',
    ...rest,
  };
}

describe('30-day figures', () => {
  const list = [
    alert({ fired: NOW - 2 * DAY_MS, lasted: 600 }),
    alert({ fired: NOW - 3 * DAY_MS, lasted: 30 }),
    alert({ fired: NOW - 40 * DAY_MS, lasted: 1200 }),
    alert({ fired: NOW - 60_000 }),
  ];

  it('averages time to resolve over the window the resolution falls in', () => {
    expect(meanTimeToResolve(list, NOW - 30 * DAY_MS, NOW)).toBe(315);
    expect(meanTimeToResolve(list, NOW - 60 * DAY_MS, NOW - 30 * DAY_MS)).toBe(1200);
    expect(meanTimeToResolve([], 0, NOW)).toBeNull();
  });

  it('counts noise as the fires that cleared within a minute', () => {
    expect(noiseRatio(list, NOW - 30 * DAY_MS, NOW)).toBeCloseTo(1 / 3);
    expect(noiseRatio([], 0, NOW)).toBeNull();
  });

  it('counts notifications by channel since a time', () => {
    const a = alert({
      fired: NOW - 3_600_000,
      timeline: [
        { time: iso(NOW - 3_600_000), kind: 'notified', detail: { channel: 'pager' } },
        { time: iso(NOW - 3_500_000), kind: 'notified', detail: { channel: 'slack' } },
        { time: iso(NOW - 3_400_000), kind: 'notified', detail: { channel: 'slack' } },
        { time: iso(NOW - 3_300_000), kind: 'notification_failed', detail: { channel: 'slack' } },
        { time: iso(NOW - 3 * DAY_MS), kind: 'notified', detail: { channel: 'old' } },
      ],
    });
    expect(notificationsSince([a], NOW - DAY_MS)).toEqual({
      total: 3,
      failed: 1,
      byChannel: [
        { channel: 'slack', n: 2 },
        { channel: 'pager', n: 1 },
      ],
    });
  });

  it('stacks fires by severity per hour or per day', () => {
    const hours = fireBuckets(
      [alert({ fired: NOW - 30 * 60_000, severity: 'critical' }), alert({ fired: NOW - 5 * 3_600_000 })],
      '24h',
      NOW,
    );
    expect(hours).toHaveLength(24);
    expect(hours.at(-1)?.counts.critical).toBe(1);
    expect(hours.reduce((s, b) => s + b.counts.warning, 0)).toBe(1);
    expect(fireBuckets(list, '30d', NOW)).toHaveLength(30);
  });

  it("sums a rule's fires, notifications and time to resolve over 30 days", () => {
    const stats = ruleStats(
      [
        alert({
          fired: NOW - DAY_MS,
          lasted: 120,
          timeline: [{ time: iso(NOW - DAY_MS), kind: 'notified', detail: { channel: 'x' } }],
        }),
        alert({ fired: NOW - 45 * DAY_MS, lasted: 60 }),
        alert({ fired: NOW - DAY_MS, lasted: 60, rule: 'other' }),
      ],
      'r',
      NOW,
    );
    expect(stats).toMatchObject({ fires: 1, notifications: 1, mttr: 120 });
  });

  it('draws the error budget left as failures accumulate', () => {
    const burn = budgetBurn(
      [
        { requests: 1000, availability: 1 },
        { requests: 1000, availability: 0.999 },
        { requests: 1000, availability: 0.998 },
      ],
      0.999,
    );
    expect(burn).toHaveLength(3);
    expect(burn[0]).toBe(1);
    expect(burn[1]).toBeCloseTo(2 / 3);
    expect(burn[2]).toBeCloseTo(0);
  });
});

describe('rule preview', () => {
  const step = 300_000;
  const start = NOW - 7 * DAY_MS;
  function rows(values: number[]): UsageBucket[] {
    return values.map((errors, i) => ({
      start: iso(start + i * step),
      group: {},
      requests: 100,
      errors,
      cost_usd: '0.10',
    }));
  }

  it('reads 5-minute buckets for short windows and hours for long ones', () => {
    expect(previewInterval('5m').interval).toBe('5m');
    expect(previewInterval('30m').interval).toBe('15m');
    expect(previewInterval('6h').interval).toBe('1h');
  });

  it('fires when the condition holds, once per episode, and respects the cooldown', () => {
    const spec = {
      category: 'errors' as const,
      metric: 'error_rate' as const,
      operator: 'gt' as const,
      threshold: 0.05,
      window: '5m',
      cooldown: '0s',
      resolve_after: '5m',
    };
    const data = foldBuckets(rows([0, 10, 10, 0, 0, 10, 0]));
    expect(simulate(spec, data, step, NOW).fires).toEqual([1, 5]);
    expect(simulate({ ...spec, cooldown: '1h' }, data, step, NOW).fires).toEqual([1]);
    expect(simulate({ ...spec, for: '10m' }, data, step, NOW).fires).toEqual([2]);
    const r = simulate(spec, data, step, NOW);
    expect(r.held).toBe(3);
    expect(r.values[1]).toBeCloseTo(0.1);
  });

  it('adds up 5xx rows of a read by status class', () => {
    const folded = foldBuckets([
      { start: iso(start), group: { status_class: '2xx' }, requests: 90, errors: 0, cost_usd: '0' },
      { start: iso(start), group: { status_class: '5xx' }, requests: 10, errors: 10, cost_usd: '0' },
    ]);
    expect(folded).toHaveLength(1);
    expect(folded[0]).toMatchObject({ requests: 100, server_errors: 10 });
  });

  it('divides a baseline ratio by the same hour a week earlier', () => {
    const week = 7 * 24 * 12;
    const values = Array.from({ length: week + 2 }, (_, i) => (i >= week ? 30 : 10));
    const data = foldBuckets(
      values.map((errors, i) => ({
        start: iso(NOW - (week + 2) * step + i * step),
        group: {},
        requests: 100,
        errors,
        cost_usd: '1',
        latency_ms: { p95: errors * 100 },
      })),
    );
    const r = simulate(
      {
        category: 'latency',
        metric: 'latency_baseline_ratio',
        operator: 'gt',
        threshold: 2,
        window: '5m',
        cooldown: '0s',
      },
      data,
      step,
      NOW,
    );
    expect(r.values.at(-1)).toBeCloseTo(3);
    expect(r.fires).toHaveLength(1);
  });
});

describe('rule form', () => {
  it('turns percentages typed by people into ratios and back', () => {
    expect(thresholdInput('error_rate', 0.02)).toBe('2');
    expect(thresholdValue('error_rate', '2')).toBe(0.02);
    expect(thresholdValue('p95_latency_ms', '1,500')).toBe(1500);
    expect(thresholdValue('error_rate', 'abc')).toBeNull();
  });

  it('builds a spec from a template with the maintenance silence and scope', () => {
    const template = RULE_TEMPLATES.find((t) => t.id === 'provider-errors');
    if (!template) throw new Error('template');
    const form = ruleForm('provider-errors', template.spec);
    form.scope.provider = 'openai';
    form.silence = true;
    form.channels = ['pager'];
    expect(ruleSpec(form)).toMatchObject({
      kind: 'metric',
      metric: 'upstream_5xx_rate',
      threshold: 0.05,
      scope: { provider: 'openai' },
      silence_during_maintenance: true,
      channels: ['pager'],
    });
    expect(ruleSpec(form).events).toBeUndefined();
  });

  it('keeps an event rule to its events', () => {
    const spec = ruleSpec(ruleForm('x', { kind: 'event', category: 'budget', events: ['budget.*'] }));
    expect(spec).toMatchObject({ kind: 'event', events: ['budget.*'] });
    expect(spec.metric).toBeUndefined();
  });

  it('says what to fix before sending', () => {
    const form = ruleForm('Bad Name', { category: 'errors', metric: 'error_rate' });
    const errors = ruleErrors(form, null, []);
    expect(errors.name).toBeTruthy();
    expect(errors.threshold).toBe('Enter a number');
    expect(ruleErrors(ruleForm('ok', { kind: 'event', category: 'budget' }), null, [])).toMatchObject({
      events: 'Choose at least one event',
    });
    expect(suggestName('error-rate', ['error-rate'])).toBe('error-rate-2');
  });

  it('keeps a stored channel secret when the field is left empty', () => {
    const existing: AlertChannelResource = {
      name: 'slack',
      spec: { type: 'slack', slack: { webhook_url: REDACTED } },
      metadata: { etag: '"1"', generation: 1, created_at: iso(NOW), updated_at: iso(NOW), managed_by: 'api' },
    };
    const form = channelForm(existing);
    expect(channelErrors(form, existing, ['slack'])).toEqual({});
    expect(channelSpec(form, existing).slack?.webhook_url).toBe(REDACTED);
    expect(channelErrors({ ...form, type: 'pagerduty' }, existing, ['slack'])).toHaveProperty('routingKey');
    expect(
      channelErrors({ ...channelForm(null), name: 'mail', type: 'email', emails: ['nope'] }, null, []),
    ).toHaveProperty('emails');
  });
});

describe('events', () => {
  it('matches subscriptions as the gateway does', () => {
    expect(covers('budget.*', 'budget.exhausted')).toBe(true);
    expect(covers('budget.*', 'budgets.x')).toBe(false);
    expect(covers('*', 'config.applied')).toBe(true);
    expect(covers('config.applied', 'config.rejected')).toBe(false);
    expect(eventWords('budget.*')).toBe('Every budget event');
    expect(eventWords('deployment.state_changed')).toBe('Deployment state changed');
  });

  it('describes the end of a snooze in the activity', () => {
    expect(activityWords('snoozed', { until: null })).toBe('Snooze ended');
    expect(activityWords('snoozed', { until: iso(NOW + 900_000) }, iso(NOW))).toBe('Snoozed for 15 min');
  });
});
