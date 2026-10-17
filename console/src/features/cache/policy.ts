import type { ModelResource, RouteResource, UsageBucket } from '@/lib/api/types';

import type { EffectivePrice, PolicyCache } from './api';

/** A Model's cache policy, or an empty one. */
export function cacheOf(m: ModelResource): PolicyCache {
  return m.spec.policies?.cache ?? {};
}

/** The three ways a Model can avoid paying for a request again; they are independent switches. */
export interface Modes {
  /** The gateway's response cache (`enabled`). */
  exact: boolean;
  /** Similar prompts also match (`semantic.enabled`, on top of the response cache). */
  semantic: boolean;
  /** Cache breakpoints the provider reads (`prompt_caching.auto_inject`). */
  prompt: boolean;
}

export function modesOf(c: PolicyCache): Modes {
  return {
    exact: c.enabled === true,
    semantic: c.enabled === true && c.semantic?.enabled === true,
    prompt: c.prompt_caching?.auto_inject === true,
  };
}

export const TTLS = [
  { value: '1m', label: '1 min' },
  { value: '5m', label: '5 min' },
  { value: '15m', label: '15 min' },
  { value: '1h', label: '1 h' },
  { value: '6h', label: '6 h' },
  { value: '1d', label: '1 day' },
];

/** "1h" → "1 h", "15m" → "15 min", "1d" → "1 day"; other values as written. */
export function duration(value: string | undefined | null): string {
  if (!value) return '—';
  const known = TTLS.find((t) => t.value === value)?.label;
  if (known) return known;
  const m = /^(\d+)(s|m|h|d|w)$/.exec(value);
  if (!m) return value;
  const n = Number(m[1]);
  const unit = { s: 's', m: 'min', h: 'h', d: n === 1 ? 'day' : 'days', w: n === 1 ? 'week' : 'weeks' }[
    m[2] as 's' | 'm' | 'h' | 'd' | 'w'
  ];
  return `${n} ${unit}`;
}

/** "1MiB" → "1 MiB". */
export function size(value: string | undefined): string {
  return (value ?? '1MiB').replace(/^(\d+)/, '$1 ');
}

/**
 * Key strategies: presets over `key.ignore_fields`. Each removes only members that never
 * change the generated answer; anything that does is refused by `ignoreFieldError`.
 */
export const KEY_PRESETS = [
  {
    id: 'full',
    label: 'Every field that changes the answer',
    short: 'Full request',
    description: 'The default. Requests share a response only when everything that shapes it is the same.',
    fields: [] as string[],
  },
  {
    id: 'tracing',
    label: 'Also ignore client tracing ids',
    short: 'Ignore tracing ids',
    description: 'Request, trace and session ids some clients add to the body don’t split the cache.',
    fields: ['/request_id', '/trace_id', '/session_id', '/tags'],
  },
  {
    id: 'prediction',
    label: 'Also ignore predicted output',
    short: 'Ignore prediction',
    description: 'A predicted output speeds up the answer but doesn’t change it.',
    fields: ['/prediction'],
  },
] as const;

export type KeyPresetId = (typeof KEY_PRESETS)[number]['id'] | 'custom';

export function keyStrategyOf(c: PolicyCache): { id: KeyPresetId; label: string; fields: string[] } {
  const fields = c.key?.ignore_fields ?? [];
  const sorted = [...fields].sort().join('\n');
  const preset = KEY_PRESETS.find((p) => [...p.fields].sort().join('\n') === sorted);
  if (preset) return { id: preset.id, label: preset.short, fields };
  return { id: 'custom', label: `Custom (${fields.length} ignored)`, fields };
}

/** Body members that change the answer, and the prompt itself. */
const OUTPUT_FIELDS = [
  'messages',
  'system',
  'input',
  'prompt',
  'instructions',
  'contents',
  'tools',
  'tool_choice',
  'temperature',
  'top_p',
  'top_k',
  'max_tokens',
  'max_completion_tokens',
  'max_output_tokens',
  'seed',
  'stop',
  'n',
  'response_format',
  'text',
  'reasoning',
  'reasoning_effort',
  'thinking',
  'logprobs',
  'top_logprobs',
  'logit_bias',
  'frequency_penalty',
  'presence_penalty',
  'parallel_tool_calls',
  'documents',
  'query',
  'dimensions',
  'encoding_format',
];

