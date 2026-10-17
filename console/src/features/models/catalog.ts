import type { components } from '@/lib/api/schema';
import type { UsageBucket } from '@/lib/api/types';
import { formatCount, type TokenUnit } from '@/lib/format';

import type { Modality } from './types';

type S = components['schemas'];
export type DeploymentCapabilities = S['DeploymentCapabilities'];
export type CatalogStatus = S['CatalogStatus'];

/**
 * What the catalog tile says about the model and price catalog: when the runtime catalog last
 * changed, or, until one is set, the version of the prices this build ships.
 */
export function catalogSync(status: CatalogStatus): { text: string; detail: string; at: string | null } {
  const rt = status.runtime;
  if (rt) {
    const what = rt.mode === 'replace' ? 'replaces the bundled one' : 'extends the bundled one';
    const version = rt.version ? ` ${rt.version}` : '';
    return {
      text: `Catalog${version}`,
      detail: `Runtime catalog${version}: ${rt.entries} ${rt.entries === 1 ? 'entry' : 'entries'}, ${what}. Bundled prices ${status.bundled.price_version}.`,
      at: rt.updated_at,
    };
  }
  return {
    text: `Prices ${status.bundled.price_version}`,
    detail: `The bundled catalog of this build: ${status.bundled.entries} entries, prices version ${status.bundled.price_version}. Set a runtime catalog to update it without a release.`,
    at: null,
  };
}
export type AccessRequest = S['AccessRequest'];

type Caps = DeploymentCapabilities['capabilities'];

/** A capability's value when something more specific than the provider type's default states it. */
export function stated(caps: Caps | undefined, name: string): unknown {
  const entry = caps?.[name];
  if (!entry || entry.source === 'type_default') return undefined;
  return entry.value;
}

/** A capability's value from any source, the type's default included. */
export function capability(caps: Caps | undefined, name: string): unknown {
  return caps?.[name]?.value;
}

const BY_NAME: [RegExp, Modality][] = [
  [/rerank/i, 'rerank'],
  [/embed/i, 'embeddings'],
  [/whisper|transcribe|tts|speech|audio/i, 'audio'],
  [/dall-e|image|imagen|flux|stable-diffusion/i, 'image'],
];

/**
 * A leaf's modality: its operations when the deployment, provider or catalog states them (a
 * provider type's default lists every operation it can carry, which says nothing about the model);
 * otherwise from the upstream model id; otherwise chat.
 */
export function modalityFor(caps: Caps | undefined, upstream: string | undefined): Modality {
  const ops = stated(caps, 'operations');
  if (Array.isArray(ops) && ops.length > 0) {
    if (ops.includes('generate')) return 'chat';
    if (ops.includes('embed')) return 'embeddings';
    if (ops.includes('rerank')) return 'rerank';
    if (ops.includes('image_generate')) return 'image';
    if (ops.includes('transcribe') || ops.includes('speech')) return 'audio';
    return 'other';
  }
  for (const [re, m] of BY_NAME) if (upstream && re.test(upstream)) return m;
  return 'chat';
}

export const MODALITY_LABEL: Record<Modality, string> = {
  chat: 'Chat',
  embeddings: 'Embeddings',
  rerank: 'Rerank',
  image: 'Image',
  audio: 'Audio',
  other: 'Other',
};

const TAGS: [string, string][] = [
  ['vision', 'Vision'],
  ['tools', 'Tools'],
  ['json_schema', 'Structured output'],
  ['reasoning', 'Reasoning'],
  ['audio_input', 'Audio input'],
  ['pdf_input', 'PDF input'],
  ['prompt_cache_markers', 'Prompt caching'],
  ['streaming', 'Streaming'],
];

/** The capability tags a leaf states (true and not merely the provider type's default). */
export function capabilityTags(caps: Caps | undefined): string[] {
  return TAGS.filter(([name]) => stated(caps, name) === true).map(([, label]) => label);
}

/** A number of tokens a leaf states (`context_window`, `max_output_tokens`), or null. */
export function tokenLimit(caps: Caps | undefined, name: string): number | null {
  const v = capability(caps, name);
  return typeof v === 'number' && v > 0 ? v : null;
}

/** 128000 → "128K", 1048576 → "1M", 8191 → "8.2K". */
export function formatContext(tokens: number | null): string {
  if (tokens === null) return '—';
  if (tokens >= 1_000_000) return `${+(tokens / 1_000_000).toFixed(tokens % 1_000_000 === 0 ? 0 : 1)}M`;
  if (tokens >= 1_000) {
    const k = tokens / 1000;
    return `${Number.isInteger(k) || k >= 100 ? Math.round(k) : +k.toFixed(1)}K`;
  }
  return String(tokens);
}

