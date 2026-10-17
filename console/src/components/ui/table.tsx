'use client';

import {
  type HTMLAttributes,
  type ReactNode,
  type TdHTMLAttributes,
  type ThHTMLAttributes,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';

import { cn } from '@/lib/cn';
import { type CsvColumn, exportCsv } from '@/lib/csv';

import { Button } from './button';
import { Icon } from './icon';
import { Tooltip } from './tooltip';

export function Table({
  children,
  label,
  className,
}: {
  children: ReactNode;
  label: string;
  className?: string;
}) {
  return (
    <div className={cn('min-w-0 overflow-x-auto', className)}>
      <table aria-label={label} className="w-full border-separate border-spacing-0 text-body">
        {children}
      </table>
    </div>
  );
}

export type SortDirection = 'asc' | 'desc';

export interface ThProps extends ThHTMLAttributes<HTMLTableCellElement> {
  numeric?: boolean;
  sort?: SortDirection | null;
  onSort?: () => void;
}

export function Th({ numeric, sort, onSort, children, className, ...rest }: ThProps) {
  const content = onSort ? (
    <button
      type="button"
      onClick={onSort}
      className={cn(
        'group inline-flex items-center gap-1 rounded-xs hover:text-fg',
        numeric && 'flex-row-reverse',
        sort && 'text-fg',
      )}
    >
      {children}
      <span aria-hidden="true" className={cn(sort ? 'text-accent' : 'opacity-0 group-hover:opacity-60')}>
        <Icon name={sort === 'asc' ? 'arrowUp' : 'arrowDown'} size={12} />
      </span>
    </button>
  ) : (
    children
  );
  return (
    <th
      scope="col"
      aria-sort={sort === 'asc' ? 'ascending' : sort === 'desc' ? 'descending' : undefined}
      className={cn(
        'sticky top-0 z-[1] h-8 border-b border-line bg-raised px-[var(--spacing-cell)] text-left align-middle text-caption font-medium whitespace-nowrap text-muted',
        numeric && 'text-right',
        className,
      )}
      {...rest}
    >
      {content}
    </th>
  );
}

export interface TrProps extends HTMLAttributes<HTMLTableRowElement> {
  selected?: boolean;
  interactive?: boolean;
}

export function Tr({ selected, interactive, className, ...rest }: TrProps) {
  return (
    <tr
      aria-selected={interactive ? Boolean(selected) : undefined}
      className={cn(
        interactive && 'cursor-pointer hover:[&>td]:bg-hover focus-visible:[&>td]:bg-hover',
        selected && '[&>td]:bg-accent-dim',
        className,
      )}
      {...rest}
    />
  );
}

export interface TdProps extends TdHTMLAttributes<HTMLTableCellElement> {
  numeric?: boolean;
  mono?: boolean;
  muted?: boolean;
}

export function Td({ numeric, mono, muted, className, title, children, ...rest }: TdProps) {
  return (
    <td
      className={cn(
        'h-[var(--spacing-row)] border-b border-line px-[var(--spacing-cell)] align-middle whitespace-nowrap',
        (numeric || mono) && 'mono tabular-nums',
        numeric && 'text-right',
        muted ? 'text-muted' : 'text-fg',
        className,
      )}
      {...rest}
    >
      {/* The full value of a shortened cell, in the console's tooltip rather than the browser's. */}
      {title ? (
        <Tooltip content={title}>
          <span>{children}</span>
        </Tooltip>
      ) : (
        children
      )}
    </td>
  );
}

/**
 * Text cut with an ellipsis at the width its cell leaves (a long key or team name), so the columns
 * after it stay on screen; once cut, the whole text shows in a tooltip.
 */
export function Truncate({
  text,
  className,
  children,
}: {
  text: string;
  className?: string;
  /** What to draw, when it is more than the plain text (a name and its muted detail). */
  children?: ReactNode;
}) {
  const ref = useRef<HTMLSpanElement>(null);
  const [cut, setCut] = useState(false);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const check = () => setCut(el.scrollWidth > el.clientWidth + 1);
    check();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(check);
    observer.observe(el);
    return () => observer.disconnect();
  }, [text]);
  return (
    <Tooltip content={cut ? text : null}>
      <span ref={ref} className={cn('block min-w-0 truncate', className)}>
        {children ?? text}
      </span>
    </Tooltip>
  );
}

/** "keys" → "key", "policies" → "policy": the singular of a count's plural noun. */
function singularOf(noun: string): string {
  if (noun.endsWith('ies')) return `${noun.slice(0, -3)}y`;
  if (noun.endsWith('s') && !noun.endsWith('ss')) return noun.slice(0, -1);
  return noun;
}

/**
 * "12 of 40 keys" at the right of a filter bar; nothing while the list is empty. `noun` is the
 * plural; the singular is derived ("1 key") unless given.
 */
export function CountNote({
  shown,
  total,
  noun,
  singular,
}: {
  shown: number;
  total: number;
  noun: string;
  singular?: string;
}) {
  if (total === 0) return null;
  const word = total === 1 ? (singular ?? singularOf(noun)) : noun;
  const tail = word ? ` ${word}` : '';
  return (
    <span className="text-caption text-muted tabular-nums" aria-live="polite">
      {shown === total ? `${total}${tail}` : `${shown} of ${total}${tail}`}
    </span>
  );
}

/**
 * "Export CSV" for any table: the rows it shows now (its filters applied), with the given columns,
 * written in the browser. Nothing while there are no rows.
 */
export function ExportCsvButton<T>({
  filename,
  columns,
  rows,
  label = 'Export CSV',
}: {
  filename: string;
  columns: CsvColumn<T>[];
  rows: T[];
  label?: string;
}) {
  if (rows.length === 0) return null;
  return (
    <Button icon="download" variant="ghost" onClick={() => exportCsv(filename, columns, rows)}>
      {label}
    </Button>
  );
}
