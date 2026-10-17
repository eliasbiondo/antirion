import type { AlertChannelResource, AlertRuleResource } from '@/lib/api/types';
import type { FieldErrors } from '@/lib/validation';

import type { ChannelSpec, RuleSpec, WebhookSpec } from './api';
import { METRICS, SCOPE_DIMENSIONS, thresholdInput, thresholdValue, type ScopeDimension } from './describe';

type Metric = NonNullable<RuleSpec['metric']>;
type Category = RuleSpec['category'];
type Severity = NonNullable<RuleSpec['severity']>;

const NAME = /^[a-z0-9]([a-z0-9_-]{0,62}[a-z0-9])?$/;

export interface RuleForm {
  name: string;
  kind: 'metric' | 'event';
  metric: Metric;
  operator: NonNullable<RuleSpec['operator']>;
  threshold: string;
  window: string;
  holdFor: string;
  events: string[];
  scope: Record<ScopeDimension, string>;
  category: Category;
  severity: Severity;
  cooldown: string;
  resolveAfter: string;
  channels: string[];
  escalation: string[];
  ownerTeam: string;
  runbookUrl: string;
  description: string;
  silence: boolean;
  enabled: boolean;
}

const EMPTY_SCOPE = Object.fromEntries(SCOPE_DIMENSIONS.map((d) => [d, ''])) as Record<
  ScopeDimension,
  string
>;

export const EMPTY_RULE: RuleForm = {
  name: '',
  kind: 'metric',
  metric: 'error_rate',
  operator: 'gt',
  threshold: '',
  window: '5m',
  holdFor: '0s',
  events: [],
  scope: EMPTY_SCOPE,
  category: 'errors',
  severity: 'warning',
  cooldown: '15m',
  resolveAfter: '5m',
  channels: [],
  escalation: [],
  ownerTeam: '',
  runbookUrl: '',
  description: '',
  silence: false,
  enabled: true,
};

/** A rule (or a template's partial spec) as the editor's values, with the schema's defaults. */
export function ruleForm(name: string, spec: Partial<RuleSpec>): RuleForm {
  const kind = spec.kind ?? (spec.events && !spec.metric ? 'event' : 'metric');
  const metric = spec.metric ?? EMPTY_RULE.metric;
  return {
    name,
    kind,
    metric,
    operator: spec.operator ?? 'gt',
    threshold: thresholdInput(metric, spec.threshold),
    window: spec.window ?? '5m',
    holdFor: spec.for ?? '0s',
    events: spec.events ?? [],
    scope: { ...EMPTY_SCOPE, ...Object.fromEntries(Object.entries(spec.scope ?? {}).filter(([, v]) => v)) },
    category: spec.category ?? EMPTY_RULE.category,
    severity: spec.severity ?? 'warning',
    cooldown: spec.cooldown ?? '15m',
    resolveAfter: spec.resolve_after ?? '5m',
    channels: spec.channels ?? [],
    escalation: spec.escalation ?? [],
    ownerTeam: spec.owner_team ?? '',
    runbookUrl: spec.runbook_url ?? '',
    description: spec.description ?? '',
    silence: spec.silence_during_maintenance ?? false,
    enabled: spec.enabled ?? true,
  };
}

/** The editor's values as a rule spec; members left at nothing are omitted. */
export function ruleSpec(f: RuleForm): RuleSpec {
  const scope = Object.fromEntries(
    SCOPE_DIMENSIONS.filter((d) => f.scope[d].trim()).map((d) => [d, f.scope[d].trim()]),
  );
  const spec: RuleSpec = {
    enabled: f.enabled,
    kind: f.kind,
    category: f.category,
    severity: f.severity,
    cooldown: f.cooldown,
    resolve_after: f.resolveAfter,
    channels: f.channels,
    escalation: f.escalation,
    silence_during_maintenance: f.silence,
  };
  if (f.kind === 'metric') {
    spec.metric = f.metric;
    spec.operator = f.operator;
    spec.threshold = thresholdValue(f.metric, f.threshold) ?? 0;
    spec.window = f.window;
    spec.for = f.holdFor;
  } else {
    spec.events = f.events;
  }
  if (Object.keys(scope).length) spec.scope = scope;
  if (f.ownerTeam.trim()) spec.owner_team = f.ownerTeam.trim();
  if (f.runbookUrl.trim()) spec.runbook_url = f.runbookUrl.trim();
  if (f.description.trim()) spec.description = f.description.trim();
  return spec;
}

