import type { AuditEntry } from '@/lib/api/types';

import { same } from './form-errors';
import { auditActorBy } from '@/lib/audit-words';

function dig(value: unknown, path: string[]): unknown {
  let v = value;
  for (const key of path) {
    if (typeof v !== 'object' || v === null) return undefined;
    v = (v as Record<string, unknown>)[key];
  }
  return v;
}

/**
 * Whether an audit entry changed the configuration at `target` (a dotted path such as
 * `observability.request_logs.sinks.http.warehouse`): one of its changes is at the target or
 * inside it, or is a broader change whose before and after differ there.
 */
export function touches(entry: AuditEntry, target: string): boolean {
  // Defensive: some audit entries carry their changes in another shape; only a list of changes counts.
  const changes: unknown = entry.changes;
  if (!Array.isArray(changes)) return false;
  for (const c of changes as { path?: unknown; before?: unknown; after?: unknown }[]) {
    if (typeof c.path !== 'string') continue;
    const path = c.path;
    if (path === target || path.startsWith(`${target}.`)) return true;
    if (target.startsWith(`${path}.`)) {
      const rest = target.slice(path.length + 1).split('.');
      if (!same(dig(c.before, rest), dig(c.after, rest))) return true;
    }
  }
  return false;
}

/** The newest successful write that changed the configuration at any of `targets`. */
export function lastChange(entries: AuditEntry[] | null, targets: string[]): AuditEntry | null {
  if (!entries) return null;
  return entries.find((e) => e.outcome === 'success' && targets.some((t) => touches(e, t))) ?? null;
}

/** Who made an audit entry, in words: a person's name, or "Bootstrap token". */
export function actorWords(entry: AuditEntry): string {
  return auditActorBy(entry.actor);
}
