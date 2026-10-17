import type { Alert, AlertChannelResource, AlertRuleResource } from '@/lib/api/types';
import { capitalize, formatCount, formatMoney, parseDuration } from '@/lib/format';

type RuleSpec = AlertRuleResource['spec'];
type Metric = NonNullable<RuleSpec['metric']>;

export type MetricUnit = 'percent' | 'ms' | 'usd' | 'ratio' | 'count';

/** Each metric in words, the unit its threshold is entered in, and how the threshold reads. */
export const METRICS: Record<
  Metric,
  {
    label: string;
    unit: MetricUnit;
    value: (v: number) => string;
    baseline?: boolean;
    /** A share of a whole, so its threshold stays within 0–100 %. */
    share?: boolean;
  }
> = {
  p95_latency_ms: { label: 'p95 latency', unit: 'ms', value: (v) => `${formatCount(v)} ms` },
  p99_latency_ms: { label: 'p99 latency', unit: 'ms', value: (v) => `${formatCount(v)} ms` },
  ttft_p95_ms: { label: 'p95 time to first token', unit: 'ms', value: (v) => `${formatCount(v)} ms` },
  error_rate: { share: true, label: 'Error rate', unit: 'percent', value: percent },
  upstream_5xx_rate: { share: true, label: 'Provider error rate', unit: 'percent', value: percent },
  fallback_rate: { share: true, label: 'Fallback rate', unit: 'percent', value: percent },
  rpm_utilization: {
    share: true,
    label: 'Use of the requests-per-minute limit',
    unit: 'percent',
    value: percent,
  },
  tpm_utilization: {
    share: true,
    label: 'Use of the tokens-per-minute limit',
    unit: 'percent',
    value: percent,
  },
  cost_per_request_usd: { label: 'Cost per request', unit: 'usd', value: (v) => formatMoney(v) },
  spend_usd: { label: 'Spend', unit: 'usd', value: (v) => formatMoney(v) },
  budget_projection_ratio: { label: 'Projected budget use', unit: 'percent', value: percent },
  guardrail_events: { label: 'Guardrail events', unit: 'count', value: (v) => formatCount(v) },
  cache_hit_rate: { share: true, label: 'Cache hit rate', unit: 'percent', value: percent },
  latency_baseline_ratio: { label: 'Latency', unit: 'ratio', value: (v) => `${v}×`, baseline: true },
  cost_baseline_ratio: { label: 'Cost', unit: 'ratio', value: (v) => `${v}×`, baseline: true },
  requests: { label: 'Requests', unit: 'count', value: (v) => formatCount(v) },
};

/** The metric's threshold as the person types it: percentages as 5 for 0.05. */
export function thresholdInput(metric: Metric, threshold: number | undefined): string {
  if (threshold === undefined || !Number.isFinite(threshold)) return '';
  const v = METRICS[metric].unit === 'percent' ? threshold * 100 : threshold;
  return String(Number(v.toFixed(6)));
}

/** The typed threshold as the rule stores it, or null when it is not a number. */
export function thresholdValue(metric: Metric, text: string): number | null {
  const n = Number(text.trim().replace(/[%$×x,\s]/g, ''));
  if (text.trim() === '' || !Number.isFinite(n)) return null;
  return METRICS[metric].unit === 'percent' ? n / 100 : n;
}

/** The unit next to a threshold field. */
export const UNIT_WORD: Record<MetricUnit, string> = {
  percent: '%',
  ms: 'ms',
  usd: 'USD',
  ratio: '× baseline',
  count: '',
};

const SCOPE_ORDER = [
  'organization',
  'team',
  'project',
  'key',
  'provider',
  'deployment',
  'model',
  'route',
] as const;
export type ScopeDimension = (typeof SCOPE_ORDER)[number];
export const SCOPE_DIMENSIONS: readonly ScopeDimension[] = SCOPE_ORDER;

/** A rule's scope in words: "provider openai · team platform", or "Whole gateway". */
export function scopeWords(scope: RuleSpec['scope']): string {
  const parts = SCOPE_ORDER.filter((d) => scope?.[d]).map((d) => `${d} ${scope?.[d] ?? ''}`);
  return parts.length ? parts.join(' · ') : 'Whole gateway';
}

function percent(v: number): string {
  const p = v * 100;
  return `${Number.isInteger(p) ? p : p.toFixed(1)}%`;
}

const OPERATORS: Record<NonNullable<RuleSpec['operator']>, string> = {
  gt: 'is more than',
  gte: 'is at least',
  lt: 'is less than',
  lte: 'is at most',
};

