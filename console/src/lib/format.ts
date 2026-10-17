/**
 * Every number the console shows goes through these helpers, so money, latency, tokens and
 * counts read the same on every screen.
 */

const EMPTY = '—';

type Numeric = number | string | null | undefined;

function toNumber(value: Numeric): number | null {
  if (value === null || value === undefined || value === '') return null;
  const n = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(n) ? n : null;
}

/** `toFixed` that never reads "-0": a value that rounds to zero is written without a sign. */
function fixed(n: number, digits: number): string {
  const text = n.toFixed(digits);
  return /^-0(\.0*)?$/.test(text) ? text.slice(1) : text;
}

/**
 * A small positive amount with `digits` significant digits and no trailing zeros, never in
 * exponent form: 0.00015 → "0.00015", 0.0347 → "0.0347". Money under a dime keeps what matters.
 */
function significant(n: number, digits: number): string {
  const decimals = Math.min(10, Math.max(0, digits - 1 - Math.floor(Math.log10(n))));
  const text = n.toFixed(decimals);
  return text.includes('.') ? text.replace(/0+$/, '').replace(/\.$/, '') : text;
}

/** Whether a formatted number shows anything but zeros. */
function nonZero(text: string): boolean {
  return /[1-9]/.test(text);
}

/** A number with fixed decimals ("0.4"), never "-0.0". */
export function formatFixed(value: Numeric, digits = 1): string {
  const n = toNumber(value);
  return n === null ? EMPTY : fixed(n, digits);
}

/**
 * A change with its sign: "+3.2", "−0.4", and "0.0" without a sign when it rounds to zero. With
 * `percent`, the value is a ratio shown as a percentage ("+12%").
 */
export function formatSigned(value: Numeric, digits = 1, options: { percent?: boolean } = {}): string {
  const n = toNumber(value);
  if (n === null) return EMPTY;
  const body = fixed(Math.abs(options.percent ? n * 100 : n), digits);
  const sign = !nonZero(body) ? '' : n < 0 ? '−' : '+';
  return `${sign}${body}${options.percent ? '%' : ''}`;
}

/**
 * Events per minute: "12/min", "0.4/min". With `taken`, what a stage removes from a flow, as
 * "−0.4/min", and "0.0/min" when nothing is.
 */
export function formatPerMinute(value: Numeric, options: { taken?: boolean } = {}): string {
  const n = toNumber(value);
  if (n === null) return EMPTY;
  const body = Math.abs(n) >= 10 ? formatCount(n) : fixed(n, 1);
  return `${options.taken && nonZero(body) ? '−' : ''}${body}/min`;
}

const SUFFIXES: [number, string][] = [
  [1e12, 'T'],
  [1e9, 'B'],
  [1e6, 'M'],
  [1e3, 'K'],
];

/** 1234 → "1.2K"; values under 1,000 keep their digits. */
export function compact(value: number, digits = 1): string {
  const abs = Math.abs(value);
  for (const [size, suffix] of SUFFIXES) {
    if (abs >= size) {
      const scaled = value / size;
      return `${fixed(scaled, Math.abs(scaled) >= 100 ? 0 : digits)}${suffix}`;
    }
  }
  return Number.isInteger(value) ? String(value || 0) : fixed(value, digits);
}

/** Integer counts: "12,345", or "12.3K" when compact. */
export function formatCount(value: Numeric, options: { compact?: boolean } = {}): string {
  const n = toNumber(value);
  if (n === null) return EMPTY;
  if (options.compact && Math.abs(n) >= 10_000) return compact(n);
  return (Math.round(n) || 0).toLocaleString('en-US');
}

/**
 * USD amounts. The Admin API sends money as decimal strings; amounts under a dollar keep four
 * decimals so per-request costs stay readable, larger ones two, and compact mode shortens
 * thousands.
 */
export function formatMoney(value: Numeric, options: { compact?: boolean; sign?: boolean } = {}): string {
  const n = toNumber(value);
  if (n === null) return EMPTY;
  const abs = Math.abs(n);
  let body: string;
  if (options.compact && abs >= 1_000) body = compact(abs);
  else if (abs < 0.000001) body = '0.00';
  else if (abs < 0.1) body = significant(abs, 3);
  else body = abs.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  const sign = !nonZero(body) ? '' : n < 0 ? '−' : options.sign && n > 0 ? '+' : '';
  return `${sign}$${body}`;
}

/** A price per 1M tokens as the API states it: "$2.50 / 1M". */
export function formatPricePerMillion(value: Numeric): string {
  return formatTokenPrice(value, '1M');
}

/** Tokens a price is stated per: the user's price unit. */
export type TokenUnit = '1M' | '1K';

/**
 * A price the API states per 1M tokens, in the user's unit: "$2.50 / 1M" or "$0.0025 / 1K".
 */
export function formatTokenPrice(perMillion: Numeric, unit: TokenUnit): string {
  const n = toNumber(perMillion);
  if (n === null) return EMPTY;
  return `${formatMoney(unit === '1K' ? n / 1000 : n)} / ${unit}`;
}

/** A cost per token as a price in the user's unit, without the unit: for a column headed by it. */
export function formatCostPerTokens(costPerToken: number, unit: TokenUnit): string {
  return formatMoney(costPerToken * (unit === '1K' ? 1000 : 1_000_000));
}

/** Latency in milliseconds: "952 ms", "2.43 s". */
export function formatLatency(ms: Numeric): string {
  const n = toNumber(ms);
  if (n === null) return EMPTY;
  if (n > 0 && n < 1) return '<1 ms';
  if (n < 1000) return `${Math.round(n)} ms`;
  if (n < 60_000) return `${(n / 1000).toFixed(2)} s`;
  return formatDuration(n / 1000);
}

