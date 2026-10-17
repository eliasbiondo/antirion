import type { RuleSpec, WebhookSpec } from '@/features/alerts/api';
import { covers } from '@/features/alerts/events';
import type { AlertChannelResource, AlertRuleResource } from '@/lib/api/types';

/** A row of the notification matrix: alerts of a severity, or a category of events. */
export type MatrixRow =
  | { id: string; label: string; kind: 'severity'; severity: 'critical' | 'warning' }
  | {
      id: string;
      label: string;
      kind: 'events';
      events: string[];
      category: NonNullable<RuleSpec['category']>;
    }
  | { id: string; label: string; kind: 'personal' };

export const MATRIX_ROWS: MatrixRow[] = [
  { id: 'critical', label: 'Critical alerts', kind: 'severity', severity: 'critical' },
  { id: 'warning', label: 'Warning alerts', kind: 'severity', severity: 'warning' },
  {
    id: 'budget-exceeded',
    label: 'Budget exceeded',
    kind: 'events',
    events: ['budget.exhausted'],
    category: 'budget',
  },
  {
    id: 'budget-forecast',
    label: 'Budget forecast',
    kind: 'events',
    events: ['budget.forecast_exceeded'],
    category: 'budget',
  },
  { id: 'safety', label: 'Safety events', kind: 'events', events: ['guardrail.event'], category: 'safety' },
  {
    id: 'key-rotations',
    label: 'Key rotations',
    kind: 'events',
    events: ['key.rotation_due', 'key.expiring'],
    category: 'security',
  },
  {
    id: 'key-anomalies',
    label: 'Key anomalies',
    kind: 'events',
    events: ['key.anomaly'],
    category: 'anomaly',
  },
  {
    id: 'privileged',
    label: 'Privileged audit events',
    kind: 'events',
    events: ['audit.privileged'],
    category: 'security',
  },
  { id: 'digest', label: 'Weekly digest', kind: 'personal' },
];

/** The rule the matrix creates for an event row when no rule sends those events yet. */
export function managedRuleName(row: MatrixRow): string {
  return `notify-${row.id}`;
}

const SEVERITY_RANK = { info: 0, warning: 1, critical: 2 } as const;

export type CellState =
  | { kind: 'on' | 'off' | 'mixed'; detail: string; rules: AlertRuleResource[] }
  | { kind: 'blocked'; detail: string };

/** Event rules whose subscription covers any of the row's events. */
function eventRules(rules: AlertRuleResource[], events: string[]): AlertRuleResource[] {
  return rules.filter(
    (r) => r.spec.kind === 'event' && (r.spec.events ?? []).some((p) => events.some((e) => covers(p, e))),
  );
}

/** Whether a rule subscribes to nothing but the row's events (so taking a channel off it affects only the row). */
export function onlyRow(rule: AlertRuleResource, events: string[]): boolean {
  return (rule.spec.events ?? []).every((p) => events.includes(p));
}

/** Where a row's events or alerts go to a channel. */
export function channelCell(
  row: MatrixRow,
  channel: AlertChannelResource,
  rules: AlertRuleResource[],
): CellState {
  if (row.kind === 'personal') return { kind: 'blocked', detail: '' };
  if (row.kind === 'severity') {
    const floor = channel.spec.min_severity ?? 'info';
    if (SEVERITY_RANK[floor] > SEVERITY_RANK[row.severity])
      return {
        kind: 'blocked',
        detail: `${channel.name} only takes ${floor} alerts; change that in the channel.`,
      };
    const of = rules.filter(
      (r) => (r.spec.kind ?? 'metric') === 'metric' && (r.spec.severity ?? 'warning') === row.severity,
    );
    if (of.length === 0) return { kind: 'blocked', detail: `No ${row.severity} alert rules yet.` };
    const using = of.filter((r) => (r.spec.channels ?? []).includes(channel.name));
    const kind = using.length === 0 ? 'off' : using.length === of.length ? 'on' : 'mixed';
    return {
      kind,
      detail:
        kind === 'mixed'
          ? `${using.length} of ${of.length} ${row.severity} rules send to ${channel.name}.`
          : kind === 'on'
            ? `Every ${row.severity} rule sends to ${channel.name}.`
            : `No ${row.severity} rule sends to ${channel.name}.`,
      rules: of,
    };
  }
  const of = eventRules(rules, row.events);
  const using = of.filter((r) => (r.spec.channels ?? []).includes(channel.name));
  return {
    kind: using.length ? 'on' : 'off',
    detail: using.length ? `Sent by ${using.map((r) => r.name).join(', ')}.` : `Not sent to ${channel.name}.`,
    rules: of,
  };
}

