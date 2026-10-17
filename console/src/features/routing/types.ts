/** Route trees as the console edits them: a conditional root whose rules are the "rule list". */

import { plural } from '@/lib/format';

export type Json = unknown;

export function isRecord(value: Json): value is Record<string, Json> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export type Operator = 'eq' | 'ne' | 'gt' | 'lt' | 'in' | 'nin' | 'contains';
export const OPERATORS: { value: Operator; label: string }[] = [
  { value: 'eq', label: '=' },
  { value: 'ne', label: '≠' },
  { value: 'gt', label: '>' },
  { value: 'lt', label: '<' },
  { value: 'in', label: 'in' },
  { value: 'nin', label: 'not in' },
  { value: 'contains', label: 'contains' },
];

/**
 * The fields a condition reads, by kind. `metadata` and `guardrail` take a key (the metadata key, or
 * the guardrail's name and the result it reads); `sample` is a share of requests, not a field.
 */
export type FieldKind =
  | 'team'
  | 'project'
  | 'key'
  | 'model'
  | 'endpoint'
  | 'tag'
  | 'input_tokens'
  | 'metadata'
  | 'guardrail'
  | 'sample';

export const FIELDS: { value: FieldKind; label: string; selector?: string; numeric?: boolean }[] = [
  { value: 'team', label: 'team', selector: 'identity.team' },
  { value: 'project', label: 'project', selector: 'identity.project' },
  { value: 'key', label: 'key', selector: 'identity.key' },
  { value: 'model', label: 'model', selector: 'request.model' },
  { value: 'endpoint', label: 'endpoint', selector: 'request.endpoint' },
  { value: 'tag', label: 'tag', selector: 'identity.tags' },
  { value: 'input_tokens', label: 'input tokens', selector: 'request.input_tokens', numeric: true },
  { value: 'metadata', label: 'metadata' },
  { value: 'guardrail', label: 'guardrail result' },
  { value: 'sample', label: 'share of requests' },
];

/** What a guardrail condition reads of the guardrail's verdict */
export const GUARD_FIELDS: { value: string; label: string }[] = [
  { value: 'passed', label: 'passed' },
  { value: 'verdict', label: 'verdict' },
  { value: 'category', label: 'category' },
];

export interface Condition {
  kind: FieldKind;
  op: Operator;
  value: string;
  /** The metadata key, or the guardrail's name. */
  key: string;
  /** The guardrail result read: passed, verdict or category. */
  guardField: string;
}

/** The selector a condition writes: identity.team, metadata.tier, guard.pii.passed. */
export function selectorOf(c: Pick<Condition, 'kind' | 'key' | 'guardField'>): string {
  if (c.kind === 'metadata') return `metadata.${c.key.trim()}`;
  if (c.kind === 'guardrail') return `guard.${c.key.trim()}.${c.guardField || 'passed'}`;
  return FIELDS.find((f) => f.value === c.kind)?.selector ?? c.kind;
}

/** The condition a selector reads back as, or null for a selector the wizard doesn't offer. */
export function conditionKind(selector: string): Pick<Condition, 'kind' | 'key' | 'guardField'> | null {
  const plain = FIELDS.find((f) => f.selector === selector);
  if (plain) return { kind: plain.value, key: '', guardField: 'passed' };
  if (selector.startsWith('metadata.'))
    return { kind: 'metadata', key: selector.slice('metadata.'.length), guardField: 'passed' };
  const guard = /^guard\.([a-z][a-z0-9_]*)\.(.+)$/.exec(selector);
  if (guard) return { kind: 'guardrail', key: guard[1] ?? '', guardField: guard[2] ?? 'passed' };
  return null;
}

export type Strategy =
  | 'single'
  | 'weighted'
  | 'fallback'
  | 'least_busy'
  | 'least_latency'
  | 'canary'
  | 'adaptive'
  | 'shadow'
  | 'tier'
  | 'pool'
  | 'cost_quality'
  | 'block';