/** A config duration ("5m") in words ("5 min"). */
export function durationWords(d: string | undefined): string | null {
  if (!d) return null;
  const s = parseDuration(d);
  return s === null ? d : spanWords(s);
}

const CATEGORY_WORDS: Record<string, string> = { pii: 'PII', errors: 'Errors', cost: 'Cost' };

/** A rule's category in words ("Latency", "PII"). */
export function categoryWord(category: string): string {
  return CATEGORY_WORDS[category] ?? capitalize(category.replace(/_/g, ' '));
}

/** The rule's condition as the config states it, for the tooltip and for operators. */
export function conditionExpression(spec: RuleSpec): string {
  if (spec.metric)
    return `${spec.metric} ${spec.operator ?? 'gt'} ${spec.threshold ?? ''}${spec.window ? ` over ${spec.window}` : ''}`;
  return (spec.events ?? []).join(', ');
}

/** The rule's condition in words: "Latency is more than 3× its baseline for 5 min". */
export function describeCondition(spec: RuleSpec): string {
  if (spec.metric) {
    const m = METRICS[spec.metric];
    const op = OPERATORS[spec.operator ?? 'gt'];
    const value = spec.threshold !== undefined ? m.value(spec.threshold) : '—';
    const over = durationWords(spec.window);
    return `${m.label} ${op} ${value}${m.baseline ? ' its baseline' : ''}${over ? ` for ${over}` : ''}`;
  }
  const events = (spec.events ?? []).map((e) => e.replace(/[._]/g, ' '));
  if (events.length === 0) return 'No condition';
  return `When ${events.join(' or ')} happens`;
}

/** "Warning and above" for a channel's minimum severity. */
export function severityFloor(min: AlertChannelResource['spec']['min_severity']): string {
  if (min === 'critical') return 'Critical only';
  if (min === 'warning') return 'Warning and above';
  return 'All alerts';
}

export const CHANNEL_TYPES: Record<AlertChannelResource['spec']['type'], string> = {
  slack: 'Slack',
  pagerduty: 'PagerDuty',
  email: 'Email',
  webhook: 'Webhook',
};

export const STATUS_WORD: Record<Alert['status'], string> = {
  firing: 'Firing',
  acknowledged: 'Acknowledged',
  resolved: 'Resolved',
};

export function severityWord(severity: string): string {
  return capitalize(severity);
}

/** One activity entry in words: "Fired", "Notified oncall-slack", "Note: checking upstream". */
export function activityWords(
  kind: string,
  detail: Record<string, unknown> | undefined,
  time?: string,
): string {
  const channel = typeof detail?.channel === 'string' ? detail.channel : null;
  switch (kind) {
    case 'fired':
      return 'Fired';
    case 'refired':
      return 'Fired again';
    case 'notified':
      return channel ? `Notified ${channel}` : 'Notified';
    case 'notification_failed':
      return channel ? `Couldn't notify ${channel}` : "Couldn't send a notification";
    case 'acknowledged':
      return 'Acknowledged';
    case 'note':
      return 'Added a note';
    case 'escalated':
      return 'Escalated';
    case 'snoozed':
      if (detail && 'until' in detail && detail.until === null) return 'Snooze ended';
      if (typeof detail?.duration === 'string') return `Snoozed for ${durationWords(detail.duration) ?? ''}`;
      if (typeof detail?.until === 'string' && time) {
        const span = (new Date(detail.until).getTime() - new Date(time).getTime()) / 1000;
        if (span > 0) return `Snoozed for ${spanWords(span)}`;
      }
      return 'Snoozed';
    case 'resolved':
      return 'Resolved';
    default:
      return capitalize(kind.replace(/_/g, ' '));
  }
}

/** A span of time in words: "45 s", "3 min", "1 h 5 min", "2 d 4 h". */
export function spanWords(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d} d ${h % 24} h` : `${d} d`;
}

/** "3 min ago", "just now". */
export function agoWords(iso: string, now: number): string {
  const s = (now - new Date(iso).getTime()) / 1000;
  return s < 30 ? 'just now' : `${spanWords(s)} ago`;
}

/** "Wed 05:05", for the axis of a week. */
export function weekdayClock(value: string | number | Date, timeZone: string): string {
  return new Intl.DateTimeFormat('en-GB', {
    timeZone,
    weekday: 'short',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(new Date(value));
}

/** "Oct 8" or, with the clock, "Oct 8, 05:05". */
export function dayMonth(value: string | number | Date, timeZone: string, clock = false): string {
  return new Intl.DateTimeFormat('en-US', {
    timeZone,
    month: 'short',
    day: 'numeric',
    ...(clock ? { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' as const } : {}),
  }).format(new Date(value));
}
