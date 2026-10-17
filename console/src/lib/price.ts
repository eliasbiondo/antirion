import { formatMoney, type TokenUnit } from './format';

/**
 * A price the API states per 1M tokens, as an amount in the user's unit for a price grid ("$2.50"
 * per 1M, "$0.00015" per 1K), without the unit. The money formatter keeps sub-dime amounts to
 * three significant digits, so a per-1K price is never rounded to a wrong or zero value.
 */
export function formatUnitAmount(perMillion: string | number | null | undefined, unit: TokenUnit): string {
  const n = perMillion === null || perMillion === undefined || perMillion === '' ? NaN : Number(perMillion);
  if (!Number.isFinite(n)) return '—';
  return formatMoney(unit === '1K' ? n / 1000 : n);
}
