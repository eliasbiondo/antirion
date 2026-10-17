/** The rule wizard's draft: what each step edits, and the rule object it writes into the tree. */

import {
  blankHedging,
  type HedgingDraft,
  hedgingFrom,
  hedgingTo,
  type PredicateDraft,
  predicatesFrom,
  predicateComplete,
  predicatesTo,
  predicateText,
} from './resilience';
import {
  canaryOf,
  type Condition,
  conditionKind,
  describePredicate,
  FIELDS,
  isRecord,
  type Json,
  leafOf,
  mirrorOf,
  type Operator,
  plainTargets,
  resilienceOf,
  type Rule,
  selectorOf,
  sharePercent,
  stickyLabel,
  type Strategy,
  strategyOf,
  type Target,
  targetsOf,
  TRIGGERS,
  triggerOn,
} from './types';

export type StickyKind = '' | 'session' | 'end_user' | 'key' | 'header' | 'metadata';
export type TierStrategy = 'least_cost' | 'least_latency' | 'weighted' | 'fallback';
export type TriggerClass = (typeof TRIGGERS)[number]['value'];

export interface RuleDraft {
  name: string;
  description: string;
  enabled: boolean;
  tags: string[];
  join: 'all' | 'any';
  conditions: Condition[];
  /** Percent of requests the rule takes (the sample predicate); empty for every request. */
  sample: string;
  strategy: Strategy;
  targets: Target[];
  block: { status: 400 | 403 | 451; message: string; category: string };
  canary: {
    steps: { weight: string; after: string }[];
    budget: boolean;
    maxErrorRate: string;
    window: string;
    action: 'rollback' | 'hold';
  };
  shadow: { target: string; percent: string; sampleBy: 'request' | 'session' };
  /** An adaptive split: the experiment, the feedback metric it learns from and its exploration. */
  adaptive: { experiment: string; metric: string; minSamples: string; stop: boolean; delta: string };
  tier: { tier: string; strategy: TierStrategy; includePreview: boolean };
  /** A self-hosted pool: who picks the replica, and whether adapters stay where they are loaded. */
  pool: {
    picker: 'builtin' | 'external';
    /** The external endpoint picker's address and how long a pick may take (empty: 300ms). */
    url: string;
    timeout: string;
    /** When the picker fails: refuse the request, or let the gateway pick. */
    onError: 'reject' | 'builtin';
    lora: boolean;
  };
  /** A strong or a weak model, by a learned (matrix-factorisation) scorer. */
  costQuality: {
    strong: string;
    weak: string;
    /** Above a score (`threshold`) or a share of prompts (`strong_fraction`), in percent. */
    by: 'threshold' | 'fraction';
    percent: string;
    /** `kept`: another kind of scorer, kept as written. */
    scorer: 'matrix_factorization' | 'kept';
    modelFile: string;
    embeddingModel: string;
    strongName: string;
    weakName: string;
    onError: 'strong' | 'weak' | 'reject';
  };
  sticky: { kind: StickyKind; name: string };
  /** Per-rule resilience; empty strings keep the defaults. */
  limits: { attempt: string; deadline: string; maxAttempts: string };
  triggers: Record<TriggerClass, boolean>;
  /** Hedged requests: another attempt when the first is slow. */
  hedging: HedgingDraft;
  /** Answers that count as failures: the next deployment is tried. */
  predicates: PredicateDraft[];
  /** The original target, kept when the wizard can't rebuild it (nested nodes, advanced settings). */
  original: Json;
}

export function emptyCondition(kind: Condition['kind'] = 'team'): Condition {
  return { kind, op: 'eq', value: '', key: '', guardField: 'passed' };
}

const DEFAULT_TRIGGERS: Record<TriggerClass, boolean> = {
  server_error: true,
  timeout: true,
  rate_limited: true,
  overloaded: true,
};

