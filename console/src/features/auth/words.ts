import { plural } from '@/lib/format';

/** A span of time in plain words, rounded down to its largest unit: "30 minutes", "2 hours", "7 days". */
export function spanInWords(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s >= 86_400) return plural(Math.floor(s / 86_400), 'day');
  if (s >= 3600) return plural(Math.floor(s / 3600), 'hour');
  if (s >= 60) return plural(Math.floor(s / 60), 'minute');
  return 'less than a minute';
}

/** How much longer a link works, from its expiry; null when the expiry is unknown or past. */
export function timeLeft(expiresAt: string | null | undefined, now: number = Date.now()): string | null {
  if (!expiresAt) return null;
  const left = (Date.parse(expiresAt) - now) / 1000;
  if (!Number.isFinite(left) || left <= 0) return null;
  return spanInWords(left);
}
