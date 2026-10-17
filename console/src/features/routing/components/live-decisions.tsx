'use client';

import Link from 'next/link';
import { useEffect, useEffectEvent, useRef, useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge, type Tone } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Panel } from '@/components/ui/layout';
import { ModelMark } from '@/components/ui/marks';
import { EmptyState, ErrorState } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { API_BASE } from '@/lib/api/client';
import type { LogRecord } from '@/lib/api/types';
import { formatLatency } from '@/lib/format';
import { formatClock } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { routingApi } from '../api';
import { decisionMs, type Outcome, outcomeOf, ruleOfRecord, whyOf } from '../insights';
import { describePredicate, type Rule } from '../types';

const KEEP = 50;
const POLL_MS = 3000;

const OUTCOME: Record<Outcome, { label: string; tone: Tone }> = {
  ok: { label: 'Served', tone: 'success' },
  fallback: { label: 'Fell back', tone: 'warn' },
  retry: { label: 'Retried', tone: 'info' },
  block: { label: 'Blocked', tone: 'error' },
  error: { label: 'Failed', tone: 'error' },
};

/**
 * The route's newest decisions as they happen (the log's live tail, or polling when the gateway
 * refuses a tail), with the rule each matched and why it went where it did; paused, it holds still.
 */
export function LiveDecisions({ route, rules }: { route: string; rules: Rule[] }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const logHref = useScopedHref('/request-log/');
  const [playing, setPlaying] = useState(true);
  const [rows, setRows] = useState<LogRecord[]>([]);
  const [mode, setMode] = useState<'stream' | 'poll'>(typeof EventSource === 'undefined' ? 'poll' : 'stream');
  const [error, setError] = useState<unknown>(null);
  const newest = useRef<string>('');

  const add = useEffectEvent((records: LogRecord[]) => {
    const mine = records.filter((r) => r.route === route);
    for (const r of records) if (r.started_at > newest.current) newest.current = r.started_at;
    if (mine.length === 0) return;
    setRows((prev) => {
      const seen = new Set(prev.map((r) => r.id));
      return [...mine.filter((r) => !seen.has(r.id)), ...prev]
        .sort((a, b) => b.started_at.localeCompare(a.started_at))
        .slice(0, KEEP);
    });
  });

  // The first rows: the last 15 minutes of the route's records.
  useEffect(() => {
    let live = true;
    newest.current = new Date(Date.now() - 15 * 60_000).toISOString();
    routingApi.recent(newest.current, 200).then(
      (records) => live && add(records),
      (e: unknown) => live && setError(e),
    );
    return () => {
      live = false;
    };
  }, [route]);

  useEffect(() => {
    if (!playing || mode !== 'stream') return;
    const source = new EventSource(`${API_BASE}/logs:tail`, { withCredentials: true });
    let opened = false;
    source.onopen = () => {
      opened = true;
    };
    source.addEventListener('log', (e) => {
      try {
        add([JSON.parse((e as MessageEvent<string>).data) as LogRecord]);
      } catch {
        // A record that does not parse is skipped.
      }
    });
    source.onerror = () => {
      if (!opened) {
        source.close();
        setMode('poll');
      }
    };
    return () => source.close();
  }, [playing, mode]);

  useEffect(() => {
    if (!playing || mode !== 'poll') return;
    const timer = window.setInterval(() => {
      routingApi.recent(newest.current, 100).then(add, () => undefined);
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [playing, mode]);

  const conditionOf = (index: number | 'default') =>
    index === 'default' ? null : rules[index] ? describePredicate(rules[index].when) : null;

  return (
    <Panel
      title="Live decisions"
      subtitle={`Route ${route} · ${playing ? (mode === 'stream' ? 'streaming' : 'every 3 seconds') : 'paused'}`}
      actions={
        <>
          <Link href={logHref} className="text-caption text-accent hover:underline">
            Open the request log
          </Link>
          <Button size="sm" icon={playing ? 'pause' : 'play'} onClick={() => setPlaying((p) => !p)}>
            {playing ? 'Pause' : 'Play'}
          </Button>
        </>
      }
      bodyClassName="px-0 pb-0"
    >
      {error ? <ErrorState error={error} subject="Recent requests" size="sm" className="mx-5 mb-4" /> : null}
      {!error && rows.length === 0 ? (
        <EmptyState
          size="sm"
          title="No decisions yet"
          description={`Requests that use route ${route} show here as they are routed.`}
        />
      ) : null}
      {rows.length > 0 ? (
        <Table label="Recent routing decisions">
          <thead>
            <tr>
              <Th>Time</Th>
              <Th>Team</Th>
              <Th>Rule and why</Th>
              <Th>Deployment</Th>
              <Th>Outcome</Th>
              <Th numeric>Decision</Th>
              <Th numeric>Total</Th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => {
              const rule = ruleOfRecord(r, route);
              const outcome = OUTCOME[outcomeOf(r)];
              const decision = decisionMs(r);
              const total = typeof r.timing.total_ms === 'number' ? r.timing.total_ms : null;
              return (
                <Tr key={r.id}>
                  <Td mono muted>
                    {formatClock(r.started_at, timeZone)}
                  </Td>
                  <Td>{r.team ?? '—'}</Td>
                  <Td>
                    <span className="block font-medium text-fg">{rule?.name ?? '—'}</span>
                    <span className="block max-w-[420px] text-caption text-muted">
                      {whyOf(r, rule, rule ? conditionOf(rule.index) : null)}
                    </span>
                  </Td>
                  <Td mono>
                    {r.deployment ? (
                      <span className="flex items-center gap-1.5">
                        <ModelMark model={null} upstream={r.upstream_model} className="text-muted" />
                        {r.deployment}
                      </span>
                    ) : (
                      '—'
                    )}
                  </Td>
                  <Td>
                    <Badge tone={outcome.tone}>{outcome.label}</Badge>
                  </Td>
                  <Td numeric mono>
                    {decision === null ? '—' : `${decision.toFixed(2)} ms`}
                  </Td>
                  <Td numeric mono>
                    {total === null ? '—' : formatLatency(total)}
                  </Td>
                </Tr>
              );
            })}
          </tbody>
        </Table>
      ) : null}
    </Panel>
  );
}