export function blankDraft(): RuleDraft {
  return {
    name: '',
    description: '',
    enabled: true,
    tags: [],
    join: 'all',
    conditions: [emptyCondition()],
    sample: '',
    strategy: 'single',
    targets: [{ deployment: '', weight: 1, role: '' }],
    block: { status: 403, message: 'Request blocked by routing policy.', category: 'custom' },
    canary: {
      steps: [
        { weight: '5', after: '0s' },
        { weight: '25', after: '1h' },
        { weight: '50', after: '6h' },
        { weight: '100', after: '24h' },
      ],
      budget: true,
      maxErrorRate: '2',
      window: '5m',
      action: 'rollback',
    },
    shadow: { target: '', percent: '10', sampleBy: 'request' },
    adaptive: { experiment: '', metric: '', minSamples: '10', stop: false, delta: '5' },
    tier: { tier: 'fast', strategy: 'least_cost', includePreview: false },
    pool: { picker: 'builtin', url: '', timeout: '', onError: 'reject', lora: false },
    costQuality: {
      strong: '',
      weak: '',
      by: 'threshold',
      percent: '50',
      scorer: 'matrix_factorization',
      modelFile: '',
      embeddingModel: '',
      strongName: '',
      weakName: '',
      onError: 'strong',
    },
    sticky: { kind: '', name: '' },
    limits: { attempt: '', deadline: '', maxAttempts: '' },
    triggers: { ...DEFAULT_TRIGGERS },
    hedging: blankHedging(),
    predicates: [],
    original: undefined,
  };
}

const OPS: Operator[] = ['eq', 'ne', 'gt', 'lt', 'in', 'nin', 'contains'];

/** The conditions of a predicate the wizard can show; null when it has parts the wizard can't. */
function conditionsFrom(p: Json): { join: 'all' | 'any'; conditions: Condition[]; sample: string } | null {
  if (p === undefined) return { join: 'all', conditions: [], sample: '' };
  if (!isRecord(p)) return null;
  let sample = '';
  let rest: Record<string, Json> = p;
  if (typeof p.sample === 'number') {
    sample = sharePercent(p.sample).replace('%', '');
    rest = Object.fromEntries(Object.entries(p).filter(([k]) => k !== 'sample'));
  }
  if (Object.keys(rest).length === 0) return { join: 'all', conditions: [], sample };
  const join: 'all' | 'any' = Array.isArray(rest.any) ? 'any' : 'all';
  const list = Array.isArray(rest.all)
    ? (rest.all as Json[])
    : Array.isArray(rest.any)
      ? (rest.any as Json[])
      : [rest];
  if ((Array.isArray(rest.all) || Array.isArray(rest.any)) && Object.keys(rest).length > 1) return null;
  const conditions: Condition[] = [];
  for (const c of list) {
    if (!isRecord(c)) return null;
    if (typeof c.sample === 'number' && Object.keys(c).length === 1 && join === 'all' && !sample) {
      sample = sharePercent(c.sample).replace('%', '');
      continue;
    }
    if (typeof c.field !== 'string') return null;
    const kind = conditionKind(c.field);
    const op = OPS.find((o) => o in c);
    if (!kind || !op || Object.keys(c).length !== 2) return null;
    const v = c[op];
    conditions.push({ ...kind, op, value: Array.isArray(v) ? v.join(', ') : String(v) });
  }
  return { join, conditions, sample };
}

function limitsOf(node: Json): RuleDraft['limits'] {
  const r = resilienceOf(node);
  const t = isRecord(r.timeouts) ? r.timeouts : {};
  const retry = isRecord(r.retry) ? r.retry : {};
  return {
    attempt: typeof t.attempt === 'string' ? t.attempt : '',
    deadline: typeof t.deadline === 'string' ? t.deadline : '',
    maxAttempts: typeof retry.max_attempts === 'number' ? String(retry.max_attempts) : '',
  };
}

function stickyOf(node: Json): RuleDraft['sticky'] {
  if (!isRecord(node) || typeof node.sticky_by !== 'string') return { kind: '', name: '' };
  const [kind = '', ...rest] = node.sticky_by.split(':');
  return { kind: kind as StickyKind, name: rest.join(':') };
}

function adaptiveOf(node: Record<string, Json>): RuleDraft['adaptive'] {
  const stop = isRecord(node.stop_after) ? node.stop_after : null;
  return {
    experiment: typeof node.experiment === 'string' ? node.experiment : '',
    metric: typeof node.metric === 'string' ? node.metric : '',
    minSamples: typeof node.min_samples_per_arm === 'number' ? String(node.min_samples_per_arm) : '10',
    stop: stop !== null,
    delta: stop && typeof stop.delta === 'number' ? sharePercent(stop.delta).replace('%', '') : '5',
  };
}

