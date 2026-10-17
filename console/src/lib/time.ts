/**
 * The console's time model: one range per page, a preset or a custom `[from, to)`, with a bucket
 * size that defaults by range and never yields more than 1,440 buckets.
 */

export const PRESETS = ['15m', '1h', '6h', '24h', '7d', '30d'] as const;
export type Preset = (typeof PRESETS)[number];

/** The intervals the analytics API accepts, in seconds. */
export const INTERVALS = {
  '1m': 60,
  '5m': 300,
  '15m': 900,
  '1h': 3600,
  '6h': 21_600,
  '1d': 86_400,
} as const;
export type Interval = keyof typeof INTERVALS;

const PRESET_SECONDS: Record<Preset, number> = {
  '15m': 900,
  '1h': 3600,
  '6h': 21_600,
  '24h': 86_400,
  '7d': 604_800,
  '30d': 2_592_000,
};

const DEFAULT_BUCKET: Record<Preset, Interval> = {
  '15m': '1m',
  '1h': '1m',
  '6h': '5m',
  '24h': '15m',
  '7d': '1h',
  '30d': '1d',
};

const PRESET_LABEL: Record<Preset, string> = {
  '15m': 'Last 15 minutes',
  '1h': 'Last 1 hour',
  '6h': 'Last 6 hours',
  '24h': 'Last 24 hours',
  '7d': 'Last 7 days',
  '30d': 'Last 30 days',
};

export const MAX_BUCKETS = 1440;

export type RangeValue = { kind: 'preset'; preset: Preset } | { kind: 'custom'; from: number; to: number };

/**
 * The window deltas compare with: the preceding one of equal length (the default), the same one a
 * week or a calendar month earlier, or one of equal length from a chosen start.
 */
export type CompareValue =
  { kind: 'previous' } | { kind: 'week' } | { kind: 'month' } | { kind: 'custom'; from: number };

export const PREVIOUS: CompareValue = { kind: 'previous' };

export interface ResolvedRange {
  from: Date;
  to: Date;
  seconds: number;
  label: string;
  /** The comparison window; `label` follows "vs" ("the previous 6 hours"). */
  previous: { from: Date; to: Date; label: string };
}

export function isPreset(value: string): value is Preset {
  return (PRESETS as readonly string[]).includes(value);
}

export function presetLabel(preset: Preset): string {
  return PRESET_LABEL[preset];
}

export function defaultInterval(range: RangeValue): Interval {
  if (range.kind === 'preset') return DEFAULT_BUCKET[range.preset];
  const seconds = (range.to - range.from) / 1000;
  return smallestInterval(seconds);
}

/** The smallest interval that keeps the range at or under the bucket cap. */
export function smallestInterval(seconds: number): Interval {
  for (const [name, size] of Object.entries(INTERVALS) as [Interval, number][]) {
    if (seconds / size <= MAX_BUCKETS) return name;
  }
  return '1d';
}

/** The intervals allowed for a range: those that give at most 1,440 buckets. */
export function allowedIntervals(seconds: number): Interval[] {
  return (Object.entries(INTERVALS) as [Interval, number][])
    .filter(([, size]) => seconds / size <= MAX_BUCKETS && size <= seconds)
    .map(([name]) => name);
}

/** "Previous period", "Same time last week": the comparison's name on its own. */
export function compareTitle(compare: CompareValue): string {
  switch (compare.kind) {
    case 'previous':
      return 'Previous period';
    case 'week':
      return 'Same time last week';
    case 'month':
      return 'Same time last month';
    case 'custom':
      return 'Custom';
  }
}

/** The comparison window of `[from, to)`: always of the same length. */
export function comparisonWindow(
  from: Date,
  to: Date,
  compare: CompareValue,
  rangeLabel: string,
): { from: Date; to: Date; label: string } {
  const ms = to.getTime() - from.getTime();
  const span = (start: Date) => ({ from: start, to: new Date(start.getTime() + ms) });
  switch (compare.kind) {
    case 'week':
      return { ...span(new Date(from.getTime() - 7 * 86_400_000)), label: 'the same time last week' };
    case 'month': {
      const start = new Date(from);
      start.setMonth(start.getMonth() - 1);
      return { ...span(start), label: 'the same time last month' };
    }
    case 'custom': {
      const w = span(new Date(compare.from));
      return { ...w, label: `${formatShortDateTime(w.from)} – ${formatShortDateTime(w.to)}` };
    }
    case 'previous': {
      const window = rangeLabel.startsWith('Last ') ? rangeLabel.slice(5).toLowerCase() : null;
      return {
        from: new Date(from.getTime() - ms),
        to: from,
        label: window ? `the previous ${window}` : 'the previous period',
      };
    }
  }
}

/** Serialises a comparison for the URL: absent for the default, `week`, `month` or a start in ms. */
export function encodeCompare(compare: CompareValue): string | null {
  if (compare.kind === 'custom') return String(compare.from);
  return compare.kind === 'previous' ? null : compare.kind;
}