export const STRATEGIES: { value: Strategy; label: string; description: string }[] = [
  {
    value: 'single',
    label: 'Send to one target',
    description: 'Every matching request goes to one deployment.',
  },
  { value: 'weighted', label: 'Weighted split', description: 'Split traffic across targets by weight.' },
  {
    value: 'fallback',
    label: 'Try primary, then fall back',
    description: 'Targets in order; the next is tried when one fails.',
  },
  { value: 'least_busy', label: 'Least busy', description: 'The target with the fewest requests in flight.' },
  {
    value: 'least_latency',
    label: 'Least latency',
    description: 'The target with the lowest recent latency.',
  },
  {
    value: 'canary',
    label: 'Canary',
    description: 'Ramp a new deployment in by steps, and take it out again when its errors pass a budget.',
  },
  {
    value: 'adaptive',
    label: 'Adaptive split',
    description:
      'Learns from feedback which deployment answers best and sends it more traffic, until one clearly wins.',
  },
  {
    value: 'shadow',
    label: 'Shadow',
    description:
      'Answer from one deployment and send a copy to another, to compare them. The copy never reaches the app.',
  },
  {
    value: 'tier',
    label: 'Tier',
    description:
      'Pick among the models of a tier, such as the cheapest fast model. New models join on their own.',
  },
  {
    value: 'pool',
    label: 'Self-hosted pool',
    description:
      'Replicas of one self-hosted model server. The gateway picks by queue, cache and prompt, or an external endpoint picker chooses.',
  },
  {
    value: 'cost_quality',
    label: 'Strong or weak model',
    description:
      'A learned scorer rates each prompt: the ones likely to need it go to the strong model, the rest to a cheaper one.',
  },
  {
    value: 'block',
    label: 'Block',
    description: 'Refuse the request. It shows as a block on the Safety & PII screen.',
  },
];

/** Every node type of a route tree, for the strategy reference. */
export const NODE_TYPES: { type: string; label: string; description: string; advanced?: string }[] = [
  { type: 'deployment', label: 'One deployment', description: 'Sends the request to one deployment.' },
  {
    type: 'provider_model',
    label: 'Provider model',
    description: 'A provider and an upstream model id, for wildcard Models.',
  },
  { type: 'route', label: 'Shared route', description: 'Uses another shared route’s rules in this place.' },
  {
    type: 'fallback',
    label: 'Try primary, then fall back',
    description: 'The first target that can serve; the next when one fails.',
  },
  {
    type: 'weighted',
    label: 'Weighted split',
    description:
      'Splits traffic by weight, optionally sticky per session, user, key, header or metadata. Also holds canaries and A/B experiments.',
    advanced: 'An experiment can learn its split from feedback (adaptive routing).',
  },
  {
    type: 'least_latency',
    label: 'Least latency',
    description: 'The target answering fastest lately, comparing two at a time.',
  },
  { type: 'least_busy', label: 'Least busy', description: 'The target with the fewest requests in flight.' },
  { type: 'least_cost', label: 'Least cost', description: 'The cheapest target for this request.' },
  {
    type: 'conditional',
    label: 'Rules',
    description: 'The first rule whose condition holds; the rule list on this screen is one.',
  },
  {
    type: 'classifier',
    label: 'Classifier',
    description: 'Sends the prompt where a keyword, pattern, embedding or model classifies it.',
  },
  {
    type: 'cost_quality',
    label: 'Cost and quality',
    description: 'A strong or a weak target, by how likely the strong one is to answer better.',
    advanced: 'The score can come from a learned scorer.',
  },
  {
    type: 'pool',
    label: 'Self-hosted pool',
    description: 'Replicas of a self-hosted server, picked by queue, cache and prompt prefix.',
    advanced: 'An external picker can choose, and adapters can stay on the replica that has them loaded.',
  },
  {
    type: 'tier',
    label: 'Tier',
    description: 'The models of one tier, by cost, latency, weight or order.',
  },
  {
    type: 'block',
    label: 'Block',
    description: 'Refuses the request with 400, 403 or 451, as a guardrail block.',
  },
];

export interface Target {
  deployment: string;
  weight: number;
  /** The role label, written as the child's id (primary, backup, stable, canary). */
  role: string;
}

export interface Rule {
  index: number;
  name: string;
  description: string;
  enabled: boolean;
  tags: string[];
  when: Json;
  target: Json;
}

export function rulesOf(tree: Json): { rules: Rule[]; fallback: Json } | null {
  if (!isRecord(tree) || tree.type !== 'conditional' || !Array.isArray(tree.rules)) return null;
  const rules = (tree.rules as Json[]).map((r, index): Rule => {
    const rec = isRecord(r) ? r : {};
    return {
      index,
      name: typeof rec.name === 'string' ? rec.name : `rule ${index + 1}`,
      description: typeof rec.description === 'string' ? rec.description : '',
      enabled: rec.enabled !== false,
      tags: Array.isArray(rec.tags) ? rec.tags.filter((t): t is string => typeof t === 'string') : [],
      when: rec.when,
      target: rec.target,
    };
  });
  return { rules, fallback: tree.default };
}

