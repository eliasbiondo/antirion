import { formatClock } from '@/lib/time';

/**
 * Axis labels for event buckets: the clock within two days; the date over longer ranges, with the
 * clock as well when there are several buckets a day, so no two labels read the same.
 */
export function bucketLabels(starts: string[], timeZone: string): string[] {
  const first = Date.parse(starts[0] ?? '');
  const last = Date.parse(starts.at(-1) ?? '');
  const long = starts.length > 1 && last - first > 2 * 86_400_000;
  if (!long) return starts.map((s) => formatClock(s, timeZone).slice(0, 5));
  const step = (last - first) / (starts.length - 1);
  const date = (s: string) =>
    new Date(s).toLocaleDateString('en-US', { month: 'short', day: 'numeric', timeZone });
  return step >= 86_400_000
    ? starts.map(date)
    : starts.map((s) => `${date(s)} ${formatClock(s, timeZone).slice(0, 5)}`);
}
