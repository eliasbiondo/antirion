/** Figures and facts the Routing screen derives from the gateway's reads, as pure functions. */

import type { Alert, AuditEntry, LogRecord, UsageBucket } from '@/lib/api/types';

import { ruleOfPath } from '@/features/models/catalog';

import type { RouteStateEntry } from './api';
import { isRecord, type Json, type Rule, targetsOf } from './types';

export interface Editor {
  actor: string;
  time: string;
}

function rulesIn(route: Json): Record<string, Json>[] {
  const tree = isRecord(route) ? route.tree : undefined;
  if (!isRecord(tree) || !Array.isArray(tree.rules)) return [];
  return (tree.rules as Json[]).filter(isRecord);
}

/**
 * Who last changed each rule and when, by rule name: the newest route write whose rule differs
 * from the version before it (a reorder alone changes no rule).
 */
export function lastEditors(audit: AuditEntry[], route: string): Map<string, Editor> {
  const out = new Map<string, Editor>();
  for (const entry of audit) {
    if (entry.resource !== `routes/${route}` || entry.outcome !== 'success') continue;
    for (const change of entry.changes ?? []) {
      if (change.path !== `routes.${route}`) continue;
      const before = new Map(rulesIn(change.before).map((r) => [String(r.name), JSON.stringify(r)]));
      for (const rule of rulesIn(change.after)) {
        const name = String(rule.name);
        if (out.has(name)) continue;
        if (before.get(name) !== JSON.stringify(rule))
          out.set(name, { actor: entry.actor.id, time: entry.time });
      }
    }
  }
  return out;
}

export interface Impact {
  requests: number;
  blocks: number;
  fallbacks: number;
}

/** Requests, blocks and fallbacks per rule (index, or `default`) of a route, and the route's total. */
export function ruleImpact(
  rows: UsageBucket[],
  route: string,
  rootId?: string,
): { byRule: Map<number | 'default', Impact>; total: number } {
  const byRule = new Map<number | 'default', Impact>();
  let total = 0;
  for (const r of rows) {
    const path = r.group.route_node;
    if (!path) continue;
    const rule = ruleOfPath(path, route, rootId);
    if (rule === null) continue;
    total += r.requests;
    const now = byRule.get(rule) ?? { requests: 0, blocks: 0, fallbacks: 0 };
    byRule.set(rule, {
      requests: now.requests + r.requests,
      blocks: now.blocks + (r.guardrail_blocks ?? 0),
      fallbacks: now.fallbacks + (r.fallbacks ?? 0),
    });
  }
  return { byRule, total };
}

function percentile(sorted: number[], p: number): number | null {
  if (sorted.length === 0) return null;
  const i = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[Math.max(0, i)] ?? null;
}

/** Time the gateway spent choosing where a request goes: its resolve and route stages. */
export function decisionMs(record: LogRecord): number | null {
  const stages = isRecord(record.timing) ? record.timing.stage_ms : null;
  if (!isRecord(stages)) return null;
  const parts = ['resolve', 'route'].map((s) => stages[s]).filter((v): v is number => typeof v === 'number');
  return parts.length > 0 ? parts.reduce((a, b) => a + b, 0) : null;
}

/** Decision time p95 and p99 over records that report stage timing. */
export function decisionLatency(records: LogRecord[]): {
  p95: number | null;
  p99: number | null;
  count: number;
} {
  const values = records
    .map(decisionMs)
    .filter((v): v is number => v !== null)
    .sort((a, b) => a - b);
  return { p95: percentile(values, 95), p99: percentile(values, 99), count: values.length };
}

/** Current (last bucket) and peak requests per second of a series, with the peak's bucket start. */
export function rates(
  buckets: UsageBucket[],
  seconds: number,
  now: Date = new Date(),
): { current: number; peak: number; peakAt: string | null } | null {
  if (buckets.length === 0 || seconds <= 0) return null;
  let peak = 0;
  let peakAt: string | null = null;
  for (const b of buckets) {
    const rate = b.requests / seconds;
    if (rate > peak) {
      peak = rate;
      peakAt = b.start;
    }
  }
  // The last bucket is still filling: its rate is over the time it has run so far.
  const last = buckets[buckets.length - 1];
  const elapsed = last
    ? Math.min(seconds, Math.max(60, (now.getTime() - new Date(last.start).getTime()) / 1000))
    : seconds;
  return { current: (last?.requests ?? 0) / elapsed, peak, peakAt };
}

/** Blocks per guardrail category, largest first (routing blocks file under their rule's category). */
export function blocksByCategory(rows: UsageBucket[]): { category: string; blocks: number }[] {
  const map = new Map<string, number>();
  for (const r of rows) {
    const c = r.group.guardrail_category;
    const n = r.guardrail_blocks ?? 0;
    if (!c || n === 0) continue;
    map.set(c, (map.get(c) ?? 0) + n);
  }
  return [...map.entries()]
    .map(([category, blocks]) => ({ category, blocks }))
    .sort((a, b) => b.blocks - a.blocks);
}

/**
 * The firing alerts that concern a rule: scoped to the route and the rule (or the route alone when
 * the alert names no rule), or to one of the rule's deployments.
 */
export function alertsOfRule(alerts: Alert[], route: string, rule: Rule): Alert[] {
  const deployments = new Set(targetsOf(rule.target).map((t) => t.deployment));
  return alerts.filter((a) => {
    if (a.status === 'resolved') return false;
    const s = a.scope ?? {};
    if (s.deployment && deployments.has(s.deployment)) return true;
    if (s.route === route) return !s.rule || s.rule === rule.name;
    return false;
  });
}