function poolOf(node: Record<string, Json>): RuleDraft['pool'] {
  const svc = isRecord(node.picker_service) ? node.picker_service : {};
  return {
    picker: node.picker === 'external' ? 'external' : 'builtin',
    url: typeof svc.url === 'string' ? svc.url : '',
    timeout: typeof svc.timeout === 'string' ? svc.timeout : '',
    onError: node.picker_on_error === 'builtin' ? 'builtin' : 'reject',
    lora: isRecord(node.scheduler) && node.scheduler.lora === true,
  };
}

function costQualityOf(
  node: Record<string, Json>,
  blank: RuleDraft['costQuality'],
): RuleDraft['costQuality'] {
  const scorer = isRecord(node.scorer) ? node.scorer : {};
  const mf = scorer.kind === 'matrix_factorization';
  const pair = isRecord(scorer.models) ? scorer.models : {};
  const fraction = typeof node.strong_fraction === 'number';
  const ratio = fraction ? node.strong_fraction : node.threshold;
  return {
    strong: leafOf(node.strong)?.deployment ?? '',
    weak: leafOf(node.weak)?.deployment ?? '',
    by: fraction ? 'fraction' : 'threshold',
    percent: typeof ratio === 'number' ? sharePercent(ratio).replace('%', '') : blank.percent,
    scorer: mf || !isRecord(node.scorer) ? 'matrix_factorization' : 'kept',
    modelFile: mf && typeof scorer.model_file === 'string' ? scorer.model_file : '',
    embeddingModel: mf && typeof scorer.embedding_model === 'string' ? scorer.embedding_model : '',
    strongName: typeof pair.strong === 'string' ? pair.strong : '',
    weakName: typeof pair.weak === 'string' ? pair.weak : '',
    onError: node.on_error === 'weak' || node.on_error === 'reject' ? node.on_error : 'strong',
  };
}

/** Whether the wizard can show a rule's condition; a rule it can't keeps its condition as written. */
export function conditionEditable(rule: Rule | null): boolean {
  return rule === null || conditionsFrom(rule.when) !== null;
}

export function draftFrom(rule: Rule | null): RuleDraft {
  const blank = blankDraft();
  if (!rule) return blank;
  const node = rule.target;
  const strategy = strategyOf(node);
  const target = isRecord(node) ? node : {};
  const cond = conditionsFrom(rule.when) ?? { join: 'all' as const, conditions: [], sample: '' };
  const canary = canaryOf(node);
  const mirror = mirrorOf(node);
  const targets = targetsOf(node);
  return {
    ...blank,
    name: rule.name,
    description: rule.description,
    enabled: rule.enabled,
    tags: rule.tags,
    ...cond,
    strategy: strategy === 'other' ? 'single' : strategy,
    targets: targets.length > 0 ? targets : blank.targets,
    block: {
      status: target.status === 400 || target.status === 451 ? target.status : 403,
      message: typeof target.message === 'string' ? target.message : blank.block.message,
      category: typeof target.category === 'string' ? target.category : 'custom',
    },
    canary: canary
      ? {
          steps: canary.steps.map((s) => ({ weight: String(s.weight), after: s.after })),
          budget: canary.budget !== null,
          maxErrorRate: canary.budget ? sharePercent(canary.budget.max_error_rate).replace('%', '') : '2',
          window: canary.budget?.window ?? '5m',
          action: canary.budget?.action ?? 'rollback',
        }
      : blank.canary,
    shadow: mirror
      ? {
          target: mirror.target,
          percent: sharePercent(mirror.fraction).replace('%', ''),
          sampleBy: mirror.sampleBy,
        }
      : blank.shadow,
    tier:
      strategy === 'tier'
        ? {
            tier: typeof target.tier === 'string' ? target.tier : 'fast',
            strategy: (typeof target.strategy === 'string' ? target.strategy : 'least_cost') as TierStrategy,
            includePreview: target.include_preview === true,
          }
        : blank.tier,
    adaptive: strategy === 'adaptive' ? adaptiveOf(target) : blank.adaptive,
    pool: strategy === 'pool' ? poolOf(target) : blank.pool,
    costQuality: strategy === 'cost_quality' ? costQualityOf(target, blank.costQuality) : blank.costQuality,
    sticky: stickyOf(node),
    limits: limitsOf(node),
    hedging: hedgingFrom(resilienceOf(node).hedging),
    predicates: predicatesFrom(
      isRecord(resilienceOf(node).retry)
        ? (resilienceOf(node).retry as Record<string, Json>).retry_on_response
        : [],
    ),
    triggers: Object.fromEntries(TRIGGERS.map((t) => [t.value, triggerOn(node, t.value)])) as Record<
      TriggerClass,
      boolean
    >,
    original: node,
  };
}