const OP_TEXT: Record<string, string> = {
  eq: 'is',
  ne: 'is not',
  gt: 'is over',
  gte: 'is at least',
  lt: 'is under',
  lte: 'is at most',
  in: 'is one of',
  nin: 'is not one of',
  contains: 'contains',
  regex: 'matches',
  exists: 'exists',
};

function fieldLabel(field: string): string {
  const kind = conditionKind(field);
  if (!kind) return field;
  if (kind.kind === 'metadata') return `metadata ${kind.key}`;
  if (kind.kind === 'guardrail') return `guardrail ${kind.key} ${kind.guardField}`;
  return FIELDS.find((f) => f.value === kind.kind)?.label ?? field;
}

/** A share as words: 0.1 → "10%", 0.005 → "0.5%". */
export function sharePercent(ratio: number): string {
  const pct = ratio * 100;
  return `${Number.isInteger(pct) ? pct : Number(pct.toFixed(2))}%`;
}

/** A predicate in plain words: "team is support-ai AND input tokens is over 100000". */
export function describePredicate(p: Json): string {
  if (p === undefined || p === null) return 'always';
  if (!isRecord(p)) return JSON.stringify(p);
  const parts: string[] = [];
  if (Array.isArray(p.all)) parts.push((p.all as Json[]).map(describePredicate).join(' AND '));
  if (Array.isArray(p.any)) parts.push((p.any as Json[]).map(describePredicate).join(' OR '));
  if (p.not !== undefined) parts.push(`NOT (${describePredicate(p.not)})`);
  if (typeof p.field === 'string') {
    const op = Object.keys(OP_TEXT).find((k) => k in p);
    const value = op ? p[op] : undefined;
    parts.push(
      `${fieldLabel(p.field)} ${op ? OP_TEXT[op] : '?'} ${Array.isArray(value) ? value.join(', ') : String(value)}`,
    );
  }
  if (typeof p.sample === 'number') parts.push(`${sharePercent(p.sample)} of requests`);
  return parts.length > 0 ? parts.join(' AND ') : JSON.stringify(p);
}

/** The wizard strategy a node reads as, or `other` for a node the wizard can't build. */
export function strategyOf(node: Json): Strategy | 'other' {
  if (typeof node === 'string') return 'single';
  if (!isRecord(node)) return 'other';
  const t = node.type ?? (typeof node.deployment === 'string' ? 'deployment' : undefined);
  if (t === 'deployment') return isRecord(node.mirror) ? 'shadow' : 'single';
  if (t === 'weighted')
    return isRecord(node.canary) ? 'canary' : node.algorithm === 'thompson' ? 'adaptive' : 'weighted';
  if (t === 'fallback' || t === 'least_busy' || t === 'least_latency' || t === 'block' || t === 'tier')
    return t;
  if (t === 'pool') return 'pool';
  if (t === 'cost_quality' && leafOf(node.strong) !== null && leafOf(node.weak) !== null)
    return 'cost_quality';
  return 'other';
}

/** The node type label of any node, for rules the wizard can't build. */
export function nodeTypeLabel(node: Json): string {
  const type = isRecord(node) ? node.type : typeof node === 'string' ? 'deployment' : undefined;
  return NODE_TYPES.find((n) => n.type === type)?.label ?? 'Custom';
}

/** A plain deployment leaf (a name, or `{deployment}` with an optional id and weight), or null. */
export function leafOf(c: Json): Target | null {
  if (typeof c === 'string') return { deployment: c, weight: 1, role: '' };
  if (
    isRecord(c) &&
    typeof c.deployment === 'string' &&
    (c.type === undefined || c.type === 'deployment') &&
    !isRecord(c.mirror)
  )
    return {
      deployment: c.deployment,
      weight: typeof c.weight === 'number' ? c.weight : 1,
      role: typeof c.id === 'string' ? c.id : '',
    };
  return null;
}

/** The plain deployment children of a node, with weights and role labels. */
export function targetsOf(node: Json): Target[] {
  const leaf = (c: Json): Target | null => {
    if (isRecord(c) && isRecord(c.mirror)) return leafOf({ ...c, mirror: undefined });
    return leafOf(c);
  };
  const self = leaf(node);
  if (self) return [self];
  if (isRecord(node) && node.type === 'cost_quality') {
    const strong = leafOf(node.strong);
    const weak = leafOf(node.weak);
    return [...(strong ? [{ ...strong, role: 'strong' }] : []), ...(weak ? [{ ...weak, role: 'weak' }] : [])];
  }
  if (isRecord(node) && Array.isArray(node.children))
    return (node.children as Json[]).flatMap((c) => {
      const t = leaf(c);
      return t ? [t] : [];
    });
  return [];
}