/** One line of a price grid: a per-token rate (in the reader's unit) or a flat charge. */
export interface PriceLine {
  field: string;
  label: string;
  /** Per-token rates are converted to the reader's unit; flat ones are shown as they are. */
  perToken: boolean;
  /** For a flat charge: what it is per ("request", "image"). */
  per?: string;
}

const TOKEN_LINES: PriceLine[] = [
  { field: 'input', label: 'Input', perToken: true },
  { field: 'output', label: 'Output', perToken: true },
  { field: 'cache_read', label: 'Cached input', perToken: true },
  { field: 'cache_write_5m', label: 'Cache write, 5 min', perToken: true },
  { field: 'cache_write_1h', label: 'Cache write, 1 h', perToken: true },
  { field: 'reasoning', label: 'Reasoning output', perToken: true },
];

const FLAT_LINES: PriceLine[] = [
  { field: 'per_request', label: 'Per request', perToken: false, per: 'request' },
  { field: 'per_page', label: 'Per page', perToken: false, per: 'page' },
  { field: 'per_audio_second_in', label: 'Audio in', perToken: false, per: 'second' },
  { field: 'per_1k_characters_in', label: 'Text in', perToken: false, per: '1K characters' },
  { field: 'per_web_search', label: 'Web search', perToken: false, per: 'search' },
];

/**
 * The price lines a Model shows for its modality: input only for embeddings, the flat charge for
 * rerank (per request, as the gateway bills a search), and every rate the price states otherwise.
 */
export function priceLines(
  price: Record<string, unknown> | null | undefined,
  modality: Modality,
): PriceLine[] {
  const has = (f: string) => price?.[f] !== undefined && price[f] !== null;
  if (modality === 'embeddings') return TOKEN_LINES.filter((l) => l.field === 'input');
  if (modality === 'rerank') {
    const flat = FLAT_LINES.filter((l) => has(l.field));
    return flat.length > 0 ? flat : FLAT_LINES.slice(0, 1);
  }
  const lines = TOKEN_LINES.filter((l) => l.field === 'input' || l.field === 'output' || has(l.field));
  return [...lines, ...FLAT_LINES.filter((l) => has(l.field))];
}

/** "2.5" per 1M in the reader's unit, as a plain decimal for a field ("0.0025" per 1K). */
export function fromPerMillion(value: string | null | undefined, unit: TokenUnit): string {
  if (value === null || value === undefined || value === '') return '';
  return unit === '1M' ? value : shiftDecimal(value, -3);
}

/** A price typed in the reader's unit, as the API's decimal per 1M tokens ("0.0025" per 1K → "2.5"). */
export function toPerMillion(value: string, unit: TokenUnit): string {
  const v = value.trim();
  if (v === '' || unit === '1M') return v;
  return shiftDecimal(v, 3);
}

/** Moves a non-negative decimal string's point by `places` (exact, no floating point). */
export function shiftDecimal(value: string, places: number): string {
  const m = /^(\d*)(?:\.(\d*))?$/.exec(value.trim());
  if (!m) return value;
  const int = m[1] ?? '';
  const frac = m[2] ?? '';
  let digits = int + frac;
  let point = int.length + places;
  if (point < 0) {
    digits = '0'.repeat(-point) + digits;
    point = 0;
  }
  if (point > digits.length) digits = digits + '0'.repeat(point - digits.length);
  const whole = digits.slice(0, point).replace(/^0+(?=\d)/, '') || '0';
  const rest = digits.slice(point).replace(/0+$/, '');
  return rest ? `${whole}.${rest}` : whole;
}

/** Totals of usage buckets, with request-weighted means of the per-bucket percentiles. */
export interface UsageTotals {
  requests: number;
  errors: number;
  cost: number;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cancelled: number;
  cacheHits: number;
  fallbacks: number;
  latencyP50: number | null;
  latencyP95: number | null;
  latencyP99: number | null;
  ttftP50: number | null;
  ttftP95: number | null;
  tokensPerSecond: number | null;
}

/** Above this, an output speed is a measuring artefact (a stream cut short), not a speed. */
export const MAX_TOKENS_PER_SECOND = 20_000;

/**
 * The output speed as shown, "1,234" (tok/s), or null when nothing streamed or the reading is not
 * a plausible speed.
 */
export function outputSpeed(tokensPerSecond: number | null | undefined): string | null {
  if (tokensPerSecond == null || !Number.isFinite(tokensPerSecond) || tokensPerSecond <= 0) return null;
  if (tokensPerSecond > MAX_TOKENS_PER_SECOND) return null;
  return formatCount(Math.round(tokensPerSecond));
}