/** Whether the draft's targets come from the original node as written (the wizard can't edit them). */
export function targetsLocked(draft: RuleDraft): boolean {
  if (draft.original === undefined) return false;
  if (strategyOf(draft.original) === 'other') return true;
  return !plainTargets(draft.original) && strategyOf(draft.original) === draft.strategy;
}

function parseValue(c: Condition): Json {
  const numeric = FIELDS.find((f) => f.value === c.kind)?.numeric;
  if (c.op === 'in' || c.op === 'nin')
    return c.value
      .split(',')
      .map((v) => v.trim())
      .filter(Boolean);
  const text = c.value.trim();
  if (c.kind === 'guardrail' && c.guardField === 'passed' && (text === 'true' || text === 'false'))
    return text === 'true';
  return numeric && /^-?\d+(\.\d+)?$/.test(text) ? Number(text) : text;
}

function ratio(percent: string): number {
  return Math.round(Number(percent) * 100) / 10_000;
}

export function toPredicate(draft: Pick<RuleDraft, 'conditions' | 'join' | 'sample'>): Json {
  const parts: Json[] = draft.conditions.map((c) => ({ field: selectorOf(c), [c.op]: parseValue(c) }));
  const sample = draft.sample.trim() === '' ? undefined : ratio(draft.sample);
  if (parts.length === 0) return sample === undefined ? undefined : { sample };
  const base: Record<string, Json> =
    parts.length === 1 ? (parts[0] as Record<string, Json>) : { [draft.join]: parts };
  // Keys of one predicate object AND together, so a sample beside a condition or a list narrows it.
  return sample === undefined ? base : { ...base, sample };
}

function child(t: Target, withWeight: boolean): Json {
  const role = t.role.trim();
  if (!role && !withWeight) return t.deployment;
  return {
    ...(role ? { id: role } : {}),
    deployment: t.deployment,
    ...(withWeight ? { weight: t.weight } : {}),
  };
}

/** Keys of the original node the wizard manages; every other key is kept as written. */
const MANAGED = new Set([
  'type',
  'children',
  'deployment',
  'canary',
  'tier',
  'strategy',
  'include_preview',
  'sticky_by',
  'status',
  'message',
  'category',
  'policies',
  'algorithm',
  'experiment',
  'metric',
  'min_samples_per_arm',
  'stop_after',
  'picker',
  'picker_service',
  'picker_on_error',
  'scheduler',
  'strong',
  'weak',
  'threshold',
  'strong_fraction',
  'scorer',
  'on_error',
]);

function kept(draft: RuleDraft): Record<string, Json> {
  const o = draft.original;
  if (!isRecord(o) || strategyOf(o) !== draft.strategy) return {};
  const out: Record<string, Json> = {};
  for (const [k, v] of Object.entries(o))
    if (!MANAGED.has(k) && !(k === 'mirror' && draft.strategy === 'shadow')) out[k] = v;
  return out;
}