/** Why a JSON Pointer can't be ignored in the key, or null when it can. */
export function ignoreFieldError(pointer: string): string | null {
  if (!/^(\/([^~/]|~[01])*)+$/.test(pointer)) return 'Write a JSON Pointer, such as /request_id';
  const first = pointer.split('/')[1] ?? '';
  if (first === 'model') return 'The model is always part of the key';
  if (OUTPUT_FIELDS.includes(first)) return `${first} changes the answer, so it stays in the key`;
  return null;
}

/** Every leaf deployment of a route tree or node (string values of `deployment` members). */
function deploymentsIn(node: unknown, out: Set<string>): void {
  if (Array.isArray(node)) {
    for (const n of node) deploymentsIn(n, out);
    return;
  }
  if (typeof node === 'string') return;
  if (!node || typeof node !== 'object') return;
  for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
    if (k === 'deployment' && typeof v === 'string') out.add(v);
    else deploymentsIn(v, out);
  }
}

/** The deployments a Model can send to: its target's leaves, or its shared route's. */
export function deploymentsOf(m: ModelResource, routes: Map<string, RouteResource>): string[] {
  const out = new Set<string>();
  if (m.spec.route) deploymentsIn(routes.get(m.spec.route)?.spec.tree, out);
  deploymentsIn(m.spec.target, out);
  return [...out];
}

function rate(price: EffectivePrice['price'] | undefined, field: string): number | null {
  const v = price?.[field];
  const n = typeof v === 'string' || typeof v === 'number' ? Number(v) : NaN;
  return Number.isFinite(n) ? n : null;
}

/**
 * The share of the input price a cached read saves on each deployment (`1 - cache_read / input`),
 * as the lowest and highest over the Model's deployments; null when no deployment prices both.
 */
export function cachedReadDiscount(
  deployments: string[],
  prices: Map<string, EffectivePrice>,
): { min: number; max: number } | null {
  const ds = deployments
    .map((d) => {
      const p = prices.get(d)?.price;
      const input = rate(p, 'input');
      const read = rate(p, 'cache_read');
      return input && input > 0 && read !== null ? 1 - read / input : null;
    })
    .filter((d): d is number => d !== null);
  if (ds.length === 0) return null;
  return { min: Math.min(...ds), max: Math.max(...ds) };
}

export interface ModelUsage {
  requests: number;
  hits: number;
  saved: number;
  cost: number;
}

/** Per-Model totals of grouped usage buckets. */
export function usageByModel(buckets: UsageBucket[]): Map<string, ModelUsage> {
  const map = new Map<string, ModelUsage>();
  for (const b of buckets) {
    const m = b.group.model;
    if (!m) continue;
    const prev = map.get(m) ?? { requests: 0, hits: 0, saved: 0, cost: 0 };
    map.set(m, {
      requests: prev.requests + b.requests,
      hits: prev.hits + (b.cache_hits ?? 0),
      saved: prev.saved + Number(b.cache_saved_usd ?? 0),
      cost: prev.cost + Number(b.cost_usd),
    });
  }
  return map;
}

export interface PrefixRow {
  prefix: string;
  deployments: string[];
  requests: number;
  read: number;
  write: number;
  /** Cached-read tokens priced at the input rate less the cached-read rate; null when unpriced. */
  saved: number | null;
}

/**
 * Prompt-cache use per cached prefix (usage grouped by `cache_prefix` and `deployment`): requests,
 * cached read and write tokens, and what the cached reads saved against full input prices.
 */
export function prefixRows(buckets: UsageBucket[], prices: Map<string, EffectivePrice>): PrefixRow[] {
  const rows = new Map<string, PrefixRow>();
  for (const b of buckets) {
    const prefix = b.group.cache_prefix;
    if (!prefix) continue;
    const d = b.group.deployment ?? null;
    const row = rows.get(prefix) ?? { prefix, deployments: [], requests: 0, read: 0, write: 0, saved: 0 };
    row.requests += b.requests;
    const read = b.input_cache_read ?? 0;
    row.read += read;
    row.write += b.input_cache_write ?? 0;
    if (d && !row.deployments.includes(d)) row.deployments.push(d);
    const p = d ? prices.get(d)?.price : undefined;
    const input = rate(p, 'input');
    const cached = rate(p, 'cache_read');
    if (read > 0) {
      if (input === null || cached === null || row.saved === null) row.saved = null;
      else row.saved += (read * Math.max(0, input - cached)) / 1_000_000;
    }
    rows.set(prefix, row);
  }
  return [...rows.values()].sort((a, b) => b.read - a.read || b.requests - a.requests);
}