/** What the person must fix before the rule is sent. */
export function ruleErrors(f: RuleForm, existing: AlertRuleResource | null, taken: string[]): FieldErrors {
  const errors: FieldErrors = {};
  if (!existing) {
    if (!NAME.test(f.name)) errors.name = 'Use lowercase letters, numbers, - and _ (up to 64)';
    else if (taken.includes(f.name)) errors.name = 'A rule with this name exists';
  }
  if (f.kind === 'metric') {
    const value = thresholdValue(f.metric, f.threshold);
    if (value === null) errors.threshold = 'Enter a number';
    else if (METRICS[f.metric].share && (value < 0 || value > 1))
      errors.threshold = 'Enter a percentage from 0 to 100';
    else if (value < 0) errors.threshold = 'Enter a number from 0 up';
  }
  if (f.kind === 'event' && f.events.length === 0) errors.events = 'Choose at least one event';
  if (f.runbookUrl.trim() && !/^https?:\/\/\S+$/.test(f.runbookUrl.trim()))
    errors.runbookUrl = 'A link that starts with https://';
  return errors;
}

/** A name suggested from a metric or the first event, unused among `taken`. */
export function suggestName(base: string, taken: string[]): string {
  const clean =
    base
      .toLowerCase()
      .replace(/[^a-z0-9_-]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 56) || 'rule';
  if (!taken.includes(clean)) return clean;
  for (let i = 2; i < 100; i += 1) if (!taken.includes(`${clean}-${i}`)) return `${clean}-${i}`;
  return `${clean}-${Date.now() % 10_000}`;
}

/** Maps the gateway's field paths under the rule to the editor's fields. */
export const RULE_FIELD: Record<string, keyof RuleForm> = {
  threshold: 'threshold',
  metric: 'metric',
  events: 'events',
  window: 'window',
  for: 'holdFor',
  cooldown: 'cooldown',
  resolve_after: 'resolveAfter',
  channels: 'channels',
  escalation: 'escalation',
  owner_team: 'ownerTeam',
  runbook_url: 'runbookUrl',
  description: 'description',
};

export interface ChannelForm {
  name: string;
  type: ChannelSpec['type'];
  minSeverity: NonNullable<ChannelSpec['min_severity']>;
  enabled: boolean;
  slackUrl: string;
  routingKey: string;
  eventsUrl: string;
  emails: string[];
  webhook: string;
}

export const REDACTED = '[REDACTED]';

export function channelForm(channel: AlertChannelResource | null, webhook?: string): ChannelForm {
  const s = channel?.spec;
  return {
    name: channel?.name ?? '',
    type: s?.type ?? (webhook ? 'webhook' : 'slack'),
    minSeverity: s?.min_severity ?? 'info',
    enabled: s?.enabled ?? true,
    // A stored secret is never shown: an empty field keeps it.
    slackUrl: '',
    routingKey: '',
    eventsUrl: s?.pagerduty?.events_url ?? '',
    emails: s?.email?.to ?? [],
    webhook: s?.webhook?.webhook ?? webhook ?? '',
  };
}

export function channelSpec(f: ChannelForm, existing: AlertChannelResource | null): ChannelSpec {
  const keep = (v: string, was: string | undefined) => v.trim() || (was ? REDACTED : '');
  const spec: ChannelSpec = { type: f.type, min_severity: f.minSeverity, enabled: f.enabled };
  if (f.type === 'slack') spec.slack = { webhook_url: keep(f.slackUrl, existing?.spec.slack?.webhook_url) };
  if (f.type === 'pagerduty')
    spec.pagerduty = {
      routing_key: keep(f.routingKey, existing?.spec.pagerduty?.routing_key),
      ...(f.eventsUrl.trim() ? { events_url: f.eventsUrl.trim() } : {}),
    };
  if (f.type === 'email') spec.email = { to: f.emails };
  if (f.type === 'webhook') spec.webhook = { webhook: f.webhook };
  return spec;
}

