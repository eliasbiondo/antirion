/**
 * Priority shares of a limit: the part of the limit each key class
 * keeps, as the API access limit dialog edits them.
 */

export interface ShareRow {
  /** A key's `priority_class`. */
  cls: string;
  /** Percent of the limit. */
  percent: string;
}

export type PriorityMode = 'strict' | 'borrow';

const CLASS = /^[a-z0-9_]{1,64}$/;

export function sharesFrom(shares: Record<string, number> | null | undefined): ShareRow[] {
  return Object.entries(shares ?? {}).map(([cls, ratio]) => ({
    cls,
    percent: String(Math.round(ratio * 10_000) / 100),
  }));
}

/** Errors by row (`shares.<i>`) and for the whole list (`shares`). */
export function sharesErrors(rows: ShareRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  const seen = new Set<string>();
  let total = 0;
  rows.forEach((r, i) => {
    const cls = r.cls.trim();
    const pct = Number(r.percent);
    if (!CLASS.test(cls)) out[`shares.${i}`] = 'A class uses lowercase letters, digits and _, e.g. prod';
    else if (seen.has(cls)) out[`shares.${i}`] = 'Each class once';
    else if (r.percent.trim() === '' || !Number.isFinite(pct) || pct < 0 || pct > 100)
      out[`shares.${i}`] = 'A percent from 0 to 100';
    seen.add(cls);
    if (Number.isFinite(pct)) total += pct;
  });
  if (!out.shares && total > 100.0001)
    out.shares = `The shares add up to ${Math.round(total * 100) / 100}%; at most 100%`;
  return out;
}

/** The `priority_shares` map (ratios), or null when there are none. */
export function sharesTo(rows: ShareRow[]): Record<string, number> | null {
  if (rows.length === 0) return null;
  return Object.fromEntries(rows.map((r) => [r.cls.trim(), Math.round(Number(r.percent) * 100) / 10_000]));
}

/** What the classes without a share keep: "10% for every other key". */
export function remainderText(rows: ShareRow[]): string {
  const total = rows.reduce((sum, r) => sum + (Number(r.percent) || 0), 0);
  const left = Math.max(0, Math.round((100 - total) * 100) / 100);
  return `${left}% for keys of any other class or none`;
}

/** The shares in a list row: "prod 90% · dev 10%, borrowing". */
export function sharesText(shares: Record<string, number> | null | undefined, mode?: string | null): string {
  const rows = sharesFrom(shares);
  if (rows.length === 0) return '';
  return `${rows.map((r) => `${r.cls} ${r.percent}%`).join(' · ')}${mode === 'borrow' ? ', borrowing' : ''}`;
}