/** Whether every child of a composite is a plain deployment, so the wizard can edit its targets. */
export function plainTargets(node: Json): boolean {
  if (typeof node === 'string') return true;
  if (!isRecord(node)) return false;
  if (node.type === 'cost_quality') return strategyOf(node) === 'cost_quality';
  if (!Array.isArray(node.children)) return strategyOf(node) !== 'other';
  return targetsOf(node).length === (node.children as Json[]).length;
}

/** The canary of a weighted node as written. */
export interface CanaryConfig {
  child: string;
  steps: { weight: number; after: string }[];
  budget: { max_error_rate: number; window: string; action: 'rollback' | 'hold' } | null;
}

export function canaryOf(node: Json): CanaryConfig | null {
  if (!isRecord(node) || !isRecord(node.canary) || typeof node.canary.child !== 'string') return null;
  const c = node.canary;
  const steps = Array.isArray(c.steps)
    ? (c.steps as Json[]).flatMap((s) =>
        isRecord(s) && typeof s.weight === 'number'
          ? [{ weight: s.weight, after: typeof s.after === 'string' ? s.after : '0s' }]
          : [],
      )
    : [];
  const b = isRecord(c.error_budget) ? c.error_budget : null;
  return {
    child: c.child as string,
    steps,
    budget:
      b && typeof b.max_error_rate === 'number'
        ? {
            max_error_rate: b.max_error_rate,
            window: typeof b.window === 'string' ? b.window : '5m',
            action: b.action === 'hold' ? 'hold' : 'rollback',
          }
        : null,
  };
}

/** The mirror of a node as written. */
export interface MirrorConfig {
  target: string;
  fraction: number;
  sampleBy: 'request' | 'session';
}

export function mirrorOf(node: Json): MirrorConfig | null {
  if (!isRecord(node) || !isRecord(node.mirror)) return null;
  const m = node.mirror;
  const target = targetsOf(m.target)[0]?.deployment ?? nodeTypeLabel(m.target);
  return {
    target,
    fraction: typeof m.fraction === 'number' ? m.fraction : 0,
    sampleBy: m.sample_by === 'session' ? 'session' : 'request',
  };
}

/** The per-rule resilience written on a target: timeouts, attempts and the classes that move on. */
export function resilienceOf(node: Json): Record<string, Json> {
  if (!isRecord(node) || !isRecord(node.policies) || !isRecord(node.policies.resilience)) return {};
  return node.policies.resilience;
}

/** Facts of a target's settings for the rule's detail: "Attempt timeout 8s", "At most 3 attempts". */
export function settingsOf(node: Json): { label: string; value: string }[] {
  const out: { label: string; value: string }[] = [];
  const r = resilienceOf(node);
  const timeouts = isRecord(r.timeouts) ? r.timeouts : {};
  const retry = isRecord(r.retry) ? r.retry : {};
  if (typeof timeouts.attempt === 'string') out.push({ label: 'Attempt timeout', value: timeouts.attempt });
  if (typeof timeouts.deadline === 'string') out.push({ label: 'Time limit', value: timeouts.deadline });
  if (typeof retry.max_attempts === 'number')
    out.push({ label: 'Attempts at most', value: String(retry.max_attempts) });
  const off = TRIGGERS.filter((t) => !triggerOn(node, t.value)).map((t) => t.short);
  if (off.length > 0) out.push({ label: 'Doesn’t move on after', value: off.join(', ') });
  if (isRecord(node) && typeof node.sticky_by === 'string')
    out.push({ label: 'Sticky by', value: stickyLabel(node.sticky_by) });
  return out;
}

/** "header:x-user" → "header x-user"; "session" → "session". */
export function stickyLabel(value: string): string {
  const [kind, ...rest] = value.split(':');
  const name = rest.join(':');
  const label = kind === 'end_user' ? 'end user' : kind;
  return name ? `${label} ${name}` : (label ?? value);
}

/** Error classes a rule can stop moving on from, with the actions kept when it doesn't move on. */
export const TRIGGERS: {
  value: 'server_error' | 'timeout' | 'rate_limited' | 'overloaded';
  label: string;
  short: string;
  keep: string[];
}[] = [
  { value: 'server_error', label: 'Server errors (5xx)', short: 'server errors', keep: ['retry_same'] },
  { value: 'timeout', label: 'Timeouts', short: 'timeouts', keep: ['retry_same'] },
  {
    value: 'rate_limited',
    label: 'Rate limits (429)',
    short: 'rate limits',
    keep: ['rotate_key', 'retry_same'],
  },
  { value: 'overloaded', label: 'Overloaded (503, 529)', short: 'overloads', keep: ['retry_same'] },
];