/** The target's `policies` with the draft's timeouts, attempts and triggers merged in. */
function policiesOf(draft: RuleDraft): Record<string, Json> | undefined {
  const o = isRecord(draft.original) && isRecord(draft.original.policies) ? draft.original.policies : {};
  const resilience = isRecord(o.resilience) ? { ...o.resilience } : {};
  const timeouts = isRecord(resilience.timeouts) ? { ...resilience.timeouts } : {};
  const retry = isRecord(resilience.retry) ? { ...resilience.retry } : {};
  const classes = isRecord(retry.classes) ? { ...retry.classes } : {};
  const set = (obj: Record<string, Json>, key: string, value: Json) => {
    if (value === undefined) Reflect.deleteProperty(obj, key);
    else obj[key] = value;
  };
  set(timeouts, 'attempt', draft.limits.attempt.trim() || undefined);
  set(timeouts, 'deadline', draft.limits.deadline.trim() || undefined);
  set(retry, 'max_attempts', draft.limits.maxAttempts.trim() ? Number(draft.limits.maxAttempts) : undefined);
  if (movesOn(draft.strategy))
    for (const t of TRIGGERS) {
      const current = classes[t.value];
      const on = draft.triggers[t.value];
      const currentOn =
        !isRecord(current) || !Array.isArray(current.actions) || current.actions.includes('next_deployment');
      if (on && !currentOn) Reflect.deleteProperty(classes, t.value);
      if (!on) classes[t.value] = { ...(isRecord(current) ? current : {}), actions: t.keep };
    }
  set(retry, 'classes', Object.keys(classes).length > 0 ? classes : undefined);
  set(retry, 'retry_on_response', draft.predicates.length > 0 ? predicatesTo(draft.predicates) : undefined);
  set(resilience, 'hedging', hedgingTo(draft.hedging));
  set(resilience, 'timeouts', Object.keys(timeouts).length > 0 ? timeouts : undefined);
  set(resilience, 'retry', Object.keys(retry).length > 0 ? retry : undefined);
  const policies: Record<string, Json> = { ...o };
  set(policies, 'resilience', Object.keys(resilience).length > 0 ? resilience : undefined);
  return Object.keys(policies).length > 0 ? policies : undefined;
}

/** Strategies with more than one deployment to move on to after a failure. */
export function movesOn(strategy: Strategy): boolean {
  return [
    'fallback',
    'weighted',
    'least_busy',
    'least_latency',
    'canary',
    'adaptive',
    'tier',
    'pool',
  ].includes(strategy);
}

/** Strategies whose split can stick to a session, user, key, header or metadata value. */
export function canStick(strategy: Strategy): boolean {
  return strategy === 'weighted' || strategy === 'canary';
}

function stickyBy(draft: RuleDraft): string | undefined {
  const { kind, name } = draft.sticky;
  if (!canStick(draft.strategy) || kind === '') return undefined;
  return kind === 'header' || kind === 'metadata' ? `${kind}:${name.trim()}` : kind;
}

