import type { UsageBucket } from '@/lib/api/types';
import { parseDuration } from '@/lib/format';

import type { RuleSpec } from './api';

type Metric = NonNullable<RuleSpec['metric']>;

/** The metrics a preview can compute from usage analytics; the others need live readings. */
const PREVIEWABLE: Record<Metric, boolean> = {
  p95_latency_ms: true,
  p99_latency_ms: true,
  ttft_p95_ms: true,
  error_rate: true,
  upstream_5xx_rate: true,
  fallback_rate: true,
  rpm_utilization: false,
  tpm_utilization: false,
  cost_per_request_usd: true,
  spend_usd: true,
  budget_projection_ratio: false,
  guardrail_events: true,
  cache_hit_rate: true,
  latency_baseline_ratio: true,
  cost_baseline_ratio: true,
  requests: true,
};

export function previewable(metric: Metric | undefined): boolean {
  return metric !== undefined && PREVIEWABLE[metric];
}

export function needsBaseline(metric: Metric | undefined): boolean {
  return metric === 'latency_baseline_ratio' || metric === 'cost_baseline_ratio';
}

export const PREVIEW_DAYS = 7;

/** The analytics bucket a preview reads for a rule window: 5 minutes up to 10, 15 up to 30, else an hour. */
export function previewInterval(window: string | undefined): { interval: '5m' | '15m' | '1h'; ms: number } {
  const seconds = parseDuration(window ?? '5m') ?? 300;
  if (seconds <= 600) return { interval: '5m', ms: 300_000 };
  if (seconds <= 1800) return { interval: '15m', ms: 900_000 };
  return { interval: '1h', ms: 3_600_000 };
}

/** A bucket as the preview reads it: the usage figures and, for the 5xx rate, the 5xx count. */
export interface PreviewBucket {
  start: number;
  requests: number;
  errors: number;
  server_errors: number;
  fallbacks: number;
  cache_hits: number;
  guardrail: number;
  cost: number;
  p95: number | null;
  p99: number | null;
  ttft: number | null;
}

/**
 * Folds usage buckets into one row per bucket start. A read grouped by status class (for the 5xx
 * rate) has one row per class: counts add up, latency percentiles are averaged by requests.
 */
export function foldBuckets(items: UsageBucket[]): PreviewBucket[] {
  const by = new Map<number, PreviewBucket & { w95: number; w99: number; wt: number }>();
  for (const b of items) {
    const start = new Date(b.start).getTime();
    const row = by.get(start) ?? {
      start,
      requests: 0,
      errors: 0,
      server_errors: 0,
      fallbacks: 0,
      cache_hits: 0,
      guardrail: 0,
      cost: 0,
      p95: null,
      p99: null,
      ttft: null,
      w95: 0,
      w99: 0,
      wt: 0,
    };
    const n = b.requests;
    row.requests += n;
    row.errors += b.errors;
    if (b.group.status_class === '5xx') row.server_errors += n;
    row.fallbacks += b.fallbacks ?? 0;
    row.cache_hits += b.cache_hits ?? 0;
    row.guardrail += (b.guardrail_blocks ?? 0) + (b.guardrail_redactions ?? 0) + (b.guardrail_flags ?? 0);
    row.cost += Number(b.cost_usd) || 0;
    const mix = (prev: number | null, w: number, v: number | null | undefined): [number | null, number] =>
      v == null || n === 0 ? [prev, w] : [((prev ?? 0) * w + v * n) / (w + n), w + n];
    [row.p95, row.w95] = mix(row.p95, row.w95, b.latency_ms?.p95);
    [row.p99, row.w99] = mix(row.p99, row.w99, b.latency_ms?.p99);
    [row.ttft, row.wt] = mix(row.ttft, row.wt, b.ttft_ms?.p95);
    by.set(start, row);
  }
  return [...by.values()].sort((a, b) => a.start - b.start);
}

