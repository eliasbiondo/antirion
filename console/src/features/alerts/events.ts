/**
 * The gateway's built-in event types, grouped by their source, in words. The contract has no
 * operation that lists them, so the console carries the catalogue.
 */
export interface EventType {
  type: string;
  label: string;
  /** What the event carries or when it is sent. */
  detail: string;
  /** An event rule may subscribe to it (the `alert.*` events would raise alerts about alerts). */
  rule: boolean;
}

export interface EventSource {
  prefix: string;
  label: string;
  events: EventType[];
}

const e = (type: string, label: string, detail: string, rule = true): EventType => ({
  type,
  label,
  detail,
  rule,
});

export const EVENT_SOURCES: EventSource[] = [
  {
    prefix: 'deployment',
    label: 'Deployments',
    events: [
      e(
        'deployment.state_changed',
        'Deployment state changed',
        'A deployment went down, came back or was taken out of rotation by its circuit breaker.',
      ),
    ],
  },
  {
    prefix: 'budget',
    label: 'Budgets',
    events: [
      e(
        'budget.threshold_crossed',
        'Budget threshold crossed',
        "Spend passed one of a budget's alert percentages.",
      ),
      e(
        'budget.forecast_exceeded',
        'Budget forecast over',
        'A budget is projected to end its period over the amount.',
      ),
      e(
        'budget.exhausted',
        'Budget used up',
        'A budget reached its amount; a hard budget now refuses requests.',
      ),
      e('budget.overage_granted', 'Overage approved', 'Someone approved going over a budget.'),
    ],
  },
  {
    prefix: 'config',
    label: 'Configuration',
    events: [
      e('config.applied', 'Configuration applied', 'A new configuration generation took effect.'),
      e(
        'config.rejected',
        'Configuration rejected',
        'A configuration change failed validation and was not applied.',
      ),
    ],
  },
  {
    prefix: 'log_sink',
    label: 'Log sinks',
    events: [
      e(
        'log_sink.dropping',
        'Log sink dropping records',
        'A log sink is dropping records; sent at most once a minute.',
      ),
    ],
  },
  {
    prefix: 'routing',
    label: 'Routing',
    events: [
      e('routing.canary_rolled_back', 'Canary rolled back', 'A canary route was rolled back by its guard.'),
      e('routing.canary_held', 'Canary held', 'A canary route stopped advancing.'),
    ],
  },
  {
    prefix: 'provider',
    label: 'Providers',
    events: [
      e(
        'provider.credential_rotation_due',
        'Provider credential due for rotation',
        'A provider credential is older than its rotation period.',
      ),
    ],
  },
  {
    prefix: 'key',
    label: 'API keys',
    events: [
      e('key.expiring', 'Key expiring', 'An API key expires soon.'),
      e('key.rotation_due', 'Key due for rotation', 'An API key is older than its rotation period.'),
      e('key.anomaly', 'Key anomaly', 'A key was used in an unusual way: a spike, a new network or country.'),
    ],
  },
  {
    prefix: 'guardrail',
    label: 'Guardrails',
    events: [e('guardrail.event', 'Guardrail event', 'A guardrail blocked, redacted or flagged a request.')],
  },
  {
    prefix: 'change_request',
    label: 'Change requests',
    events: [
      e('change_request.opened', 'Change request opened', 'Someone asked for a change that needs approval.'),
      e('change_request.approved', 'Change request approved', 'A second person approved a change.'),
      e('change_request.applied', 'Change request applied', 'An approved change took effect.'),
    ],
  },
  {
    prefix: 'audit',
    label: 'Security',
    events: [
      e(
        'audit.privileged',
        'Privileged change',
        'Role bindings, security, sign-in or governance settings changed, or a secret was rotated.',
      ),
      e(
        'auth.login_failed',
        'Failed sign-in',
        'A known user failed to sign in; at most once a minute per user.',
      ),
    ],
  },
  {
    prefix: 'alert',
    label: 'Alerts',
    events: [
      e('alert.fired', 'Alert fired', 'An alert started firing.', false),
      e('alert.acknowledged', 'Alert acknowledged', 'Someone acknowledged an alert.', false),
      e('alert.resolved', 'Alert resolved', 'An alert resolved, by itself or by hand.', false),
    ],
  },
];

export const EVENT_TYPES: EventType[] = EVENT_SOURCES.flatMap((s) => s.events);

/** Whether a subscription entry (a type, `<prefix>.*` or `*`) covers an event type, as the gateway matches. */
export function covers(pattern: string, type: string): boolean {
  if (pattern === '*') return true;
  if (pattern.endsWith('.*')) return type.startsWith(pattern.slice(0, -1));
  return pattern === type;
}

/** A subscription entry in words: "Budget used up", "Every budget event", "Every event". */
export function eventWords(pattern: string): string {
  if (pattern === '*') return 'Every event';
  if (pattern.endsWith('.*')) {
    const prefix = pattern.slice(0, -2);
    const source = EVENT_SOURCES.find((s) => s.events.some((ev) => ev.type.startsWith(`${prefix}.`)));
    return `Every ${(source?.label ?? prefix).toLowerCase().replace(/s$/, '')} event`;
  }
  return EVENT_TYPES.find((ev) => ev.type === pattern)?.label ?? pattern;
}

/** The category an event rule on these events most likely belongs to. */
export function categoryForEvents(events: string[]): string {
  const first = events[0] ?? '';
  if (first.startsWith('budget')) return 'budget';
  if (first.startsWith('deployment') || first.startsWith('routing')) return 'availability';
  if (first.startsWith('guardrail')) return 'safety';
  if (first.startsWith('key') || first.startsWith('audit') || first.startsWith('auth')) return 'security';
  return 'anomaly';
}