/** Whether failures of a class move on to another deployment (the default for every one of them). */
export function triggerOn(node: Json, cls: string): boolean {
  const retry = resilienceOf(node).retry;
  const classes = isRecord(retry) && isRecord(retry.classes) ? retry.classes : {};
  const c = classes[cls];
  if (!isRecord(c) || !Array.isArray(c.actions)) return true;
  return (c.actions as Json[]).includes('next_deployment');
}

/** Advanced settings of a node or its policies, read-only in the console (bandit, scorer, picker, hedging). */
export function advancedOf(node: Json): { label: string; value: string }[] {
  const out: { label: string; value: string }[] = [];
  const visit = (n: Json) => {
    if (!isRecord(n)) return;
    if (n.algorithm === 'thompson') {
      const arms = Array.isArray(n.arms) ? (n.arms as Json[]).join(', ') : 'every weighted target';
      out.push({
        label: 'Adaptive split',
        value: `learns from ${typeof n.metric === 'string' ? n.metric : 'feedback'} over ${arms}`,
      });
    }
    if (isRecord(n.scorer) && n.scorer.kind === 'matrix_factorization')
      out.push({
        label: 'Learned scorer',
        value: `matrix factorisation${typeof n.scorer.model_file === 'string' ? `, ${n.scorer.model_file}` : ''}`,
      });
    if (n.picker === 'external')
      out.push({
        label: 'Endpoint picker',
        value:
          isRecord(n.picker_service) && typeof n.picker_service.url === 'string'
            ? n.picker_service.url
            : 'external service',
      });
    if (isRecord(n.scheduler) && n.scheduler.lora === true)
      out.push({ label: 'Adapter affinity', value: 'on' });
    const r = resilienceOf(n);
    if (isRecord(r.hedging) && r.hedging.enabled === true)
      out.push({
        label: 'Hedged requests',
        value: `after ${typeof r.hedging.delay === 'string' ? r.hedging.delay : '2s'}, up to ${
          typeof r.hedging.max_extra === 'number' ? r.hedging.max_extra : 1
        } more`,
      });
    if (isRecord(r.retry) && Array.isArray(r.retry.retry_on_response) && r.retry.retry_on_response.length > 0)
      out.push({
        label: 'Failed answers',
        value: `${plural(r.retry.retry_on_response.length, 'check')} on successful answers`,
      });
    for (const key of ['children', 'classes'] as const)
      if (Array.isArray(n[key]))
        for (const c of n[key] as Json[]) visit(isRecord(c) && 'target' in c ? c.target : c);
    for (const key of ['strong', 'weak', 'default'] as const) if (n[key] !== undefined) visit(n[key]);
  };
  visit(node);
  return out;
}

/** Replaces, inserts or removes a rule of the conditional root; returns the new tree. */
export function withRules(tree: Json, rules: Json[]): Json {
  if (!isRecord(tree)) return tree;
  return { ...tree, rules };
}

/**
 * What deleting rule `index` does to a route: the tree with one rule fewer; for the last rule, the
 * route's other target alone (a decision with no rule left is its default), or, with none, the
 * route goes.
 */
export function afterDeletingRule(
  tree: Json,
  rules: Json[],
  index: number,
): { kind: 'tree'; tree: Json } | { kind: 'default'; tree: Json } | { kind: 'route' } {
  const rest = rules.filter((_, i) => i !== index);
  if (rest.length > 0) return { kind: 'tree', tree: withRules(tree, rest) };
  const fallback = isRecord(tree) ? tree.default : undefined;
  return fallback !== undefined && fallback !== null
    ? { kind: 'default', tree: fallback }
    : { kind: 'route' };
}

/** A copy of a rule placed after it, named "<name> (copy)" or "(copy 2)" when that is taken. */
export function duplicateRule(rules: Json[], index: number): Json[] {
  const original = rules[index];
  if (!isRecord(original)) return rules;
  const names = new Set(rules.map((r) => (isRecord(r) && typeof r.name === 'string' ? r.name : '')));
  const base = typeof original.name === 'string' ? original.name : `rule ${index + 1}`;
  let name = `${base} (copy)`;
  for (let n = 2; names.has(name); n++) name = `${base} (copy ${n})`;
  const copy: Record<string, Json> = {
    ...(JSON.parse(JSON.stringify(original)) as Record<string, Json>),
    name: name.slice(0, 100),
    enabled: false,
  };
  return [...rules.slice(0, index + 1), copy, ...rules.slice(index + 1)];
}
