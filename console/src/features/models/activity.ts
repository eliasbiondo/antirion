import type { Alert, AuditEntry } from '@/lib/api/types';

export type ActivityKind = 'change' | 'access' | 'alert';

/** One line of a Model's or the catalog's activity, newest first. */
export interface ActivityItem {
  id: string;
  time: string;
  kind: ActivityKind;
  tone: 'success' | 'warn' | 'error' | 'info' | 'neutral';
  /** What happened, in words ("gpt-4o deprecated"). */
  text: string;
  /** Who did it, when someone did. */
  actor: string | null;
  /** The Model it is about, when there is one. */
  model: string | null;
}

const FIELD_WORDS: Record<string, string> = {
  target: 'fallback chain',
  route: 'route',
  aliases: 'aliases',
  access: 'access list',
  lifecycle: 'status',
  tier: 'tier',
  labels: 'labels',
  display_name: 'display name',
  description: 'description',
  enabled: 'on or off',
  catalog: 'catalog details',
  policies: 'policies',
  annotations: 'notes',
};

const LIFECYCLE_WORDS: Record<string, string> = {
  ga: 'made available',
  preview: 'put in preview',
  restricted: 'restricted',
  deprecated: 'deprecated',
};

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** The top-level fields that differ between two specs. */
export function changedFields(before: unknown, after: unknown): string[] {
  const a = isRecord(before) ? before : {};
  const b = isRecord(after) ? after : {};
  return [...new Set([...Object.keys(a), ...Object.keys(b)])]
    .filter((k) => JSON.stringify(a[k]) !== JSON.stringify(b[k]))
    .sort();
}

function firstChange(entry: AuditEntry): { before: unknown; after: unknown } | null {
  const changes = entry.changes as unknown;
  if (!Array.isArray(changes) || changes.length === 0) return null;
  const c = changes[0] as { before?: unknown; after?: unknown };
  return { before: c.before, after: c.after };
}

/** What an action recorded: its one change's `after`, or the object older gateways stored. */
function actionFacts(entry: AuditEntry): Record<string, unknown> | null {
  const changes = entry.changes as unknown;
  if (Array.isArray(changes)) {
    const after = changes.length === 1 ? (changes[0] as { after?: unknown }).after : null;
    return isRecord(after) ? after : null;
  }
  return isRecord(changes) ? changes : null;
}

const VERB: Record<string, string> = {
  create: 'added',
  update: 'changed',
  replace: 'changed',
  patch: 'changed',
  delete: 'deleted',
};

/** "Deployment x: price, capabilities changed", or "Deployment x added". */
function withFields(subject: string, verb: string, entry: AuditEntry): string {
  const change = firstChange(entry);
  const fields = verb === 'changed' && change ? changedFields(change.before, change.after) : [];
  return fields.length > 0
    ? `${subject}: ${fields.map((f) => f.replace(/_/g, ' ')).join(', ')} changed`
    : `${subject} ${verb}`;
}

/** An audit entry of the catalog in words, or null for one that is not about it. */
export function describeAudit(entry: AuditEntry): ActivityItem | null {
  const resource = entry.resource ?? '';
  const [collection, name, sub] = resource.split('/');
  const base = {
    id: entry.id,
    time: entry.time,
    actor: entry.actor.type === 'system' ? null : entry.actor.id,
  };
  const failed = entry.outcome !== 'success';
  if (collection === 'models' && name && sub === 'access_requests') {
    const facts = actionFacts(entry);
    const scope = typeof facts?.scope === 'string' ? facts.scope : null;
    const verb = entry.action.endsWith(':approve')
      ? 'approved'
      : entry.action.endsWith(':deny')
        ? 'denied'
        : 'requested';
    return {
      ...base,
      kind: 'access',
      model: name,
      tone: verb === 'approved' ? 'success' : verb === 'denied' ? 'error' : 'info',
      text: `Access to ${name} ${verb}${scope ? ` for ${scope}` : ''}`,
    };
  }
  const verb = VERB[entry.action.split('.').pop() ?? ''] ?? 'changed';
  if (collection === 'models' && name) {
    const change = firstChange(entry);
    let text = `Model ${name} ${verb}`;
    if (verb === 'changed' && change) {
      const fields = changedFields(change.before, change.after);
      const after = isRecord(change.after) ? change.after : {};
      const lifecycle = isRecord(after.lifecycle) ? after.lifecycle.state : undefined;
      if (fields.length === 1 && fields[0] === 'lifecycle' && typeof lifecycle === 'string')
        text = `Model ${name} ${LIFECYCLE_WORDS[lifecycle] ?? 'changed'}`;
      else if (fields.length === 1 && fields[0] === 'enabled')
        text = `Model ${name} turned ${after.enabled === false ? 'off' : 'on'}`;
      else if (fields.length > 0)
        text = `Model ${name}: ${fields.map((f) => FIELD_WORDS[f] ?? f.replace(/_/g, ' ')).join(', ')} changed`;
    }
    return {
      ...base,
      kind: 'change',
      model: name,
      tone: failed ? 'error' : verb === 'deleted' ? 'warn' : 'neutral',
      text,
    };
  }
  if (collection === 'routes' && name)
    return {
      ...base,
      kind: 'change',
      model: null,
      tone: failed ? 'error' : 'neutral',
      text: withFields(`Shared route ${name}`, verb, entry),
    };
  if (collection === 'deployments' && name)
    return {
      ...base,
      kind: 'change',
      model: null,
      tone: failed ? 'error' : 'neutral',
      text: withFields(`Deployment ${name}`, verb, entry),
    };
  if (resource === 'settings/routing')
    return { ...base, kind: 'change', model: null, tone: 'neutral', text: 'Default models or tiers changed' };
  return null;
}

/** An alert instance as an activity line. */
export function describeAlert(alert: Alert): ActivityItem {
  return {
    id: `alert-${alert.id}`,
    time: alert.fired_at,
    kind: 'alert',
    tone: alert.severity === 'critical' ? 'error' : alert.severity === 'warning' ? 'warn' : 'info',
    text: alert.summary,
    actor: null,
    model: alert.scope?.model ?? null,
  };
}

/** Audit entries and alerts merged into one feed, newest first, without duplicates. */
export function activityFeed(audit: AuditEntry[], alerts: Alert[], limit = 50): ActivityItem[] {
  const seen = new Set<string>();
  return [
    ...audit.map(describeAudit).filter((i): i is ActivityItem => i !== null),
    ...alerts.map(describeAlert),
  ]
    .filter((i) => (seen.has(i.id) ? false : (seen.add(i.id), true)))
    .sort((a, b) => b.time.localeCompare(a.time))
    .slice(0, limit);
}
