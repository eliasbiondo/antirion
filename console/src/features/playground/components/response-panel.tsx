'use client';

import Link from 'next/link';
import { useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge, Dot, StatusCode } from '@/components/ui/badge';
import { buttonClass, Button } from '@/components/ui/button';
import { Icon } from '@/components/ui/icon';
import { Tabs } from '@/components/ui/layout';
import { CopyButton } from '@/components/ui/misc';
import { ProviderMark } from '@/components/ui/marks';
import { EmptyState } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { ApiError, describeError } from '@/lib/api/client';
import { cn } from '@/lib/cn';
import { formatCount, formatLatency, formatMoney, plural } from '@/lib/format';
import { useProviderType } from '@/stores/provider-types';

import { readout, type Run } from '../use-send';

type View = 'answer' | 'headers' | 'raw';

/** The decision headers in reading order; any other the gateway sent follows them. */
const HEADER_ORDER = [
  'x-antirion-route',
  'x-antirion-route-path',
  'x-antirion-provider',
  'x-antirion-model',
  'x-antirion-deployment',
  'x-antirion-attempts',
  'x-antirion-response-path',
  'x-antirion-upstream-status',
  'x-antirion-cache',
  'x-antirion-cost',
  'x-antirion-overhead-ms',
  'x-antirion-log-id',
  'x-request-id',
];

function sortedHeaders(headers: Record<string, string>): [string, string][] {
  const rank = (k: string) => {
    const i = HEADER_ORDER.indexOf(k);
    return i < 0 ? HEADER_ORDER.length : i;
  };
  return Object.entries(headers).sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b));
}

function failure(error: unknown): string {
  if (error instanceof ApiError && error.status === 404)
    return 'The playground is off on this gateway, or the project is no longer there.';
  if (error instanceof ApiError && error.status === 403)
    return "You don't have permission to send through the playground in this project.";
  return describeError(error);
}

function Figure({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 bg-raised px-4 py-2.5">
      <dt className="text-caption text-muted">{label}</dt>
      <dd className="mono mt-0.5 truncate text-body font-semibold text-fg tabular-nums">{value}</dd>
    </div>
  );
}

