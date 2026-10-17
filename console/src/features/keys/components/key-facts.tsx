'use client';

import Link from 'next/link';

import { useScopedHref } from '@/components/layout/url-sync';
import { Meter, TimeChart } from '@/components/ui/charts';
import { DefinitionGrid } from '@/components/ui/layout';
import { Tooltip } from '@/components/ui/tooltip';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { formatCount, formatMoney, formatPercent, formatSigned } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import type { LimitCounter } from '../api';
import { type Hourly, useKeysContext } from '../store';

/** The last 24 hours and the 24 before, from 48 hourly buckets ending now. */
export function splitDays(hours: Hourly[], now: number): { current: Hourly[]; previous: Hourly[] } {
  const cut = now - 24 * 3_600_000;
  return {
    current: hours.filter((h) => new Date(h.start).getTime() >= cut),
    previous: hours.filter((h) => new Date(h.start).getTime() < cut),
  };
}

/** The change of `value` against `before`, as a fraction; null when there was nothing before. */
export function change(value: number, before: number): number | null {
  if (before <= 0) return null;
  return value / before - 1;
}

/** A change against the 24 hours before, after a figure ("+12%"); nothing when there was nothing before. */
export function DeltaText({ value, before }: { value: number; before: number }) {
  const c = change(value, before);
  if (c === null) return null;
  return (
    <Tooltip content="Against the 24 hours before">
      <span className="ml-2 text-caption text-muted">{formatSigned(c, 0, { percent: true })}</span>
    </Tooltip>
  );
}

/** "14:00" in the reader's time zone. */
function hourLabel(start: number, timeZone: string): string {
  return new Date(start).toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit', timeZone });
}

/** 24 hourly values ending at the current hour, missing hours as zero. */
export function lastDay(
  hours: Hourly[],
  now: number,
  pick: (h: Hourly) => number,
): { labels: string[]; values: number[]; starts: number[] } {
  const hour = 3_600_000;
  const end = Math.floor(now / hour) * hour;
  const starts = Array.from({ length: 24 }, (_, i) => end - (23 - i) * hour);
  const by = new Map(hours.map((h) => [Math.floor(new Date(h.start).getTime() / hour) * hour, h]));
  return {
    starts,
    labels: starts.map((s) => new Date(s).toISOString()),
    values: starts.map((s) => {
      const h = by.get(s);
      return h ? pick(h) : 0;
    }),
  };
}

/** A key's last 24 hours: requests and spend against the 24 before, and requests per hour. */
export function KeyHours({ name, now }: { name: string; now: number }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const slot = useKeysContext((s) => s.keyHourly[name]);
  if (!slot || (slot.status === 'loading' && !slot.data)) return <LoadingRows rows={3} />;
  if (slot.error) return <ErrorState error={slot.error} subject="The key's hours" size="sm" />;
  const hours = slot.data ?? [];
  const { current, previous } = splitDays(hours, now);
  const sum = (list: Hourly[], f: (h: Hourly) => number) => list.reduce((a, h) => a + f(h), 0);
  const requests = sum(current, (h) => h.requests);
  const cost = sum(current, (h) => h.cost);
  const day = lastDay(hours, now, (h) => h.requests);
  return (
    <div className="flex flex-col gap-3">
      <DefinitionGrid
        columns={2}
        items={[
          {
            label: 'Requests, last 24 h',
            value: (
              <>
                {formatCount(requests)}
                <DeltaText value={requests} before={sum(previous, (h) => h.requests)} />
              </>
            ),
          },
          {
            label: 'Spend, last 24 h',
            value: (
              <>
                {formatMoney(cost)}
                <DeltaText value={cost} before={sum(previous, (h) => h.cost)} />
              </>
            ),
          },
        ]}
      />
      <TimeChart
        kind="bars"
        title="Requests per hour, last 24 hours"
        labels={day.starts.map((s) => hourLabel(s, timeZone))}
        series={[{ name: 'Requests', values: day.values, tone: 'accent' }]}
        height={120}
        formatValue={(v) => formatCount(v)}
        emptyLabel="No requests in the last 24 hours"
      />
    </div>
  );
}

