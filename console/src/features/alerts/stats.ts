import type { Alert } from '@/lib/api/types';

export const DAY_MS = 86_400_000;
const HOUR_MS = 3_600_000;

const ms = (iso: string | null | undefined) => (iso ? new Date(iso).getTime() : NaN);

/** Seconds from firing to resolution, for a resolved instance. */
export function resolveSeconds(a: Alert): number | null {
  const fired = ms(a.fired_at);
  const resolved = ms(a.resolved_at);
  return Number.isFinite(fired) && Number.isFinite(resolved) ? Math.max(0, (resolved - fired) / 1000) : null;
}

/** Mean time to resolve over the instances resolved in `[from, to)`, or null without any. */
export function meanTimeToResolve(list: Alert[], from: number, to: number): number | null {
  const spans = list
    .filter((a) => {
      const r = ms(a.resolved_at);
      return r >= from && r < to;
    })
    .map(resolveSeconds)
    .filter((s): s is number => s !== null);
  return spans.length ? spans.reduce((x, y) => x + y, 0) / spans.length : null;
}

/** Instances that fired in `[from, to)`. */
export function firedBetween(list: Alert[], from: number, to: number): Alert[] {
  return list.filter((a) => {
    const f = ms(a.fired_at);
    return f >= from && f < to;
  });
}

/**
 * Noise: of the instances that fired in the window, the share that resolved within a minute of
 * firing. Null without any instance.
 */
export function noiseRatio(list: Alert[], from: number, to: number): number | null {
  const fired = firedBetween(list, from, to);
  if (fired.length === 0) return null;
  const quick = fired.filter((a) => {
    const s = resolveSeconds(a);
    return s !== null && s <= 60;
  }).length;
  return quick / fired.length;
}

/** Notifications sent since `from`: the `notified` entries of every instance, by channel. */
export function notificationsSince(
  list: Alert[],
  from: number,
): { total: number; failed: number; byChannel: { channel: string; n: number }[] } {
  const counts = new Map<string, number>();
  let total = 0;
  let failed = 0;
  for (const a of list) {
    for (const t of a.timeline ?? []) {
      if (ms(t.time) < from) continue;
      if (t.kind === 'notification_failed') failed += 1;
      if (t.kind !== 'notified') continue;
      total += 1;
      const channel = typeof t.detail?.channel === 'string' ? t.detail.channel : 'other';
      counts.set(channel, (counts.get(channel) ?? 0) + 1);
    }
  }
  const byChannel = [...counts.entries()]
    .map(([channel, n]) => ({ channel, n }))
    .sort((a, b) => b.n - a.n || a.channel.localeCompare(b.channel));
  return { total, failed, byChannel };
}

export const SEVERITIES = ['critical', 'warning', 'info'] as const;
export type Severity = (typeof SEVERITIES)[number];

export interface FireBucket {
  start: number;
  counts: Record<Severity, number>;
}

/** Fires stacked by severity: 24 hourly buckets for `24h`, 30 daily buckets for `30d`, ending now. */
export function fireBuckets(list: Alert[], span: '24h' | '30d', now: number): FireBucket[] {
  const size = span === '24h' ? HOUR_MS : DAY_MS;
  const n = span === '24h' ? 24 : 30;
  const end = span === '24h' ? Math.ceil(now / HOUR_MS) * HOUR_MS : Math.ceil(now / DAY_MS) * DAY_MS;
  const start = end - n * size;
  const buckets: FireBucket[] = Array.from({ length: n }, (_, i) => ({
    start: start + i * size,
    counts: { critical: 0, warning: 0, info: 0 },
  }));
  for (const a of list) {
    const i = Math.floor((ms(a.fired_at) - start) / size);
    const b = buckets[i];
    if (b && (SEVERITIES as readonly string[]).includes(a.severity)) b.counts[a.severity] += 1;
  }
  return buckets;
}

export interface RuleStats {
  fires: number;
  notifications: number;
  mttr: number | null;
  lastFired: string | null;
}

/** A rule's last 30 days: fires, notifications sent and mean time to resolve. */
export function ruleStats(list: Alert[], rule: string, now: number): RuleStats {
  const from = now - 30 * DAY_MS;
  const mine = list.filter((a) => a.rule === rule);
  const fired = firedBetween(mine, from, now + 1);
  const notifications = notificationsSince(mine, from).total;
  const last = fired.reduce<string | null>(
    (best, a) => (best === null || ms(a.fired_at) > ms(best) ? a.fired_at : best),
    null,
  );
  return {
    fires: fired.length,
    notifications,
    mttr: meanTimeToResolve(mine, from, now + 1),
    lastFired: last,
  };
}

/**
 * The error budget left at the end of each bucket of an availability objective's window, from the
 * availability report: the budget is the failures the objective allows over the whole window; each
 * point subtracts the failures seen so far.
 */
export function budgetBurn(
  rows: { requests: number; availability: number | null }[],
  objective: number,
): number[] {
  const total = rows.reduce((s, r) => s + r.requests, 0);
  const allowed = (1 - objective) * total;
  if (allowed <= 0) return [];
  let failed = 0;
  return rows.map((r) => {
    failed += r.availability === null ? 0 : (1 - r.availability) * r.requests;
    return 1 - failed / allowed;
  });
}

export interface BuiltInRule {
  name: string;
  category: string;
  severity: Severity;
  /** Instances that fired in the last 30 days. */
  fires: number;
  lastFired: string;
}

/**
 * The gateway's built-in rules (key anomaly, budget forecast…), known only from the alerts they
 * raised: every rule name on an instance that no configured rule carries, most recent first.
 */
export function builtInRules(instances: Alert[], configured: string[], now: number): BuiltInRule[] {
  const own = new Set(configured);
  const seen = new Set<string>();
  const byName = new Map<string, BuiltInRule>();
  for (const a of instances) {
    if (own.has(a.rule) || seen.has(a.id)) continue;
    seen.add(a.id);
    const fired = ms(a.fired_at);
    const prev = byName.get(a.rule);
    const recent = fired >= now - 30 * DAY_MS ? 1 : 0;
    if (!prev) {
      byName.set(a.rule, {
        name: a.rule,
        category: a.category,
        severity: a.severity,
        fires: recent,
        lastFired: a.fired_at,
      });
      continue;
    }
    prev.fires += recent;
    if (fired > ms(prev.lastFired)) {
      prev.lastFired = a.fired_at;
      prev.severity = a.severity;
    }
  }
  return [...byName.values()].sort(
    (a, b) => ms(b.lastFired) - ms(a.lastFired) || a.name.localeCompare(b.name),
  );
}
