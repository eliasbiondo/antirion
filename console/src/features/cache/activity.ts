import type { AuditEntry } from '@/lib/api/types';

import { describeCache } from './policy';
import type { PolicyCache } from './api';

export type ActivityKind = 'clear' | 'rule' | 'config' | 'report' | 'remove';

export interface Activity {
  id: string;
  time: string;
  kind: ActivityKind;
  /** One sentence: "Cleared the cache of model gpt-4o". */
  text: string;
  /** Who: a person or token, or "Automatic" for a rule the gateway fired. */
  actor: string;
  failed: boolean;
}

function record(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/**
 * The facts an action records: the `after` of its one change (the gateway states an action's facts
 * as an update of its resource), or the object older gateways stored.
 */
function facts(changes: unknown): Record<string, unknown> | null {
  if (Array.isArray(changes))
    return changes.length === 1 ? record((changes[0] as { after?: unknown }).after) : null;
  return record(changes);
}

/** The `policies.cache` of a Model snapshot in an audit change. */
function cachePolicy(snapshot: unknown): PolicyCache | undefined {
  const policies = record(record(snapshot)?.policies);
  return record(policies?.cache) ?? undefined;
}

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
}

/** What changed in a cache policy, in words ("Keep 15 min, Similar prompts match"). */
function policyChange(before: PolicyCache | undefined, after: PolicyCache | undefined): string {
  if (!after) return 'removed its cache settings';
  const b = before ?? {};
  const changed = Object.fromEntries(
    Object.entries(after).filter(([k, v]) => !same(v, (b as Record<string, unknown>)[k])),
  ) as PolicyCache;
  const words = describeCache(changed);
  return words.length > 0 ? words.join(', ').toLowerCase() : 'changed its cache settings';
}

function last(resource: string | null): string {
  return resource?.split('/').pop() ?? '';
}

function actorOf(e: AuditEntry): string {
  if (e.actor.type === 'system') return 'Automatic';
  if (e.actor.id === 'bootstrap') return 'Bootstrap token';
  return e.actor.id;
}

const RULE_VERBS: Record<string, string> = {
  create: 'Created',
  replace: 'Edited',
  update: 'Edited',
  patch: 'Edited',
  delete: 'Deleted',
};

/**
 * Turns audit entries into the cache activity feed; entries that are not about the cache (a Model
 * edit that leaves its cache policy alone, a read) are dropped.
 */
export function toActivity(entries: AuditEntry[]): Activity[] {
  const out: Activity[] = [];
  for (const e of entries) {
    if (e.kind === 'read') continue;
    const base = { id: e.id, time: e.time, actor: actorOf(e), failed: e.outcome !== 'success' };
    const a = e.action;
    const changes = e.changes as unknown;
    if (a === 'cache.invalidate') {
      const parts = (e.resource ?? '').split('/');
      const text =
        parts[1] === 'gateway'
          ? 'Cleared the whole cache'
          : parts[1] === 'older_than'
            ? 'Cleared responses older than a cutoff'
            : `Cleared the cache of ${(parts[1] ?? '').replace('_', ' ')} ${parts.slice(2).join('/')}`;
      out.push({ ...base, kind: 'clear', text });
    } else if (a === 'cache.delete') {
      out.push({ ...base, kind: 'remove', text: 'Removed a cached response' });
    } else if (a === 'cache.report_false_positive') {
      out.push({ ...base, kind: 'report', text: 'Reported a wrong similar match; the response was removed' });
    } else if (a === 'cache.rule_fired') {
      const c = facts(changes);
      const dropped = typeof c?.dropped === 'number' ? c.dropped : null;
      const trigger = typeof c?.trigger === 'string' ? c.trigger.replace('_', ' ') : null;
      out.push({
        ...base,
        kind: 'rule',
        text: `Rule ${last(e.resource)} fired${trigger && trigger !== 'manual' ? ` (${trigger})` : ''}${
          dropped !== null ? `, removing ${dropped === 1 ? '1 response' : `${dropped} responses`}` : ''
        }`,
      });
    } else if (a === 'cache.run_rule') {
      out.push({ ...base, kind: 'rule', text: `Ran rule ${last(e.resource)} by hand` });
    } else if (a.startsWith('cache.invalidation_rules.')) {
      const verb = RULE_VERBS[a.split('.').pop() ?? ''] ?? 'Changed';
      const list = Array.isArray(changes) ? (changes as { before?: unknown; after?: unknown }[]) : [];
      const before = record(list[0]?.before);
      const after = record(list[0]?.after);
      let text = `${verb} clearing rule ${last(e.resource)}`;
      if (verb === 'Edited' && before && after && before.enabled !== after.enabled)
        text = `Turned ${after.enabled === false ? 'off' : 'on'} clearing rule ${last(e.resource)}`;
      out.push({ ...base, kind: 'rule', text });
    } else if (e.resource === 'settings/cache') {
      out.push({ ...base, kind: 'config', text: 'Changed the cache storage settings' });
    } else if (e.resource === 'settings/policies' || e.resource?.startsWith('models/')) {
      const list = Array.isArray(changes)
        ? (changes as { path?: string; before?: unknown; after?: unknown }[])
        : [];
      for (const c of list) {
        const gateway = e.resource === 'settings/policies';
        const before = gateway ? cachePolicy({ policies: record(c.before) }) : cachePolicy(c.before);
        const after = gateway ? cachePolicy({ policies: record(c.after) }) : cachePolicy(c.after);
        if (same(before, after)) continue;
        const subject = gateway ? 'the gateway default' : `model ${last(e.resource)}`;
        const text =
          !before && after?.enabled === true
            ? `Turned on caching for ${subject}`
            : before?.enabled === true && after?.enabled === false
              ? `Turned off caching for ${subject}`
              : `Changed caching of ${subject}: ${policyChange(before, after)}`;
        out.push({ ...base, kind: 'config', text });
        break;
      }
    }
  }
  return out;
}
