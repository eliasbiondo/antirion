'use client';

import { useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { usePermission } from '@/components/ui/gate';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { isApiError } from '@/lib/api/client';
import { formatMoney, formatTokens, plural } from '@/lib/format';

import {
  budgetNames,
  type LedgerLine,
  ledgerApi,
  lineSubject,
  SOURCE_LABEL,
  STATUS_LABEL,
  STATUS_TONE,
} from '../api';

type Read = { id: string; lines: LedgerLine[] | null; error: string | null };

function useRequestLines(id: string, enabled: boolean): Read | null {
  const [read, setRead] = useState<Read | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    ledgerApi.request(id).then(
      (lines) => {
        if (live) setRead({ id, lines, error: null });
      },
      (e: unknown) => {
        // A request with no line is a 404 on some gateways and an empty list on others.
        if (live)
          setRead(
            isApiError(e) && e.status === 404
              ? { id, lines: [], error: null }
              : { id, lines: null, error: e instanceof Error ? e.message : 'The ledger could not be read' },
          );
      },
    );
    return () => {
      live = false;
    };
  }, [id, enabled]);
  return read?.id === id ? read : null;
}

function money(v: string | null | undefined): number | null {
  if (v === null || v === undefined) return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

/**
 * The request's cost ledger lines, under the drawer's usage: what was charged, from which usage and
 * to which budgets. Shown to readers of analytics only.
 */
export function RequestLedger({ id }: { id: string }) {
  const canRead = usePermission('analytics:read');
  const read = useRequestLines(id, canRead);
  if (!canRead) return null;
  const lines = read?.lines ?? null;
  const total = (lines ?? []).reduce((a, l) => a + (money(l.billed_usd) ?? 0), 0);
  return (
    <section aria-label="Cost ledger">
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <h3 className="text-ui font-semibold text-fg">Cost ledger</h3>
        {lines && lines.length > 0 ? (
          <span className="text-caption text-muted">
            {plural(lines.length, 'line')} · <span className="mono text-fg">{formatMoney(total)}</span> billed
          </span>
        ) : null}
      </div>
      {!read ? <p className="text-ui text-muted">Reading the ledger…</p> : null}
      {read?.error ? <p className="text-ui text-error">{read.error}</p> : null}
      {lines?.length === 0 ? (
        <p className="text-ui text-muted">
          No ledger line for this request. It may still be settling, or it ended in a way that charges
          nothing.
        </p>
      ) : null}
      {lines && lines.length > 0 ? (
        <Table label="Cost ledger lines">
          <thead>
            <tr>
              <Th>#</Th>
              <Th>For</Th>
              <Th>Outcome</Th>
              <Th numeric>Billed</Th>
              <Th className="max-sm:hidden">Usage</Th>
            </tr>
          </thead>
          <tbody>
            {lines.map((l) => {
              const billed = money(l.billed_usd);
              const cost = money(l.cost_usd);
              const avoided = money(l.avoided_cost_usd);
              const budgets = budgetNames(l);
              const input = typeof l.usage?.input === 'number' ? l.usage.input : null;
              const output = typeof l.usage?.output === 'number' ? l.usage.output : null;
              return (
                <Tr key={`${l.request_id}-${l.line}`}>
                  <Td mono muted>
                    {l.line + 1}
                  </Td>
                  <Td>
                    <div className="mono text-ui text-fg">{lineSubject(l)}</div>
                    <div className="text-caption [overflow-wrap:anywhere] whitespace-normal text-muted">
                      {l.purpose}
                      {l.shadow ? ' · shadow copy' : ''}
                      {budgets.length > 0 ? ` · charged to ${budgets.join(', ')}` : ''}
                    </div>
                  </Td>
                  <Td>
                    <Badge tone={STATUS_TONE[l.status]} dot>
                      {STATUS_LABEL[l.status]}
                    </Badge>
                  </Td>
                  <Td numeric>
                    <Tooltip
                      content={
                        [
                          cost !== null && cost !== billed ? `Cost ${formatMoney(cost)}` : null,
                          l.price_version ? `Prices ${l.price_version}` : null,
                          l.service_tier ? `Tier ${l.service_tier}` : null,
                        ]
                          .filter(Boolean)
                          .join(' · ') || undefined
                      }
                    >
                      <span>{formatMoney(billed)}</span>
                    </Tooltip>
                    {avoided ? (
                      <div className="text-caption text-success">saved {formatMoney(avoided)}</div>
                    ) : null}
                  </Td>
                  <Td className="whitespace-nowrap max-sm:hidden">
                    <div className="mono text-caption text-fg">
                      {input !== null || output !== null
                        ? `${formatTokens(input)} in · ${formatTokens(output)} out`
                        : '—'}
                    </div>
                    <div className="text-caption text-muted">
                      {SOURCE_LABEL[l.usage_source ?? ''] ?? l.usage_source}
                    </div>
                  </Td>
                </Tr>
              );
            })}
          </tbody>
        </Table>
      ) : null}
    </section>
  );
}
