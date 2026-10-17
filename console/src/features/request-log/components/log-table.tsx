'use client';

import { movesPageList } from '@/components/ui/list-detail';
import { Badge, StatusCode } from '@/components/ui/badge';
import { Mark, useModelFace } from '@/components/ui/marks';
import { Tooltip } from '@/components/ui/tooltip';
import { Table, Td, Th, Tr, Truncate } from '@/components/ui/table';
import type { LogRecord } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatLatency, formatMoney, formatTokens, shortId } from '@/lib/format';
import { useMediaQuery } from '@/lib/hooks';
import { formatClock, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { type ReactNode, useEffect, useRef } from 'react';

import { columnOf, STANDARD_COLUMNS } from '../columns';
import {
  cacheStatus,
  costUsd,
  environmentOf,
  flags,
  hasUsage,
  isStreamed,
  latencyMs,
  type LogSort,
  promptPreview,
  ttftMs,
} from '../types';

export interface LogTableProps {
  records: LogRecord[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  sort?: LogSort;
  order?: 'asc' | 'desc';
  onSort?: (sort: LogSort) => void;
  /** The columns to show, in order (ids of `LOG_COLUMNS`); the standard set by default. */
  columns?: readonly string[];
}

/**
 * The mark beside a request's model: the model's maker (never the provider that served it, which a
 * refused request doesn't have), with who made it and who served it on hover.
 */
function RecordModelMark({ r }: { r: LogRecord }) {
  const model = r.model ?? r.requested_model;
  const face = useModelFace(model, r.upstream_model);
  const made = face.brand === null ? 'Maker unknown' : `Made by ${face.label}`;
  return (
    <Tooltip content={r.provider ? `${made} · served by ${r.provider}` : made}>
      <span className="flex text-muted">
        <Mark face={face} />
      </span>
    </Tooltip>
  );
}

/** A request slower than this is drawn in the warning colour (with its figure, never colour alone). */
const SLOW_MS = 2000;

function RequestFlags({ r }: { r: LogRecord }) {
  const f = flags(r);
  const cache = cacheStatus(r);
  return (
    <span className="flex gap-1">
      {cache === 'hit' || cache === 'semantic_hit' ? <Badge tone="info">Cache</Badge> : null}
      {isStreamed(r) ? <Badge>Stream</Badge> : null}
      {r.tool_call_count ? <Badge>Tools {r.tool_call_count}</Badge> : null}
      {f.map((x) => (
        <Badge key={x} tone={x === 'pii' ? 'warn' : 'error'} dot>
          {x === 'pii' ? 'PII' : x.charAt(0).toUpperCase() + x.slice(1)}
        </Badge>
      ))}
    </span>
  );
}

function hasFlags(r: LogRecord): boolean {
  const cache = cacheStatus(r);
  return (
    cache === 'hit' ||
    cache === 'semantic_hit' ||
    isStreamed(r) ||
    Boolean(r.tool_call_count) ||
    flags(r).length > 0
  );
}

function Tokens({ r }: { r: LogRecord }) {
  if (!hasUsage(r)) return <span className="text-dim">—</span>;
  return (
    <>
      {formatTokens(r.usage.input)}
      <span className="text-dim"> → </span>
      <span className="text-accent">{formatTokens(r.usage.output)}</span>
    </>
  );
}

/** On a phone each request is one stacked row: time and status, the model, then the figures. */
function RequestCards({ records, selectedId, onSelect }: LogTableProps) {
  const timeZone = usePreferences((s) => s.timeZone);
  return (
    <ul aria-label="Requests" className="flex flex-col">
      {records.map((r) => {
        const latency = latencyMs(r);
        return (
          <li key={r.id}>
            <button
              type="button"
              onClick={() => onSelect(r.id)}
              aria-current={r.id === selectedId ? 'true' : undefined}
              className={cn(
                'flex w-full flex-col gap-1 border-b border-line px-gutter-sm py-2.5 text-left',
                r.id === selectedId ? 'bg-accent-dim' : 'bg-raised hover:bg-hover',
              )}
            >
              <span className="flex items-center gap-2 text-caption text-muted">
                <StatusCode code={r.status_code} />
                <span className="mono">{formatClock(r.started_at, timeZone)}</span>
                <span className="text-dim">{formatRelative(r.started_at)}</span>
                <span className="mono ml-auto text-fg tabular-nums">{formatMoney(costUsd(r))}</span>
              </span>
              <span className="flex items-center gap-2 text-ui">
                <RecordModelMark r={r} />
                <span className="mono truncate text-fg">{r.model ?? r.requested_model ?? '—'}</span>
                <span
                  className={cn(
                    'mono ml-auto shrink-0 tabular-nums',
                    latency !== null && latency > SLOW_MS ? 'text-warn' : 'text-fg',
                  )}
                >
                  {formatLatency(latency)}
                </span>
              </span>
              <span className="flex items-center gap-2 text-caption text-muted">
                <span className="truncate">
                  {r.team ?? '—'} · {r.end_user ?? r.key ?? '—'}
                </span>
                <span className="mono ml-auto shrink-0 tabular-nums">
                  <Tokens r={r} />
                </span>
              </span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

/**
 * The request table, shared by the log and the Overview; arrow keys move, Enter opens. Columns
 * follow the user's choice; the prompt and flags give way when every row would leave them empty,
 * and the request id first when the table is narrow; on a phone the rows stack.
 */
export function LogTable(props: LogTableProps) {
  const { records, selectedId, onSelect, sort, order, onSort } = props;
  const timeZone = usePreferences((s) => s.timeZone);
  const phone = useMediaQuery('(max-width: 639px)');
  const body = useRef<HTMLTableSectionElement>(null);
  // ↑, ↓, j and k enter the list from anywhere on the page that is not a field: the selected row,
  // else the first.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!movesPageList(e, body.current)) return;
      const rows = Array.from(body.current?.querySelectorAll<HTMLTableRowElement>('tr') ?? []);
      const target = rows.find((r) => r.getAttribute('aria-selected') === 'true') ?? rows[0];
      if (!target) return;
      e.preventDefault();
      target.focus();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  if (phone && records.length > 0) return <RequestCards {...props} />;
  const prompts = records.some((r) => promptPreview(r) !== null);
  const anyFlags = records.some(hasFlags);
  const chosen = props.columns ?? STANDARD_COLUMNS;
  const columns = chosen.filter((c) => (c === 'prompt' ? prompts : c === 'flags' ? anyFlags : true));
  const ttftColumn = columns.includes('ttft');
  /** The request id column hides when the table is narrower than this. */
  const narrow: Record<string, string> = {
    request_id: '@max-[1180px]:hidden',
    prompt: '@max-[1020px]:hidden',
  };
  const sortProps = (key: LogSort | undefined) =>
    onSort && key ? { sort: sort === key ? (order ?? 'desc') : null, onSort: () => onSort(key) } : {};

  const cell = (id: string, r: LogRecord): ReactNode => {
    const latency = latencyMs(r);
    const ttft = ttftMs(r);
    switch (id) {
      case 'time':
        return (
          <Td key={id} mono>
            {formatClock(r.started_at, timeZone)}{' '}
            <span className="text-caption text-dim">{formatRelative(r.started_at)}</span>
          </Td>
        );
      case 'request_id':
        return (
          <Td key={id} mono muted title={r.request_id} className={narrow.request_id}>
            {shortId(r.request_id, 14)}
          </Td>
        );
      case 'status':
        return (
          <Td key={id} title={r.code ?? undefined}>
            <StatusCode code={r.status_code} />
          </Td>
        );
      case 'model':
        return (
          <Td key={id}>
            <span className="mono flex items-center gap-1.5">
              <RecordModelMark r={r} />
              {r.model ?? r.requested_model ?? '—'}
            </span>
          </Td>
        );
      case 'team':
        return (
          <Td key={id} className="max-w-[240px]">
            <Truncate text={`${r.team ?? '—'} · ${r.end_user ?? r.key ?? '—'}`}>
              {r.team ?? '—'}{' '}
              <span className="mono text-caption text-muted">· {r.end_user ?? r.key ?? '—'}</span>
            </Truncate>
          </Td>
        );
      case 'prompt':
        return (
          <Td key={id} muted className={cn('max-w-[320px] truncate', narrow.prompt)}>
            {promptPreview(r) ?? ''}
          </Td>
        );
      case 'tokens':
        return (
          <Td key={id} numeric>
            <Tokens r={r} />
          </Td>
        );
      case 'output':
        return (
          <Td key={id} numeric>
            {r.usage.output != null ? formatTokens(r.usage.output) : <span className="text-dim">—</span>}
          </Td>
        );
      case 'latency':
        return (
          <Td key={id} numeric>
            <span className={latency !== null && latency > SLOW_MS ? 'text-warn' : undefined}>
              {formatLatency(latency)}
            </span>
            {ttft !== null && !ttftColumn ? (
              <span className="ml-1.5 text-caption text-dim">ttft {formatLatency(ttft)}</span>
            ) : null}
          </Td>
        );
      case 'ttft':
        return (
          <Td key={id} numeric>
            {ttft !== null ? formatLatency(ttft) : <span className="text-dim">—</span>}
          </Td>
        );
      case 'cost':
        return (
          <Td key={id} numeric>
            {formatMoney(costUsd(r))}
          </Td>
        );
      case 'flags':
        return (
          <Td key={id}>
            <RequestFlags r={r} />
          </Td>
        );
      case 'key':
        return (
          <Td key={id} mono muted>
            {r.key ?? '—'}
          </Td>
        );
      case 'environment':
        return <Td key={id}>{environmentOf(r) ?? <span className="text-dim">—</span>}</Td>;
      case 'endpoint':
        return (
          <Td key={id} mono muted>
            {r.endpoint}
          </Td>
        );
      case 'route':
        return (
          <Td key={id} mono muted>
            {r.route ?? '—'}
          </Td>
        );
      default:
        return null;
    }
  };

  return (
    <Table label="Requests" className="@container">
      <thead>
        <tr>
          {columns.map((id) => {
            const c = columnOf(id);
            if (!c) return null;
            return (
              <Th key={id} numeric={c.numeric} className={narrow[id]} {...sortProps(c.sort)}>
                {id === 'ttft' ? 'TTFT' : c.label}
              </Th>
            );
          })}
        </tr>
      </thead>
      <tbody
        ref={body}
        onKeyDown={(e) => {
          const down = e.key === 'ArrowDown' || e.key === 'j';
          if (!down && e.key !== 'ArrowUp' && e.key !== 'k') return;
          e.preventDefault();
          const rows = Array.from(e.currentTarget.querySelectorAll<HTMLTableRowElement>('tr'));
          const i = rows.indexOf(document.activeElement as HTMLTableRowElement);
          rows[Math.max(0, Math.min(rows.length - 1, i + (down ? 1 : -1)))]?.focus();
        }}
      >
        {records.map((r) => (
          <Tr
            key={r.id}
            interactive
            selected={r.id === selectedId}
            tabIndex={0}
            onClick={() => onSelect(r.id)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') onSelect(r.id);
            }}
          >
            {columns.map((id) => cell(id, r))}
          </Tr>
        ))}
      </tbody>
    </Table>
  );
}
