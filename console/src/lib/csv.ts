import { usesTimeRange } from '@/lib/nav';
import { resolveRange } from '@/lib/time';
import { useTimeRange } from '@/stores/time-range';

/** Formats rows as RFC 4180 CSV: CRLF line ends, quotes doubled, fields quoted when needed. */
export function toCsv(header: string[], rows: (string | number | null | undefined)[][]): string {
  const cell = (value: string | number | null | undefined) => {
    const text = value === null || value === undefined ? '' : String(value);
    return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
  };
  return [header, ...rows].map((r) => r.map(cell).join(',')).join('\r\n') + '\r\n';
}

/** Writes CSV in the browser and saves it; nothing leaves the page. */
export function downloadCsv(
  filename: string,
  header: string[],
  rows: (string | number | null | undefined)[][],
): void {
  downloadText(filename, toCsv(header, rows), 'text/csv');
}

/** Saves text the page already holds as a file. */
export function downloadText(filename: string, text: string, type: string): void {
  const blob = new Blob([text], { type: `${type};charset=utf-8` });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** One column of a CSV export: its header and how a row gives its cell. */
export interface CsvColumn<T> {
  header: string;
  value: (row: T) => string | number | boolean | null | undefined;
}

/** The rows a table shows, as CSV text with the given columns. */
export function rowsToCsv<T>(columns: CsvColumn<T>[], rows: T[]): string {
  return toCsv(
    columns.map((c) => c.header),
    rows.map((r) =>
      columns.map((c) => {
        const v = c.value(r);
        return typeof v === 'boolean' ? String(v) : v;
      }),
    ),
  );
}

/** Query parameters that pick an item, open a tab or a dialog, or set the scope: not filters. */
const NOT_FILTERS = new Set([
  'org',
  'env',
  'range',
  'bucket',
  'cmp',
  'tab',
  'view',
  'settings',
  'account',
  'sort',
  'request',
  'alert',
  'budget',
  'plugin',
  'agent',
  'server',
  'gateway',
  'rule',
  'detail',
  'route',
  'event',
  'entry',
  'provider',
]);

/** The item a screen selects by a parameter that elsewhere is a filter (`key` on API keys). */
const SELECTED: Record<string, string[]> = {
  '/keys/': ['key'],
  '/models/': ['model'],
  '/teams/': ['user', 'team'],
};

function slug(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 40);
}

/**
 * A file name that says what an export holds: the screen's name, the filters in the page's
 * address and the days it covers ("alerts-firing-critical-2026-10-09-to-2026-10-16.csv").
 */
export function exportFileName(
  base: string,
  ext: string,
  window: { from: string | Date; to: string | Date } | null = null,
  location: { pathname: string; search: string } = globalThis.location,
): string {
  if (base.endsWith(`.${ext}`)) return base;
  const path = location.pathname.replace(/^\/ui(?=\/)/, '');
  const selected = SELECTED[path.endsWith('/') ? path : `${path}/`] ?? [];
  const params = new URLSearchParams(location.search);
  const filters: string[] = [];
  for (const [k, v] of params) {
    if (NOT_FILTERS.has(k) || selected.includes(k) || v === '' || v === 'all') continue;
    filters.push(slug(v === '1' || v === 'true' ? k : v));
  }
  const day = (d: string | Date) => (typeof d === 'string' ? d : d.toISOString()).slice(0, 10);
  const today = new Date().toISOString().slice(0, 10);
  // A screen that reads the time range exports that range's days.
  const range =
    window ??
    (usesTimeRange(path)
      ? (() => {
          const t = useTimeRange.getState();
          return resolveRange(t.range, new Date(t.anchoredAt), t.compare);
        })()
      : null);
  const from = range ? day(range.from) : today;
  const to = range ? day(range.to) : today;
  const dates = from === to ? from : `${from}-to-${to}`;
  return `${[base, ...filters.filter(Boolean).slice(0, 4), dates].join('-')}.${ext}`;
}

/** Saves the rows a table shows as CSV, with the given columns; any table can offer it. */
export function exportCsv<T>(
  filename: string,
  columns: CsvColumn<T>[],
  rows: T[],
  window: { from: string | Date; to: string | Date } | null = null,
): void {
  downloadText(exportFileName(filename, 'csv', window), rowsToCsv(columns, rows), 'text/csv');
}