/** The canary and mirror results of each rule, by rule index, from the route's state. */
export function resultsByRule(
  entries: RouteStateEntry[],
  route: string,
  rootId?: string,
): Map<number | 'default', RouteStateEntry> {
  const out = new Map<number | 'default', RouteStateEntry>();
  for (const e of entries) {
    if (!e.canary && !e.mirror && !e.picker) continue;
    const rule = ruleOfPath(e.path, route, rootId);
    if (rule !== null && !out.has(rule)) out.set(rule, e);
  }
  return out;
}

export type Outcome = 'ok' | 'fallback' | 'retry' | 'block' | 'error';

/** What became of a request: answered, after a fallback or a retry, blocked, or failed. */
export function outcomeOf(r: LogRecord): Outcome {
  if (r.code === 'guardrail_blocked' || (r.stage === 'route' && r.status_code >= 400 && r.status_code < 500))
    return 'block';
  if (r.status_code >= 400) return 'error';
  if (r.response_path === 'fallback') return 'fallback';
  if (r.attempt_count > 1) return 'retry';
  return 'ok';
}

/** The rule a record went through, as "name" (or "Everything else"), from its route rules. */
export function ruleOfRecord(
  r: LogRecord,
  route: string,
): { index: number | 'default'; name: string } | null {
  if (r.route !== route) return null;
  const rules = r.route_rules ?? [];
  const first = rules[0];
  if (first) return { index: first.index, name: first.name ?? `rule ${first.index + 1}` };
  if (r.route_path) {
    const rule = ruleOfPath(r.route_path, route);
    if (rule === 'default') return { index: 'default', name: 'Everything else' };
    if (typeof rule === 'number') return { index: rule, name: `rule ${rule + 1}` };
  }
  return null;
}

/** Why the request went where it did, in words. */
export function whyOf(r: LogRecord, rule: { name: string } | null, condition: string | null): string {
  const parts: string[] = [];
  if (rule)
    parts.push(
      rule.name === 'Everything else'
        ? 'No rule matched'
        : `Matched ${rule.name}${condition ? ` (${condition})` : ''}`,
    );
  const tried = r.attempts.map((a) => a.deployment);
  if (r.response_path === 'fallback' && tried.length > 1)
    parts.push(`fell back from ${tried[0]} after ${r.attempts[0]?.reason ?? 'an error'}`);
  else if (r.attempt_count > 1) parts.push(`retried ${r.attempt_count - 1}×`);
  if (outcomeOf(r) === 'block') parts.push(r.reason ? `blocked: ${r.reason}` : 'blocked');
  return parts.join('; ') || 'Routed';
}

export interface FeedItem {
  at: string;
  kind: 'config' | 'alert' | 'canary';
  title: string;
  detail: string;
  actor: string | null;
  tone: 'neutral' | 'warn' | 'error' | 'success';
}

const RESOURCE_LABEL: Record<string, string> = {
  'settings/resilience': 'Breaker and health defaults',
  'settings/policies': 'Retry and timeout defaults',
  'settings/server': 'Admission cap',
};

/**
 * Routing changes over time: route and default edits from the audit log, breaker and canary
 * alerts, and the current canary states of the shown route.
 */
export function feedOf(
  audit: AuditEntry[],
  alerts: Alert[],
  state: RouteStateEntry[],
  route: string | null,
  rules: Rule[] = [],
): FeedItem[] {
  const items: FeedItem[] = [];
  for (const e of audit) {
    if (e.outcome !== 'success') continue;
    const resource = e.resource ?? '';
    const verb = e.action.endsWith('.delete')
      ? 'deleted'
      : e.action.endsWith('.create')
        ? 'created'
        : 'changed';
    const title = resource.startsWith('routes/')
      ? resource.endsWith('/affinity')
        ? `Sessions of route ${resource.split('/')[1] ?? ''} reset`
        : `Route ${resource.slice('routes/'.length)} ${verb}`
      : `${RESOURCE_LABEL[resource] ?? resource} ${verb}`;
    items.push({
      at: e.time,
      kind: 'config',
      title,
      detail: e.reason ?? (e.generation_after != null ? `Configuration ${e.generation_after}` : ''),
      actor: e.actor.id,
      tone: 'neutral',
    });
  }
  for (const a of alerts) {
    const s = a.scope ?? {};
    const routing =
      Boolean(s.deployment || s.route) ||
      /breaker|canary|deployment|routing|provider/.test(`${a.category} ${a.rule}`);
    if (!routing) continue;
    items.push({
      at: a.fired_at,
      kind: `${a.category} ${a.rule}`.includes('canary') ? 'canary' : 'alert',
      title: a.summary,
      detail: a.status === 'resolved' ? 'Resolved' : a.status === 'acknowledged' ? 'Acknowledged' : 'Firing',
      actor: a.acknowledged_by ?? a.resolved_by ?? null,
      tone: a.status === 'resolved' ? 'success' : a.severity === 'critical' ? 'error' : 'warn',
    });
  }
  if (route)
    for (const e of state) {
      if (!e.canary || (e.canary.state !== 'rolled_back' && e.canary.state !== 'held')) continue;
      const index = ruleOfPath(e.path, route);
      const name = typeof index === 'number' ? (rules[index]?.name ?? `rule ${index + 1}`) : e.path;
      items.push({
        at: '',
        kind: 'canary',
        title: `Canary of ${name} ${e.canary.state === 'held' ? 'held' : 'rolled back'}`,
        detail: `${e.canary.child} passed its error budget${
          e.canary.error_rate != null ? ` at ${(e.canary.error_rate * 100).toFixed(1)}% errors` : ''
        }`,
        actor: 'gateway',
        tone: 'warn',
      });
    }
  return items.sort((a, b) => (b.at || '9').localeCompare(a.at || '9'));
}