function per(limit: LimitCounter): string {
  if (limit.unit === 'concurrency') return 'at once';
  const p = limit.per ?? '';
  if (p === '1m' || p === '60s') return 'per minute';
  if (p === '1h') return 'per hour';
  if (p === '1s') return 'per second';
  if (p === '1d' || p === '24h') return 'per day';
  return p ? `per ${p}` : '';
}

/** The key's rate limits with what is used of each now. */
export function KeyLimits({
  name,
  declared,
}: {
  name: string;
  declared: [string, { unit?: string; limit?: number; per?: string } | undefined][];
}) {
  const slot = useKeysContext((s) => s.limits[name]);
  const counters = slot?.data ?? [];
  const counted = new Set(counters.map((c) => c.name));
  const rest = declared.filter(([n]) => !counted.has(n));
  if (counters.length === 0 && rest.length === 0)
    return <p className="text-muted">None on the key. Team and project limits still apply.</p>;
  return (
    <ul className="flex flex-col gap-2.5">
      {counters.map((c) => {
        const used = c.used ?? c.limit - c.remaining;
        const ratio = c.limit > 0 ? used / c.limit : 0;
        return (
          <li key={`${c.name}/${c.partition ?? ''}`} className="flex flex-col gap-1">
            <div className="flex flex-wrap items-baseline gap-x-2 text-fg">
              <span>
                {formatCount(c.limit)} {c.unit} {per(c)}
              </span>
              <span className="mono text-caption text-dim">{c.name}</span>
              <span className="mono ml-auto text-caption text-muted">
                {formatCount(used)} used{c.resets_at ? ` · full again ${formatRelative(c.resets_at)}` : ''}
              </span>
            </div>
            <Meter
              value={used}
              max={c.limit}
              tone={ratio >= 1 ? 'error' : ratio >= 0.8 ? 'warn' : 'accent'}
              label={`${c.name}: ${formatPercent(ratio, 0)} used`}
            />
          </li>
        );
      })}
      {rest.map(([n, l]) => (
        <li key={n} className="text-fg">
          {formatCount(l?.limit)} {l?.unit ?? ''} per {l?.per === '1m' ? 'minute' : (l?.per ?? '—')}
          <span className="mono ml-2 text-caption text-dim">{n}</span>
          <span className="ml-2 text-caption text-muted">nothing used yet</span>
        </li>
      ))}
    </ul>
  );
}

/** The addresses the key was used from over 24 hours, with their country. */
export function TopIps({ name }: { name: string }) {
  const slot = useKeysContext((s) => s.ips[name]);
  const logHref = useScopedHref('/request-log/', { key: name });
  if (!slot || (slot.status === 'loading' && !slot.data)) return <LoadingRows rows={2} />;
  if (slot.error) return <ErrorState error={slot.error} subject="Source addresses" size="sm" />;
  const ips = (slot.data ?? []).slice(0, 8);
  if (ips.length === 0) return <EmptyState size="sm" title="No requests in the last 24 hours" />;
  const total = ips.reduce((a, i) => a + i.requests, 0);
  return (
    <>
      <Table label="Top source addresses, last 24 hours">
        <thead>
          <tr>
            <Th>Address</Th>
            <Th>Country</Th>
            <Th numeric>Requests</Th>
            <Th numeric>Share</Th>
            <Th numeric>Errors</Th>
          </tr>
        </thead>
        <tbody>
          {ips.map((i) => (
            <Tr key={i.ip}>
              <Td mono>{i.ip}</Td>
              <Td muted>{i.country ?? 'Unknown'}</Td>
              <Td numeric>{formatCount(i.requests)}</Td>
              <Td numeric>{formatPercent(total > 0 ? i.requests / total : 0, 0)}</Td>
              <Td numeric>{formatCount(i.errors)}</Td>
            </Tr>
          ))}
        </tbody>
      </Table>
      <Link href={logHref} className="mt-2 inline-block text-caption text-accent hover:underline">
        See this key&apos;s requests
      </Link>
    </>
  );
}