/** Token counts: "950", "18.1K", "4.79M". */
export function formatTokens(value: Numeric): string {
  const n = toNumber(value);
  if (n === null) return EMPTY;
  if (Math.abs(n) < 1000) return String(Math.round(n) || 0);
  const abs = Math.abs(n);
  if (abs >= 1e6) return `${fixed(n / 1e6, abs >= 1e8 ? 0 : 2)}M`;
  return `${fixed(n / 1e3, 1)}K`;
}

/** A ratio (0.123) as a percentage ("12.3%"), without trailing zeros ("30%", "99.95%"). */
export function formatPercent(ratio: Numeric, digits = 1): string {
  const n = toNumber(ratio);
  if (n === null) return EMPTY;
  const text = fixed(n * 100, digits);
  return `${text.includes('.') ? text.replace(/0+$/, '').replace(/\.$/, '') : text}%`;
}

/** A change against the previous window, e.g. "+3.2%" or "−0.4pp". */
export function formatDelta(
  current: Numeric,
  previous: Numeric,
  mode: 'relative' | 'points' = 'relative',
): string {
  const c = toNumber(current);
  const p = toNumber(previous);
  if (c === null || p === null) return EMPTY;
  if (p === 0 && mode === 'relative') return c === 0 ? NO_CHANGE : 'new';
  const d = mode === 'points' ? (c - p) * 100 : ((c - p) / Math.abs(p)) * 100;
  const signed = formatSigned(d, 1);
  if (!nonZero(signed)) return NO_CHANGE;
  return `${signed}${mode === 'points' ? 'pp' : '%'}`;
}

/** A delta that rounds to nothing. */
export const NO_CHANGE = 'no change';

/** Seconds as "4h 12m", "3m 05s", "12d 4h". */
export function formatDuration(seconds: Numeric): string {
  const n = toNumber(seconds);
  if (n === null) return EMPTY;
  const s = Math.max(0, Math.round(n));
  const d = Math.floor(s / 86_400);
  const h = Math.floor((s % 86_400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (d > 0) return h > 0 ? `${d}d ${h}h` : `${d}d`;
  if (h > 0) return `${h}h ${String(m).padStart(2, '0')}m`;
  if (m > 0) return `${m}m ${String(sec).padStart(2, '0')}s`;
  return `${sec}s`;
}

/** Bytes as "48.2 GB". */
export function formatBytes(value: Numeric): string {
  const n = toNumber(value);
  if (n === null) return EMPTY;
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n;
  let i = 0;
  while (Math.abs(v) >= 1000 && i < units.length - 1) {
    v /= 1000;
    i += 1;
  }
  return `${i === 0 ? Math.round(v) || 0 : fixed(v, 1)} ${units[i] ?? 'B'}`;
}

/** A Duration string of the config (`90d`, `1h30m`, `500ms`) in seconds. */
export function parseDuration(value: string): number | null {
  const re = /(\d+(?:\.\d+)?)(ms|s|m|h|d|w)/g;
  const scale: Record<string, number> = { ms: 0.001, s: 1, m: 60, h: 3600, d: 86_400, w: 604_800 };
  let total = 0;
  let matched = '';
  for (const match of value.matchAll(re)) {
    const [whole, amount, unit] = match;
    if (amount === undefined || unit === undefined) return null;
    total += Number(amount) * (scale[unit] ?? 0);
    matched += whole;
  }
  return matched.length > 0 && matched === value ? total : null;
}

/** A display name's initials: "Priya Okonkwo" → "PO". */
export function initials(name: string): string {
  const parts = name
    .trim()
    .split(/[\s._@-]+/)
    .filter(Boolean);
  const letters = parts.length >= 2 ? [parts[0], parts[parts.length - 1]] : [parts[0]];
  return letters
    .map((p) => (p ?? '').charAt(0))
    .join('')
    .slice(0, 2)
    .toUpperCase();
}

/** A stable hue for a name, for avatars and team colours without a colour of their own. */
export function hueOf(name: string): number {
  let h = 0;
  for (const ch of name) h = (h * 31 + ch.charCodeAt(0)) % 360;
  return h;
}

/** Shortens a long id for a table cell: "req_4e7b596930…". */
export function shortId(id: string, keep = 14): string {
  return id.length > keep ? `${id.slice(0, keep)}…` : id;
}

/** "service" → "Service". */
export function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * A count with its noun in the right number: plural(1, 'provider') → "1 provider",
 * plural(3, 'policy', 'policies') → "3 policies". The count goes through `formatCount`.
 */
export function plural(count: number, singular: string, pluralForm = `${singular}s`): string {
  return `${formatCount(count)} ${count === 1 ? singular : pluralForm}`;
}

/** The decimals a step needs to be read exactly ("0.0025" → 4), at most 6. */
function stepDecimals(step: number): number {
  if (!(step > 0)) return 0;
  const text = step.toFixed(6).replace(/0+$/, '');
  const dot = text.indexOf('.');
  return dot < 0 ? 0 : text.length - dot - 1;
}

/** An axis tick in dollars, with the decimals its step needs and no more: "$0", "$0.002", "$1.5". */
export function formatMoneyTick(value: number, step: number): string {
  if (value === 0) return '$0';
  const digits = Math.min(6, stepDecimals(step));
  return `$${value.toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

/** An axis tick of latency: "0 ms", "250 ms", "1.5 s" (no trailing zeros). */
export function formatLatencyTick(ms: number): string {
  if (ms === 0) return '0 ms';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${String(Number((ms / 1000).toFixed(2)))} s`;
  return formatDuration(ms / 1000);
}