/** The cache settings a level writes, in words, for the overrides table. */
export function describeCache(c: PolicyCache): string[] {
  const out: string[] = [];
  if (c.enabled !== undefined) out.push(c.enabled ? 'Caching on' : 'Caching off');
  if (c.ttl) out.push(`Keep ${duration(c.ttl)}`);
  if (c.max_ttl) out.push(`At most ${duration(c.max_ttl)}`);
  if (c.scope) out.push(`Shared per ${c.scope.replace('_', ' ')}`);
  if (c.read_only !== undefined) out.push(c.read_only ? 'Read only' : 'Writes new responses');
  if (c.max_entries !== undefined)
    out.push(c.max_entries === null ? 'No entry cap' : `Up to ${c.max_entries} responses`);
  if (c.max_entry_size) out.push(`Up to ${size(c.max_entry_size)} each`);
  if (c.semantic?.enabled !== undefined)
    out.push(c.semantic.enabled ? 'Similar prompts match' : 'Exact matches only');
  if (c.semantic?.threshold !== undefined) out.push(`Similarity ${c.semantic.threshold.toFixed(2)}`);
  if (c.prompt_caching?.auto_inject !== undefined)
    out.push(c.prompt_caching.auto_inject ? 'Provider prompt caching on' : 'Provider prompt caching off');
  if (c.activation === 'opt_in') out.push('Only when a request asks');
  if (c.fail_mode === 'closed') out.push('Refuses requests when the cache is down');
  if (c.embeddings?.per_item !== undefined)
    out.push(c.embeddings.per_item ? 'Embeddings cached per item' : 'Embeddings cached per request');
  if (c.key?.ignore_fields !== undefined) out.push(`Key: ${keyStrategyOf(c).label}`);
  if (c.hit_usage === 'zero') out.push('Hits report no usage');
  if (c.locked && c.locked.length > 0) out.push(`Locks ${c.locked.join(', ')}`);
  return out;
}

export type OverrideLevel = 'organization' | 'team' | 'project' | 'key' | 'route';

export interface Override {
  level: OverrideLevel;
  name: string;
  /** The level its unset fields come from: its parent, or the gateway default. */
  inherits: string;
  settings: string[];
}

/** Levels that set `policies.cache`, broadest first, with what they change and inherit. */
export function overridesOf(input: {
  organizations: { name: string; cache: PolicyCache | undefined }[];
  teams: { name: string; organization: string; cache: PolicyCache | undefined }[];
  projects: { name: string; team: string; cache: PolicyCache | undefined }[];
  keys: { name: string; project: string | undefined; cache: PolicyCache | undefined }[];
  routes: { name: string; cache: PolicyCache | undefined }[];
}): Override[] {
  const set = (c: PolicyCache | undefined) => c !== undefined && Object.keys(c).length > 0;
  const orgs = new Set(input.organizations.filter((o) => set(o.cache)).map((o) => o.name));
  const teams = new Map(input.teams.map((t) => [t.name, t]));
  const projects = new Map(input.projects.map((p) => [p.name, p]));
  const teamParent = (team: string): string => {
    const t = teams.get(team);
    if (t && orgs.has(t.organization)) return `organization ${t.organization}`;
    return 'the gateway default';
  };
  const teamSets = (team: string) => set(teams.get(team)?.cache);
  const projectParent = (project: string): string => {
    const p = projects.get(project);
    if (!p) return 'the gateway default';
    return teamSets(p.team) ? `team ${p.team}` : teamParent(p.team);
  };
  const out: Override[] = [];
  for (const o of input.organizations)
    if (set(o.cache))
      out.push({
        level: 'organization',
        name: o.name,
        inherits: 'the gateway default',
        settings: describeCache(o.cache ?? {}),
      });
  for (const t of input.teams)
    if (set(t.cache))
      out.push({
        level: 'team',
        name: t.name,
        inherits: teamParent(t.name),
        settings: describeCache(t.cache ?? {}),
      });
  for (const p of input.projects)
    if (set(p.cache))
      out.push({
        level: 'project',
        name: p.name,
        inherits: teamSets(p.team) ? `team ${p.team}` : teamParent(p.team),
        settings: describeCache(p.cache ?? {}),
      });
  for (const k of input.keys)
    if (set(k.cache)) {
      const parent = k.project
        ? set(projects.get(k.project)?.cache)
          ? `project ${k.project}`
          : projectParent(k.project)
        : 'the gateway default';
      out.push({ level: 'key', name: k.name, inherits: parent, settings: describeCache(k.cache ?? {}) });
    }
  for (const r of input.routes)
    if (set(r.cache))
      out.push({
        level: 'route',
        name: r.name,
        inherits: 'the request’s key and its owners',
        settings: describeCache(r.cache ?? {}),
      });
  return out;
}