export function channelErrors(
  f: ChannelForm,
  existing: AlertChannelResource | null,
  taken: string[],
): FieldErrors {
  const errors: FieldErrors = {};
  if (!existing) {
    if (!NAME.test(f.name)) errors.name = 'Use lowercase letters, numbers, - and _ (up to 64)';
    else if (taken.includes(f.name)) errors.name = 'A channel with this name exists';
  }
  const kept = existing?.spec.type === f.type ? existing.spec : undefined;
  if (f.type === 'slack' && !f.slackUrl.trim() && !kept?.slack?.webhook_url)
    errors.slackUrl = 'Paste the incoming webhook URL from Slack';
  else if (f.type === 'slack' && f.slackUrl.trim() && !/^https?:\/\/\S+$/.test(f.slackUrl.trim()))
    errors.slackUrl = 'Enter a link that starts with https:// or http://';
  if (f.type === 'pagerduty' && !f.routingKey.trim() && !kept?.pagerduty?.routing_key)
    errors.routingKey = 'Paste the integration key of the PagerDuty service';
  if (f.type === 'pagerduty' && f.eventsUrl.trim() && !/^https:\/\/\S+$/.test(f.eventsUrl.trim()))
    errors.eventsUrl = 'A link that starts with https://';
  if (f.type === 'email') {
    if (f.emails.length === 0) errors.emails = 'Add at least one address';
    else if (f.emails.some((e) => !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(e)))
      errors.emails = 'One of the addresses is not an email address';
  }
  if (f.type === 'webhook' && !f.webhook) errors.webhook = 'Choose an event webhook';
  return errors;
}

export const CHANNEL_FIELD: Record<string, keyof ChannelForm> = {
  type: 'type',
  'slack.webhook_url': 'slackUrl',
  'pagerduty.routing_key': 'routingKey',
  'pagerduty.events_url': 'eventsUrl',
  'email.to': 'emails',
  'webhook.webhook': 'webhook',
};

export interface WebhookForm {
  name: string;
  url: string;
  events: string[];
  enabled: boolean;
  timeout: string;
}

export function webhookForm(
  name: string | null,
  spec: WebhookSpec | undefined,
  events?: string[],
): WebhookForm {
  return {
    name: name ?? '',
    url: spec?.url ?? '',
    events: spec?.events ?? events ?? [],
    enabled: spec?.enabled ?? true,
    timeout: spec?.timeout ?? '5s',
  };
}

export function webhookErrors(f: WebhookForm, isNew: boolean, taken: string[]): FieldErrors {
  const errors: FieldErrors = {};
  if (isNew) {
    if (!NAME.test(f.name)) errors.name = 'Use lowercase letters, numbers, - and _ (up to 64)';
    else if (taken.includes(f.name)) errors.name = 'A webhook with this name exists';
  }
  if (!/^https?:\/\/\S+$/.test(f.url.trim())) errors.url = 'A link that starts with https://';
  if (f.events.length === 0) errors.events = 'Choose at least one event';
  return errors;
}

/** Field errors from a gateway validation answer, mapped by the path under `prefix`. */
export function mapServerErrors<F>(
  error: unknown,
  prefix: string,
  fields: Record<string, keyof F>,
): Partial<Record<keyof F, string>> {
  const details = (error as { details?: { path?: string; message?: string }[] } | null)?.details ?? [];
  const out: Partial<Record<keyof F, string>> = {};
  for (const d of details) {
    if (!d.path?.startsWith(prefix)) continue;
    const rest = d.path.slice(prefix.length).replace(/^\./, '');
    const key = Object.keys(fields)
      .sort((a, b) => b.length - a.length)
      .find((k) => rest === k || rest.startsWith(`${k}.`) || rest.startsWith(`${k}[`));
    const field = key ? fields[key] : undefined;
    if (field && d.message) out[field] = d.message.charAt(0).toUpperCase() + d.message.slice(1);
  }
  return out;
}