/** The answer, how the gateway routed it, what it cost, and the way to its record. */
export function ResponsePanel({ run, onContinue }: { run: Run | null; onContinue: (() => void) | null }) {
  const [view, setView] = useState<View>('answer');
  const typeOf = useProviderType();
  const logHref = useScopedHref('/request-log/', run?.head?.logId ? { request: run.head.logId } : {});
  if (!run) {
    return (
      <EmptyState
        size="sm"
        title="Nothing sent yet"
        description="Send a request to see the answer, its route and its cost."
      />
    );
  }
  const { head, answer } = run;
  const figures = readout(run);
  const live = run.status === 'sending' || run.status === 'streaming';
  const provider = head?.headers['x-antirion-provider'];
  const deployment = head?.headers['x-antirion-deployment'];
  const attempts = Number(head?.headers['x-antirion-attempts'] ?? '1');
  const raw =
    run.events.length > 0
      ? run.events.map((e) => JSON.stringify(e.event ? { event: e.event, data: e.data } : e.data)).join('\n')
      : run.body !== null
        ? JSON.stringify(run.body, null, 2)
        : '';
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        {head ? <StatusCode code={head.status} /> : null}
        {live ? (
          <span className="flex items-center gap-1.5 text-caption text-muted" role="status">
            <Dot tone="accent" className="animate-pulse" />
            {run.status === 'sending' ? 'Sending' : 'Streaming'}
          </span>
        ) : run.status === 'stopped' ? (
          <Badge tone="warn">Stopped</Badge>
        ) : answer.error ? (
          <Badge tone="error">Stream failed</Badge>
        ) : answer.finish ? (
          <span className="text-caption text-muted">Finished: {answer.finish}</span>
        ) : null}
        {provider ? (
          <span className="flex min-w-0 items-center gap-1.5 text-caption text-muted">
            <ProviderMark type={typeOf(provider)} />
            <span className="mono truncate text-fg">{deployment ?? provider}</span>
            {attempts > 1 ? <Badge tone="warn">{plural(attempts, 'attempt')}</Badge> : null}
          </span>
        ) : null}
        <span className="ml-auto flex items-center gap-2">
          {head?.logId ? (
            <Link href={logHref} className={buttonClass('secondary', 'sm')}>
              <Icon name="list" size={14} />
              View in request log
            </Link>
          ) : null}
        </span>
      </div>
      {run.status === 'failed' ? (
        <div
          role="alert"
          className="flex items-start gap-2.5 rounded-control border border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-error)_8%,transparent)] px-3 py-2.5"
        >
          <Icon name="alertCircle" size={16} className="mt-0.5 shrink-0 text-error" />
          <div className="min-w-0">
            <p className="text-ui font-semibold text-fg">The gateway didn&apos;t send it</p>
            <p className="mt-0.5 text-ui text-muted">{failure(run.error)}</p>
          </div>
        </div>
      ) : (
        <dl
          aria-label="Usage, cost and timing"
          className="grid grid-cols-2 gap-px border-y border-line bg-line sm:grid-cols-4"
        >
          <Figure
            label="Tokens in · out"
            value={
              figures.input === null && figures.output === null
                ? '—'
                : `${figures.input === null ? '—' : formatCount(figures.input)} · ${figures.output === null ? '—' : formatCount(figures.output)}`
            }
          />
          <Figure label="Cost" value={figures.cost === null ? '—' : formatMoney(figures.cost)} />
          <Figure
            label="Latency"
            value={
              figures.latency === null
                ? '—'
                : `${figures.recorded ? '' : '~'}${formatLatency(figures.latency)}`
            }
          />
          <Figure
            label="Time to first token"
            value={
              figures.ttft === null ? (run.input.stream ? '—' : 'Not streamed') : formatLatency(figures.ttft)
            }
          />
        </dl>
      )}
      {run.status !== 'failed' ? (
        <div className="flex min-w-0 flex-col">
          <Tabs<View>
            label="Answer views"
            idPrefix="pg-view"
            className="!px-0"
            value={view}
            onChange={setView}
            items={[
              { value: 'answer', label: 'Answer' },
              {
                value: 'headers',
                label: 'Decision headers',
                count: head ? Object.keys(head.headers).length : undefined,
              },
              { value: 'raw', label: 'Raw JSON' },
            ]}
          />
          <div role="tabpanel" aria-labelledby={`pg-view-${view}`} className="pt-3">
            {view === 'answer' ? (
              <Answer run={run} live={live} />
            ) : view === 'headers' ? (
              head ? (
                <Table label="Decision headers">
                  <thead>
                    <tr>
                      <Th>Header</Th>
                      <Th>Value</Th>
                    </tr>
                  </thead>
                  <tbody>
                    {sortedHeaders(head.headers).map(([k, v]) => (
                      <Tr key={k}>
                        <Td mono muted className="w-[42%] break-all">
                          {k}
                        </Td>
                        <Td mono className="break-all">
                          {v}
                        </Td>
                      </Tr>
                    ))}
                  </tbody>
                </Table>
              ) : (
                <p className="text-ui text-muted">The headers arrive with the answer.</p>
              )
            ) : raw ? (
              <div className="relative">
                <div className="absolute top-2 right-2">
                  <CopyButton value={raw} label="Copy the raw answer" />
                </div>
                <pre className="mono max-h-[420px] overflow-auto rounded-control border border-line bg-canvas p-3 pr-10 text-caption whitespace-pre-wrap text-fg">
                  {raw}
                </pre>
              </div>
            ) : (
              <EmptyState size="sm" title="Nothing received yet" />
            )}
          </div>
        </div>
      ) : null}
      <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line pt-3 text-caption text-muted">
        <span>
          {head ? (
            <>
              Sent with the short-lived key <span className="mono text-fg">{head.key}</span> in project{' '}
              <span className="mono text-fg">{run.input.project}</span>
            </>
          ) : (
            <>
              Sent in project <span className="mono text-fg">{run.input.project}</span>
            </>
          )}
        </span>
        {onContinue && run.status === 'done' && answer.text ? (
          <Button size="sm" variant="ghost" icon="messages" onClick={onContinue}>
            Continue the conversation
          </Button>
        ) : null}
      </div>
    </div>
  );
}

function Answer({ run, live }: { run: Run; live: boolean }) {
  const { answer } = run;
  if (answer.vectors) {
    return (
      <div className="flex flex-col gap-2 text-ui text-fg">
        <p>
          {plural(answer.vectors.count, 'vector')} of {formatCount(answer.vectors.dimensions)} dimensions.
        </p>
        <p className="mono text-caption text-muted">
          [{answer.vectors.sample.map((v) => v.toFixed(4)).join(', ')}
          {answer.vectors.dimensions > answer.vectors.sample.length ? ', …' : ''}]
        </p>
      </div>
    );
  }
  const streamError = answer.error ? (
    <p className="flex items-start gap-2 text-ui text-fg">
      <Icon name="alertCircle" size={16} className="mt-0.5 shrink-0 text-error" />
      {answer.text ? `The stream failed after this part of the answer: ${answer.error}` : answer.error}
    </p>
  ) : null;
  // A stream that fails after words arrived keeps them, with the error under them.
  if (answer.error && !answer.text) return streamError;
  if (answer.error) {
    return (
      <div className="flex flex-col gap-3">
        <div className="max-h-[480px] overflow-auto text-body leading-6 whitespace-pre-wrap text-fg">
          {answer.text}
        </div>
        {streamError}
      </div>
    );
  }
  if (!answer.text) {
    return (
      <p className="text-ui text-muted">
        {live
          ? 'Waiting for the first words…'
          : run.status === 'stopped'
            ? 'Stopped before any words arrived.'
            : 'The answer has no text.'}
      </p>
    );
  }
  return (
    <div
      aria-live={live ? 'polite' : undefined}
      className={cn(
        'max-h-[480px] overflow-auto text-body leading-6 whitespace-pre-wrap text-fg',
        live &&
          'after:ml-0.5 after:inline-block after:h-4 after:w-1.5 after:translate-y-0.5 after:animate-pulse after:bg-accent',
      )}
    >
      {answer.text}
    </div>
  );
}