/** The metric over the trailing window ending with bucket `i` (`span` buckets). */
function windowValue(metric: Metric, rows: PreviewBucket[], i: number, span: number): number | null {
  const slice = rows.slice(Math.max(0, i - span + 1), i + 1);
  const sum = (f: (r: PreviewBucket) => number) => slice.reduce((s, r) => s + f(r), 0);
  const requests = sum((r) => r.requests);
  const weighted = (f: (r: PreviewBucket) => number | null) => {
    let w = 0;
    let v = 0;
    for (const r of slice) {
      const x = f(r);
      if (x === null || r.requests === 0) continue;
      v += x * r.requests;
      w += r.requests;
    }
    return w > 0 ? v / w : null;
  };
  const ratio = (n: number) => (requests > 0 ? n / requests : null);
  switch (metric) {
    case 'p95_latency_ms':
    case 'latency_baseline_ratio':
      return weighted((r) => r.p95);
    case 'p99_latency_ms':
      return weighted((r) => r.p99);
    case 'ttft_p95_ms':
      return weighted((r) => r.ttft);
    case 'error_rate':
      return ratio(sum((r) => r.errors));
    case 'upstream_5xx_rate':
      return ratio(sum((r) => r.server_errors));
    case 'fallback_rate':
      return ratio(sum((r) => r.fallbacks));
    case 'cache_hit_rate':
      return ratio(sum((r) => r.cache_hits));
    case 'cost_per_request_usd':
    case 'cost_baseline_ratio':
      return ratio(sum((r) => r.cost));
    case 'spend_usd':
      return sum((r) => r.cost);
    case 'guardrail_events':
      return sum((r) => r.guardrail);
    case 'requests':
      return requests;
    default:
      return null;
  }
}

export interface PreviewResult {
  /** One value per bucket of the preview's days (the metric the rule compares). */
  starts: number[];
  values: (number | null)[];
  /** Bucket indices where the rule would have fired (a new alert). */
  fires: number[];
  /** Buckets where the condition held. */
  held: number;
  /** Buckets with traffic (before a baseline divides it). */
  measured: number;
}

/**
 * Replays the rule over the buckets of the last days: the condition holds in a bucket when the
 * metric over the trailing window compares true; the rule fires once it has held for `for`, unless
 * an alert is still open or the cooldown since the last fire has not passed; an open alert
 * resolves after the condition has been clear for `resolve_after`. Baseline ratios divide by the
 * same metric one week earlier, averaged over that hour.
 */
export function simulate(
  spec: RuleSpec,
  rows: PreviewBucket[],
  intervalMs: number,
  now: number,
): PreviewResult {
  const metric = spec.metric;
  const from = now - PREVIEW_DAYS * 86_400_000;
  const empty: PreviewResult = { starts: [], values: [], fires: [], held: 0, measured: 0 };
  if (!metric || spec.threshold === undefined) return empty;
  const span = Math.max(1, Math.round(((parseDuration(spec.window ?? '5m') ?? 300) * 1000) / intervalMs));
  const steps = (d: string | undefined, fallback: number) =>
    Math.ceil(((parseDuration(d ?? '') ?? fallback) * 1000) / intervalMs);
  const forSteps = steps(spec.for, 0);
  const resolveSteps = steps(spec.resolve_after, 300);
  const cooldownMs = (parseDuration(spec.cooldown ?? '15m') ?? 900) * 1000;
  const raw = rows.map((_, i) => windowValue(metric, rows, i, span));
  const index = new Map(rows.map((r, i) => [r.start, i]));
  const baseline = (i: number): number | null => {
    const start = rows[i]?.start ?? 0;
    const hour = Math.floor((start - 7 * 86_400_000) / 3_600_000) * 3_600_000;
    const vals: number[] = [];
    for (let t = hour; t < hour + 3_600_000; t += intervalMs) {
      const j = index.get(t);
      const v = j === undefined ? null : raw[j];
      if (v !== null && v !== undefined) vals.push(v);
    }
    return vals.length ? vals.reduce((a, b) => a + b, 0) / vals.length : null;
  };
  const ratioMetric = needsBaseline(metric);
  const out: PreviewResult = { starts: [], values: [], fires: [], held: 0, measured: 0 };
  let streak = 0;
  let clear = 0;
  let open = false;
  let lastFire = -Infinity;
  const op = spec.operator ?? 'gt';
  rows.forEach((r, i) => {
    if (r.start < from) return;
    let v = raw[i] ?? null;
    if (v !== null) out.measured += 1;
    if (ratioMetric && v !== null) {
      const b = baseline(i);
      v = b && b > 0 ? v / b : null;
    }
    const t = spec.threshold ?? 0;
    const holds = v !== null && (op === 'gt' ? v > t : op === 'gte' ? v >= t : op === 'lt' ? v < t : v <= t);
    out.starts.push(r.start);
    out.values.push(v);
    if (holds) {
      out.held += 1;
      streak += 1;
      clear = 0;
      if (!open && streak > forSteps - 1 && r.start - lastFire >= cooldownMs) {
        open = true;
        lastFire = r.start;
        out.fires.push(out.values.length - 1);
      }
    } else {
      streak = 0;
      clear += 1;
      if (open && clear >= Math.max(1, resolveSteps)) open = false;
    }
  });
  return out;
}
