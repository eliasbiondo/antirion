'use client';

import Link from 'next/link';
import { useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { BarRow } from '@/components/ui/charts';
import { Segmented } from '@/components/ui/form';
import { Panel } from '@/components/ui/layout';
import { ModelMark, ProviderMark } from '@/components/ui/marks';
import { EmptyState } from '@/components/ui/states';
import { Tooltip } from '@/components/ui/tooltip';
import type { BudgetCounter, UsageBucket } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount, formatMoney, hueOf } from '@/lib/format';
import { useConfiguredProviders, useProviderType } from '@/stores/provider-types';

import { groupTotals } from '@/features/analytics/store';
import type { ProviderStatus } from '@/features/providers/api';

import { headroom, teamBudgets } from '../runtime';

/** A team's colour when it has none of its own: a hue from its name. */
export function teamColor(name: string): string {
  return `oklch(0.68 0.13 ${hueOf(name)})`;
}

function TeamDot({ name }: { name: string }) {
  return (
    <span
      aria-hidden="true"
      ref={(el) => {
        if (el) el.style.backgroundColor = teamColor(name);
      }}
      className="inline-block h-1.5 w-1.5 shrink-0 rounded-full"
    />
  );
}

function emptyRows(text: string) {
  return <EmptyState size="sm" title={text} />;
}

const STATE_TONE = { on_pace: undefined, at_risk: 'warn', over: 'error', blocked: 'error' } as const;
const STATE_WORDS = {
  on_pace: 'on pace',
  at_risk: 'at risk',
  over: 'over budget',
  blocked: 'blocked',
} as const;

/**
 * Teams' spend this month against their monthly budget, coloured by the budget's state; a team
 * without a budget shows its spend alone.
 */
export function TeamSpend({
  usage,
  counters,
  className,
}: {
  usage: UsageBucket[] | null | undefined;
  counters: BudgetCounter[] | null;
  className?: string;
}) {
  const budgetsHref = useScopedHref('/budgets/');
  const budgets = teamBudgets(counters);
  const spent = new Map(groupTotals(usage, 'team').map((t) => [t.name, t.totals.cost] as const));
  const names = [...new Set([...spent.keys(), ...budgets.keys()])].filter((n) => n !== 'other');
  const rows = names
    .map((name) => {
      const b = budgets.get(name);
      const amount = b ? Number(b.effective_amount ?? b.amount) : null;
      return {
        name,
        spent: b ? Number(b.spent) : (spent.get(name) ?? 0),
        amount: amount && amount > 0 ? amount : null,
        state: b?.state ?? null,
      };
    })
    .sort((a, b) => b.spent - a.spent)
    .slice(0, 8);
  const maxSpend = Math.max(0.0001, ...rows.map((r) => r.spent));
  return (
    <Panel
      title="Team spend"
      className={className}
      subtitle={
        budgets.size > 0 ? 'This month, on budget counters where a team has one' : 'This month, from requests'
      }
      actions={
        <Link href={budgetsHref} className="text-caption text-muted hover:text-fg">
          View budgets
        </Link>
      }
    >
      {rows.length === 0 ? (
        emptyRows('No spend this month')
      ) : (
        <ul aria-label="Spend by team this month" className="flex flex-col gap-0.5">
          {rows.map((t) => {
            const share = t.amount ? t.spent / t.amount : null;
            const tone = t.state ? STATE_TONE[t.state] : undefined;
            return (
              <BarRow
                key={t.name}
                name={
                  <>
                    <TeamDot name={t.name} />
                    <span className="truncate">{t.name}</span>
                  </>
                }
                value={t.spent}
                max={t.amount ?? maxSpend}
                {...(tone ? { tone } : { color: teamColor(t.name) })}
                label={
                  t.amount
                    ? `${t.name}: ${formatMoney(t.spent)} of ${formatMoney(t.amount)}${t.state ? `, ${STATE_WORDS[t.state]}` : ''}`
                    : `${t.name}: ${formatMoney(t.spent)}, no monthly budget`
                }
                display={
                  <>
                    {formatMoney(t.spent, { compact: true })}
                    {t.amount ? (
                      <>
                        <span className="text-dim"> / {formatMoney(t.amount, { compact: true })}</span>
                        <span
                          className={cn(
                            'ml-2 inline-block w-9 text-right',
                            tone === 'error' ? 'text-error' : tone === 'warn' ? 'text-warn' : 'text-muted',
                          )}
                        >
                          {Math.round((share ?? 0) * 100)}%
                        </span>
                      </>
                    ) : (
                      <span className="ml-2 inline-block w-9 text-right text-dim">—</span>
                    )}
                  </>
                }
              />
            );
          })}
        </ul>
      )}
    </Panel>
  );
}