function weighted(
  buckets: UsageBucket[],
  pick: (b: UsageBucket) => number | null | undefined,
): number | null {
  let sum = 0;
  let weight = 0;
  for (const b of buckets) {
    const v = pick(b);
    if (v === null || v === undefined || !Number.isFinite(v) || b.requests <= 0) continue;
    sum += v * b.requests;
    weight += b.requests;
  }
  return weight > 0 ? sum / weight : null;
}

export function sumBuckets(buckets: UsageBucket[]): UsageTotals {
  const t: UsageTotals = {
    requests: 0,
    errors: 0,
    cost: 0,
    input: 0,
    output: 0,
    cacheRead: 0,
    cacheWrite: 0,
    cancelled: 0,
    cacheHits: 0,
    fallbacks: 0,
    latencyP50: weighted(buckets, (b) => b.latency_ms?.p50),
    latencyP95: weighted(buckets, (b) => b.latency_ms?.p95),
    latencyP99: weighted(buckets, (b) => b.latency_ms?.p99),
    ttftP50: weighted(buckets, (b) => b.ttft_ms?.p50),
    ttftP95: weighted(buckets, (b) => b.ttft_ms?.p95),
    tokensPerSecond: weighted(buckets, (b) => b.output_tokens_per_second),
  };
  for (const b of buckets) {
    t.requests += b.requests;
    t.errors += b.errors;
    t.cost += Number(b.cost_usd) || 0;
    t.input += b.input ?? 0;
    t.output += b.output ?? 0;
    t.cacheRead += b.input_cache_read ?? 0;
    t.cacheWrite += b.input_cache_write ?? 0;
    t.cancelled += b.cancelled ?? 0;
    t.cacheHits += b.cache_hits ?? 0;
    t.fallbacks += b.fallbacks ?? 0;
  }
  return t;
}

/** Usage buckets summed per value of one group key ("team"), largest first by requests. */
export function byGroup(buckets: UsageBucket[], key: string): { name: string; totals: UsageTotals }[] {
  const groups = new Map<string, UsageBucket[]>();
  for (const b of buckets) {
    const name = b.group[key];
    if (!name) continue;
    groups.set(name, [...(groups.get(name) ?? []), b]);
  }
  return [...groups.entries()]
    .map(([name, list]) => ({ name, totals: sumBuckets(list) }))
    .sort((a, b) => b.totals.requests - a.totals.requests);
}

/** The relative change from `previous` to `current`, or null without a base. */
export function change(current: number, previous: number | null | undefined): number | null {
  if (previous === null || previous === undefined || previous <= 0) return null;
  return (current - previous) / previous;
}

/** The Model with the largest spend and its share of all spend. */
export function leader(usage: Map<string, { cost: number }>): { name: string; share: number } | null {
  let total = 0;
  let best: { name: string; cost: number } | null = null;
  for (const [name, u] of usage) {
    total += u.cost;
    if (!best || u.cost > best.cost) best = { name, cost: u.cost };
  }
  return best && total > 0 && best.cost > 0 ? { name: best.name, share: best.cost / total } : null;
}

/**
 * The Model with the highest error rate among those with at least `minRequests` requests (a Model
 * with a handful of requests would otherwise lead on one failure).
 */
export function worstErrorRate(
  usage: Map<string, { requests: number; errors: number }>,
  minRequests = 20,
): { name: string; rate: number } | null {
  let worst: { name: string; rate: number } | null = null;
  for (const [name, u] of usage) {
    if (u.requests < minRequests) continue;
    const rate = u.errors / u.requests;
    if (!worst || rate > worst.rate) worst = { name, rate };
  }
  return worst;
}

/**
 * The rule a served leaf's route path went through: `r<i>` after the route name (and after the
 * root's id when it has one), or `default`. Null when the path is of another route.
 */
export function ruleOfPath(path: string, route: string, rootId?: string): number | 'default' | null {
  const segments = path.split('/');
  if (segments[0] !== route) return null;
  const segment = rootId && segments[1] === rootId ? segments[2] : segments[1];
  if (segment === 'default') return 'default';
  const m = /^r(\d+)$/.exec(segment ?? '');
  return m ? Number(m[1]) : null;
}

/** "team/support-ai" → { kind: "team", name: "support-ai" }. */
export function parseScope(
  scope: string,
): { kind: 'organization' | 'team' | 'project' | 'key'; name: string } | null {
  const [kind, ...rest] = scope.split('/');
  const name = rest.join('/');
  if (!name) return null;
  if (kind === 'organization' || kind === 'team' || kind === 'project' || kind === 'key')
    return { kind, name };
  return null;
}

export const SCOPE_NOUN: Record<'organization' | 'team' | 'project' | 'key', string> = {
  organization: 'Organization',
  team: 'Team',
  project: 'Project',
  key: 'Key',
};