/** The rule writes that turn a channel cell on or off: replaced rules and, at most, one new rule. */
export function channelToggle(
  row: MatrixRow,
  channel: string,
  on: boolean,
  rules: AlertRuleResource[],
): {
  replace: { rule: AlertRuleResource; spec: RuleSpec }[];
  create: { name: string; spec: RuleSpec } | null;
  still: string[];
} {
  const replace: { rule: AlertRuleResource; spec: RuleSpec }[] = [];
  const withChannel = (r: AlertRuleResource): RuleSpec => ({
    ...r.spec,
    channels: [...new Set([...(r.spec.channels ?? []), channel])],
  });
  const without = (r: AlertRuleResource): RuleSpec => ({
    ...r.spec,
    channels: (r.spec.channels ?? []).filter((c) => c !== channel),
  });
  if (row.kind === 'severity') {
    const of = rules.filter(
      (r) => (r.spec.kind ?? 'metric') === 'metric' && (r.spec.severity ?? 'warning') === row.severity,
    );
    for (const r of of) {
      const has = (r.spec.channels ?? []).includes(channel);
      if (on && !has) replace.push({ rule: r, spec: withChannel(r) });
      if (!on && has) replace.push({ rule: r, spec: without(r) });
    }
    return { replace, create: null, still: [] };
  }
  if (row.kind !== 'events') return { replace, create: null, still: [] };
  const of = eventRules(rules, row.events);
  if (on) {
    const own = rules.find((r) => r.name === managedRuleName(row)) ?? of.find((r) => onlyRow(r, row.events));
    if (own) {
      replace.push({ rule: own, spec: { ...withChannel(own), enabled: true } });
      return { replace, create: null, still: [] };
    }
    return {
      replace,
      create: {
        name: managedRuleName(row),
        spec: {
          kind: 'event',
          events: row.events,
          category: row.category,
          severity: row.id === 'privileged' ? 'critical' : 'warning',
          channels: [channel],
          description: `${row.label}, as set in Settings → Notifications.`,
        },
      },
      still: [],
    };
  }
  const still: string[] = [];
  for (const r of of) {
    if (!(r.spec.channels ?? []).includes(channel)) continue;
    if (onlyRow(r, row.events)) replace.push({ rule: r, spec: without(r) });
    else still.push(r.name);
  }
  return { replace, create: null, still };
}

/** Whether an event webhook receives the row's events: all, some or none of them. */
export function webhookCell(row: MatrixRow, hook: WebhookSpec): CellState {
  if (row.kind !== 'events')
    return {
      kind: 'blocked',
      detail:
        'Alerts reach a webhook through a channel of type webhook, or as alert events it subscribes to.',
    };
  const got = row.events.filter((e) => hook.events.some((p) => covers(p, e)));
  const kind = got.length === 0 ? 'off' : got.length === row.events.length ? 'on' : 'mixed';
  return { kind, detail: kind === 'off' ? 'Not subscribed.' : 'Subscribed.', rules: [] };
}

/**
 * The subscription after turning a webhook cell on or off: on adds the row's events; off removes
 * them, and reports the broader patterns (`budget.*`, `*`) that keep sending them.
 */
export function webhookToggle(
  row: MatrixRow,
  hook: WebhookSpec,
  on: boolean,
): { events: string[]; still: string[] } {
  if (row.kind !== 'events') return { events: hook.events, still: [] };
  if (on) return { events: [...new Set([...hook.events, ...row.events])], still: [] };
  const events = hook.events.filter((p) => !row.events.includes(p));
  const still = events.filter((p) => row.events.some((e) => covers(p, e)));
  return { events, still };
}