export function toTarget(draft: RuleDraft): Json {
  if (targetsLocked(draft)) {
    const o = isRecord(draft.original) ? { ...draft.original } : draft.original;
    if (!isRecord(o)) return o;
    const policies = policiesOf(draft);
    if (policies) o.policies = policies;
    else delete o.policies;
    return o;
  }
  const t = draft.targets.filter((x) => x.deployment);
  const extra = kept(draft);
  const policies = draft.strategy === 'block' ? undefined : policiesOf(draft);
  const withPolicies = (node: Record<string, Json>): Json => ({
    ...extra,
    ...node,
    ...(policies ? { policies } : {}),
  });
  const sticky = stickyBy(draft);
  switch (draft.strategy) {
    case 'single': {
      const first = t[0];
      if (!first) return '';
      if (!policies && !first.role.trim() && Object.keys(extra).length === 0) return first.deployment;
      return withPolicies({
        ...(first.role.trim() ? { id: first.role.trim() } : {}),
        type: 'deployment',
        deployment: first.deployment,
      });
    }
    case 'weighted':
      return withPolicies({
        type: 'weighted',
        children: t.map((x) => child(x, true)),
        ...(sticky ? { sticky_by: sticky } : {}),
      });
    case 'fallback':
    case 'least_busy':
    case 'least_latency':
      return withPolicies({
        type: draft.strategy,
        children: t.map((x) => child(x, draft.strategy === 'least_busy' && x.weight !== 1)),
      });
    case 'canary': {
      const stable = t[0];
      const candidate = t[1];
      const steps = draft.canary.steps
        .filter((s) => s.weight.trim() !== '')
        .map((s) => ({ weight: Number(s.weight), after: s.after.trim() || '0s' }));
      const first = steps[0]?.weight ?? 0;
      const stableRole = stable?.role.trim() || 'stable';
      const candidateRole = candidate?.role.trim() || 'canary';
      return withPolicies({
        type: 'weighted',
        children: [
          { id: stableRole, deployment: stable?.deployment ?? '', weight: Math.max(0, 100 - first) },
          { id: candidateRole, deployment: candidate?.deployment ?? '', weight: first },
        ],
        canary: {
          child: candidateRole,
          steps,
          ...(draft.canary.budget
            ? {
                error_budget: {
                  max_error_rate: ratio(draft.canary.maxErrorRate),
                  window: draft.canary.window.trim() || '5m',
                  action: draft.canary.action,
                },
              }
            : {}),
        },
        ...(sticky ? { sticky_by: sticky } : {}),
      });
    }
    case 'adaptive': {
      const a = draft.adaptive;
      const samples = Number(a.minSamples.trim());
      return withPolicies({
        type: 'weighted',
        algorithm: 'thompson',
        experiment: a.experiment.trim(),
        metric: a.metric.trim(),
        children: t.map((x, i) => ({
          id: x.role.trim() || `arm-${i + 1}`,
          deployment: x.deployment,
          weight: 1,
        })),
        ...(a.minSamples.trim() && samples !== 10 ? { min_samples_per_arm: samples } : {}),
        ...(a.stop ? { stop_after: { delta: ratio(a.delta) } } : {}),
      });
    }
    case 'shadow': {
      const primary = t[0];
      return withPolicies({
        ...(primary?.role.trim() ? { id: primary.role.trim() } : {}),
        type: 'deployment',
        deployment: primary?.deployment ?? '',
        mirror: {
          target: draft.shadow.target,
          fraction: ratio(draft.shadow.percent),
          ...(draft.shadow.sampleBy === 'session' ? { sample_by: 'session' } : {}),
        },
      });
    }
    case 'tier':
      return withPolicies({
        type: 'tier',
        tier: draft.tier.tier,
        ...(draft.tier.strategy !== 'least_cost' ? { strategy: draft.tier.strategy } : {}),
        ...(draft.tier.includePreview ? { include_preview: true } : {}),
      });
    case 'pool': {
      const o = isRecord(draft.original) && strategyOf(draft.original) === 'pool' ? draft.original : {};
      const scheduler: Record<string, Json> = { ...(isRecord(o.scheduler) ? o.scheduler : {}) };
      if (draft.pool.lora) scheduler.lora = true;
      else Reflect.deleteProperty(scheduler, 'lora');
      const p = draft.pool;
      return withPolicies({
        type: 'pool',
        children: t.map((x) => child(x, false)),
        ...(Object.keys(scheduler).length > 0 ? { scheduler } : {}),
        ...(p.picker === 'external'
          ? {
              picker: 'external',
              picker_service: {
                url: p.url.trim(),
                ...(p.timeout.trim() ? { timeout: p.timeout.trim() } : {}),
              },
              ...(p.onError === 'builtin' ? { picker_on_error: 'builtin' } : {}),
            }
          : {}),
      });
    }
    case 'cost_quality': {
      const c = draft.costQuality;
      const o =
        isRecord(draft.original) && strategyOf(draft.original) === 'cost_quality' ? draft.original : {};
      const scorer =
        c.scorer === 'kept' && isRecord(o.scorer)
          ? o.scorer
          : {
              kind: 'matrix_factorization',
              model_file: c.modelFile.trim(),
              embedding_model: c.embeddingModel.trim(),
              models: { strong: c.strongName.trim(), weak: c.weakName.trim() },
            };
      return withPolicies({
        type: 'cost_quality',
        strong: c.strong,
        weak: c.weak,
        [c.by === 'fraction' ? 'strong_fraction' : 'threshold']: ratio(c.percent),
        scorer,
        ...(c.onError !== 'strong' ? { on_error: c.onError } : {}),
      });
    }
    case 'block':
      return {
        ...extra,
        type: 'block',
        status: draft.block.status,
        message: draft.block.message,
        category: draft.block.category,
      };
  }
}

/** The rule object written into the conditional node. */
export function toRule(draft: RuleDraft, keepWhen?: Json): Record<string, Json> {
  const when = keepWhen !== undefined ? keepWhen : toPredicate(draft);
  return {
    name: draft.name.trim(),
    ...(draft.description.trim() ? { description: draft.description.trim() } : {}),
    ...(draft.tags.length > 0 ? { tags: draft.tags } : {}),
    ...(draft.enabled ? {} : { enabled: false }),
    ...(when === undefined ? {} : { when }),
    target: toTarget(draft),
  };
}