/** The eight Models with the most requests or spend in the range, each linking to its screen. */
export function TopModels({
  usage,
  format,
}: {
  usage: UsageBucket[] | null | undefined;
  format: (n: number) => string;
}) {
  const [by, setBy] = useState<'requests' | 'spend'>('requests');
  const modelsHref = useScopedHref('/models/');
  const models = groupTotals(usage, 'model')
    .filter((m) => m.name !== 'other')
    .sort((a, b) => (by === 'spend' ? b.totals.cost - a.totals.cost : b.totals.requests - a.totals.requests))
    .slice(0, 8);
  const max = Math.max(
    by === 'spend' ? 0.0001 : 1,
    ...models.map((m) => (by === 'spend' ? m.totals.cost : m.totals.requests)),
  );
  const link = (name: string) =>
    `${modelsHref}${modelsHref.includes('?') ? '&' : '?'}model=${encodeURIComponent(name)}`;
  return (
    <Panel
      title="Top models"
      actions={
        <Segmented<'requests' | 'spend'>
          label="Rank models by"
          value={by}
          onChange={setBy}
          options={[
            { value: 'requests', label: 'Requests' },
            { value: 'spend', label: 'Spend' },
          ]}
          className="[&_button]:h-6 [&_button]:px-2 [&_button]:text-caption"
        />
      }
    >
      {models.length === 0 ? (
        emptyRows('No requests in this range')
      ) : (
        <ul aria-label={`Top models by ${by}`} className="flex flex-col gap-0.5">
          {models.map((m) => (
            <BarRow
              key={m.name}
              name={
                <>
                  <ModelMark model={m.name} className="text-muted" />
                  <Link
                    href={link(m.name)}
                    className="mono truncate rounded-xs text-caption text-fg hover:text-accent hover:underline"
                  >
                    {m.name}
                  </Link>
                </>
              }
              value={by === 'spend' ? m.totals.cost : m.totals.requests}
              max={max}
              label={`${m.name}: ${formatCount(m.totals.requests)} requests, ${formatMoney(m.totals.cost)}`}
              display={
                by === 'spend' ? (
                  <>
                    {formatMoney(m.totals.cost, { compact: true })}
                    <span className="text-dim"> · </span>
                    <span className="text-muted">{format(m.totals.requests)}</span>
                  </>
                ) : (
                  <>
                    {format(m.totals.requests)}
                    <span className="text-dim"> · </span>
                    <span className="text-muted">{formatMoney(m.totals.cost, { compact: true })}</span>
                  </>
                )
              }
            />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/** Warn at this share of a provider's limit. */
const WARN_AT = 0.8;

/**
 * Each provider's headroom: requests per minute against the limit it last reported, with its
 * account tier, warning from 80 %; a provider that reports no limit shows its rate alone.
 */
export function ProviderHeadroom({
  usage,
  status,
  minutes,
}: {
  usage: UsageBucket[] | null | undefined;
  status: ProviderStatus[] | null;
  minutes: number;
}) {
  const typeOf = useProviderType();
  const providersHref = useScopedHref('/providers/');
  const configuredList = useConfiguredProviders();
  const traffic = new Map(groupTotals(usage, 'provider').map((p) => [p.name, p.totals] as const));
  // Headroom is a configured provider's: traffic of a provider removed since is left out.
  const configured = status ? new Set(status.map((s) => s.name)) : configuredList;
  const names = [...new Set([...(status ?? []).map((s) => s.name), ...traffic.keys()])].filter(
    (n) => n !== 'other' && (configured === null || configured.has(n)),
  );
  const rows = names
    .map((name) => {
      const s = status?.find((x) => x.name === name);
      const t = traffic.get(name);
      return {
        name,
        room: s ? headroom(s) : null,
        tier: s?.account_tier ?? null,
        rate: (t?.requests ?? 0) / minutes,
        errors: t ? t.errors / Math.max(1, t.requests) : 0,
      };
    })
    .sort((a, b) => (b.room?.share ?? -1) - (a.room?.share ?? -1) || b.rate - a.rate)
    .slice(0, 8);
  const maxRate = Math.max(1, ...rows.map((r) => r.rate));
  const warned = rows.filter((r) => (r.room?.share ?? 0) >= WARN_AT).length;
  return (
    <Panel
      title="Provider headroom"
      subtitle={warned ? `${warned} near the limit` : 'against upstream limits'}
      actions={
        <Link href={providersHref} className="text-caption text-muted hover:text-fg">
          View providers
        </Link>
      }
    >
      {rows.length === 0 ? (
        emptyRows('No providers yet')
      ) : (
        <ul aria-label="Provider headroom" className="flex flex-col gap-0.5">
          {rows.map((r) => {
            const room = r.room;
            const tone = room
              ? room.share >= 0.95
                ? 'error'
                : room.share >= WARN_AT
                  ? 'warn'
                  : 'accent'
              : 'muted';
            return (
              <BarRow
                key={r.name}
                name={
                  <>
                    <ProviderMark type={typeOf(r.name)} className="text-muted" />
                    <span className="truncate">{r.name}</span>
                    {r.tier ? <span className="truncate text-caption text-dim">· {r.tier}</span> : null}
                  </>
                }
                value={room ? room.used : r.rate}
                max={room ? room.limit : maxRate}
                tone={tone}
                label={
                  room
                    ? `${r.name}: ${formatCount(room.used)} of ${formatCount(room.limit)} ${room.unit} per minute used upstream`
                    : `${r.name}: ${r.rate.toFixed(1)} requests per minute; the provider reports no limit`
                }
                display={
                  room ? (
                    <Tooltip
                      content={`Last reading from the provider${room.stale ? ', no longer current' : ''}: ${formatCount(room.used)} of ${formatCount(room.limit)} ${room.unit} per minute`}
                    >
                      <span
                        className={cn(
                          tone === 'error' ? 'text-error' : tone === 'warn' ? 'text-warn' : undefined,
                        )}
                      >
                        {formatCount(room.used, { compact: true })}
                        <span className="text-dim"> / {formatCount(room.limit, { compact: true })}</span>
                        {room.unit === 'tokens' ? <span className="text-dim"> tok</span> : null}
                        <span className="ml-2 inline-block w-9 text-right">
                          {Math.round(room.share * 100)}%
                        </span>
                      </span>
                    </Tooltip>
                  ) : (
                    <Tooltip content="The provider reports no rate limit; this is the gateway's own rate in the range">
                      <span className="text-muted">
                        {r.rate.toFixed(1)}/min
                        <span className="ml-2 inline-block w-9 text-right text-dim">—</span>
                      </span>
                    </Tooltip>
                  )
                }
              />
            );
          })}
        </ul>
      )}
    </Panel>
  );
}
