import type { LogSort } from './types';

/** A column of the request table: its id (kept in preferences and saved views), label and sort. */
export interface LogColumn {
  id: string;
  label: string;
  /** The server-side sort the header applies, when the contract has one for it. */
  sort?: LogSort;
  numeric?: boolean;
  /** Shown when the user has not chosen columns. */
  standard: boolean;
  /** Cannot be hidden: a row needs it to be recognised. */
  fixed?: boolean;
  /** When a chosen column still gives way, said where columns are chosen. */
  note?: string;
}

export const LOG_COLUMNS: LogColumn[] = [
  { id: 'time', label: 'Time', sort: 'started_at', standard: true, fixed: true },
  { id: 'request_id', label: 'Request ID', standard: true, note: 'Gives way while the table is narrow' },
  { id: 'status', label: 'Status', sort: 'status_code', standard: true },
  { id: 'model', label: 'Model', standard: true },
  { id: 'team', label: 'Team · user', standard: true },
  { id: 'prompt', label: 'Prompt', standard: true, note: 'Shown when a request has a prompt to preview' },
  { id: 'tokens', label: 'Tokens', sort: 'input_tokens', numeric: true, standard: true },
  { id: 'output', label: 'Output tokens', sort: 'output_tokens', numeric: true, standard: false },
  { id: 'latency', label: 'Latency', sort: 'latency_ms', numeric: true, standard: true },
  { id: 'ttft', label: 'Time to first token', sort: 'ttft_ms', numeric: true, standard: false },
  { id: 'cost', label: 'Cost', sort: 'cost_usd', numeric: true, standard: true },
  { id: 'flags', label: 'Flags', standard: true, note: 'Shown when a request is flagged' },
  { id: 'key', label: 'API key', standard: false },
  { id: 'environment', label: 'Environment', standard: false },
  { id: 'endpoint', label: 'Endpoint', standard: false },
  { id: 'route', label: 'Route', standard: false },
];

export const STANDARD_COLUMNS: string[] = LOG_COLUMNS.filter((c) => c.standard).map((c) => c.id);

const BY_ID = new Map(LOG_COLUMNS.map((c) => [c.id, c]));

export function columnOf(id: string): LogColumn | undefined {
  return BY_ID.get(id);
}

/**
 * The columns to show, in order: the chosen ids the console knows, without repeats, with the fixed
 * ones kept; the standard set when nothing (or nothing known) was chosen.
 */
export function resolveColumns(chosen: readonly string[] | null | undefined): string[] {
  const known = [...new Set((chosen ?? []).filter((id) => BY_ID.has(id)))];
  if (known.length === 0) return STANDARD_COLUMNS;
  const fixed = LOG_COLUMNS.filter((c) => c.fixed && !known.includes(c.id)).map((c) => c.id);
  return [...fixed, ...known];
}

/** Whether a choice is the standard set, so it need not be stored. */
export function isStandard(columns: readonly string[]): boolean {
  return columns.length === STANDARD_COLUMNS.length && columns.every((c, i) => c === STANDARD_COLUMNS[i]);
}

/** Moves a column one place up or down. */
export function moveColumn(columns: readonly string[], id: string, by: -1 | 1): string[] {
  const i = columns.indexOf(id);
  const j = i + by;
  if (i < 0 || j < 0 || j >= columns.length) return [...columns];
  const next = [...columns];
  const a = next[i];
  const b = next[j];
  if (a === undefined || b === undefined) return next;
  next[i] = b;
  next[j] = a;
  return next;
}

/** Shows or hides a column; a shown column goes back to its place in the standard order. */
export function toggleColumn(columns: readonly string[], id: string): string[] {
  if (columnOf(id)?.fixed) return [...columns];
  if (columns.includes(id)) return columns.filter((c) => c !== id);
  const order = LOG_COLUMNS.map((c) => c.id);
  const at = order.indexOf(id);
  const next = [...columns];
  const after = next.findIndex((c) => order.indexOf(c) > at);
  next.splice(after < 0 ? next.length : after, 0, id);
  return next;
}