/**
 * The draft in plain language, for the wizard's live preview: only the parts filled in; a sentence
 * still missing its last part ends with the ellipsis alone, not "…." (a dot after it).
 */
export function describeDraft(d: RuleDraft): string {
  return draftSentence(d).replace(/…\.$/, '…');
}

function draftSentence(d: RuleDraft): string {
  const filled = {
    ...d,
    conditions: d.conditions.filter(
      (c) =>
        c.value.trim() !== '' && ((c.kind !== 'metadata' && c.kind !== 'guardrail') || c.key.trim() !== ''),
    ),
  };
  const predicate = toPredicate(filled);
  const when = predicate !== undefined ? `When ${describePredicate(predicate)}` : 'For every request';
  const names = d.targets.filter((t) => t.deployment).map((t) => t.deployment);
  const list = (sep: string) => names.join(sep) || '…';
  switch (d.strategy) {
    case 'single':
      return `${when}, send the request to ${names[0] ?? '…'}.`;
    case 'weighted':
      return `${when}, split traffic: ${
        d.targets
          .filter((t) => t.deployment)
          .map((t) => `${t.deployment} ${t.weight}`)
          .join(', ') || '…'
      }${stickyBy(d) ? `, sticky by ${stickyLabel(stickyBy(d) ?? '')}` : ''}${extrasText(d)}.`;
    case 'fallback':
      return `${when}, try ${list(', then ')}${extrasText(d)}.`;
    case 'least_busy':
      return `${when}, route to whichever of ${list(' / ')} has the fewest requests in flight${extrasText(d)}.`;
    case 'least_latency':
      return `${when}, route to whichever of ${list(' / ')} answers fastest${extrasText(d)}.`;
    case 'canary': {
      const steps = d.canary.steps.filter((s) => s.weight.trim());
      const ramp = steps.map((s) => `${s.weight}%`).join(' → ');
      return `${when}, send ${names[1] ?? '…'} ${ramp || '…'} of traffic and the rest to ${names[0] ?? '…'}${
        d.canary.budget
          ? `; ${d.canary.action === 'hold' ? 'hold the ramp' : 'roll back'} if its errors pass ${d.canary.maxErrorRate || '…'}%`
          : ''
      }.`;
    }
    case 'adaptive':
      return `${when}, learn from ${d.adaptive.metric.trim() || '…'} feedback which of ${list(' / ')} answers best and send it more traffic${
        d.adaptive.stop ? ', then all of it once one wins' : ''
      }.`;
    case 'shadow':
      return `${when}, answer from ${names[0] ?? '…'} and copy ${d.shadow.percent || '…'}% of requests to ${
        d.shadow.target || '…'
      } to compare.`;
    case 'tier':
      return `${when}, send the request to the ${
        d.tier.strategy === 'least_cost'
          ? 'cheapest'
          : d.tier.strategy === 'least_latency'
            ? 'fastest'
            : d.tier.strategy === 'fallback'
              ? 'first available'
              : 'next'
      } model of the ${d.tier.tier || '…'} tier.`;
    case 'pool':
      return `${when}, send the request to the replica of ${list(' / ')} ${
        d.pool.picker === 'external'
          ? `the endpoint picker at ${d.pool.url.trim() || '…'} chooses`
          : 'with the shortest queue and the most of the prompt cached'
      }${d.pool.lora ? ', preferring replicas that run its adapter' : ''}${extrasText(d)}.`;
    case 'cost_quality': {
      const c = d.costQuality;
      return `${when}, send ${
        c.by === 'fraction'
          ? `about ${c.percent || '…'}% of prompts, the ones the scorer rates as needing it most,`
          : `prompts the scorer rates at ${c.percent || '…'}% or more`
      } to ${c.strong || '…'} and the rest to ${c.weak || '…'}.`;
    }
    case 'block':
      return `${when}, block the request.`;
  }
}

/** Hedging and response checks in words, for the preview: ", hedged after 2s". */
function extrasText(d: RuleDraft): string {
  const parts: string[] = [];
  if (d.hedging.enabled) parts.push(`hedged after ${d.hedging.delay.trim() || '2s'}`);
  const checks = d.predicates.filter(predicateComplete);
  if (checks.length > 0) parts.push(`moving on when an answer has ${checks.map(predicateText).join(' or ')}`);
  return parts.length > 0 ? `, ${parts.join(', ')}` : '';
}