export function decodeCompare(value: string | null | undefined): CompareValue {
  if (value === 'week' || value === 'month') return { kind: value };
  if (value && /^\d{10,16}$/.test(value)) return { kind: 'custom', from: Number(value) };
  return PREVIOUS;
}

export function resolveRange(
  range: RangeValue,
  now: Date = new Date(),
  compare: CompareValue = PREVIOUS,
): ResolvedRange {
  let from: Date;
  let to: Date;
  let label: string;
  if (range.kind === 'preset') {
    to = now;
    from = new Date(now.getTime() - PRESET_SECONDS[range.preset] * 1000);
    label = PRESET_LABEL[range.preset];
  } else {
    from = new Date(range.from);
    to = new Date(range.to);
    label = `${formatShortDate(from)} – ${formatShortDate(to)}`;
  }
  const ms = to.getTime() - from.getTime();
  return {
    from,
    to,
    seconds: ms / 1000,
    label,
    previous: comparisonWindow(from, to, compare, label),
  };
}

/** Serialises a range for the URL: `6h` or `1700000000000-1700003600000`. */
export function encodeRange(range: RangeValue): string {
  return range.kind === 'preset' ? range.preset : `${range.from}-${range.to}`;
}

export function decodeRange(value: string | null | undefined): RangeValue | null {
  if (!value) return null;
  if (isPreset(value)) return { kind: 'preset', preset: value };
  const match = /^(\d{10,16})-(\d{10,16})$/.exec(value);
  if (!match) return null;
  const from = Number(match[1]);
  const to = Number(match[2]);
  return to > from ? { kind: 'custom', from, to } : null;
}

function formatShortDate(d: Date): string {
  return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
}

function formatShortDateTime(d: Date): string {
  const time = d.toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit', hour12: false });
  return `${formatShortDate(d)}, ${time}`;
}

/** The zone label shown next to every time: "UTC" or the IANA name's city. */
export function zoneLabel(timeZone: string): string {
  if (timeZone === 'UTC') return 'UTC';
  const parts = new Intl.DateTimeFormat('en-US', { timeZone, timeZoneName: 'short' }).formatToParts(
    new Date(),
  );
  return parts.find((p) => p.type === 'timeZoneName')?.value ?? timeZone;
}

export function formatClock(value: string | Date, timeZone: string): string {
  const d = typeof value === 'string' ? new Date(value) : value;
  return new Intl.DateTimeFormat('en-GB', {
    timeZone,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  }).format(d);
}

/**
 * A chart's label for a bucket starting at `start`: the time of day within two days, the date with
 * the time over longer ranges ("Oct 9 21:00"), the date alone for buckets of a day or more.
 */
export function chartLabel(
  start: string,
  timeZone: string,
  rangeSeconds: number,
  bucketSeconds = 3600,
): string {
  if (rangeSeconds <= 2 * 86_400) return formatClock(start, timeZone).slice(0, 5);
  const day = new Date(start).toLocaleDateString('en-US', { month: 'short', day: 'numeric', timeZone });
  return bucketSeconds >= 86_400 ? day : `${day} ${formatClock(start, timeZone).slice(0, 5)}`;
}

export function formatDateTime(value: string | Date | null | undefined, timeZone: string): string {
  if (!value) return '—';
  const d = typeof value === 'string' ? new Date(value) : value;
  if (Number.isNaN(d.getTime())) return '—';
  const text = new Intl.DateTimeFormat('en-US', {
    timeZone,
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(d);
  return `${text} ${zoneLabel(timeZone)}`;
}

export function formatDate(value: string | Date | null | undefined, timeZone: string): string {
  if (!value) return '—';
  const d = typeof value === 'string' ? new Date(value) : value;
  if (Number.isNaN(d.getTime())) return '—';
  return new Intl.DateTimeFormat('en-US', {
    timeZone,
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  }).format(d);
}

/** "35s ago", "3m ago", "2h ago", "4d ago", or "in 12d" for future times. */
export function formatRelative(value: string | Date | null | undefined, now: Date = new Date()): string {
  if (!value) return '—';
  const d = typeof value === 'string' ? new Date(value) : value;
  const diff = (now.getTime() - d.getTime()) / 1000;
  if (Number.isNaN(diff)) return '—';
  const future = diff < 0;
  const s = Math.abs(diff);
  let text: string;
  if (s < 60) text = `${Math.round(s)}s`;
  else if (s < 3600) text = `${Math.floor(s / 60)}m`;
  else if (s < 86_400) text = `${Math.floor(s / 3600)}h`;
  else text = `${Math.floor(s / 86_400)}d`;
  return future ? `in ${text}` : `${text} ago`;
}

/** A value for `<input type="datetime-local">` in the given zone's wall time. */
export function toLocalInput(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function daysUntil(value: string | null | undefined, now: Date = new Date()): number | null {
  if (!value) return null;
  const t = new Date(value).getTime();
  if (Number.isNaN(t)) return null;
  return Math.ceil((t - now.getTime()) / 86_400_000);
}
