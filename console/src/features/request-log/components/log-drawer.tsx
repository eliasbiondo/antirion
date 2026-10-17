'use client';

import { Fragment, useEffect, useRef, useState, type ReactNode } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';

import { Badge, Dot, StatusCode } from '@/components/ui/badge';
import { Button, buttonClass } from '@/components/ui/button';
import { ModelMark } from '@/components/ui/marks';
import { usePermission } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { Tabs } from '@/components/ui/layout';
import { CopyButton, CopyLinkButton } from '@/components/ui/misc';
import { Drawer } from '@/components/ui/overlay';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { api } from '@/lib/api/client';
import type { LogRecord } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { capitalize, formatCount, formatLatency, formatMoney, formatTokens } from '@/lib/format';
import { formatClock, formatDate, zoneLabel } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useShell } from '@/stores/shell';

import { useLogs } from '../store';
import { requestMessages, responseView, storedBody } from '../transcript';
import { cacheStatus, costUsd, flags, latencyMs, ttftMs } from '../types';
import { CodeBlock as Json, MessagesTab, RequestTab, ResponseTab } from './drawer-content';
import { ReplayDialog } from './replay-dialog';
import { EraseEndUserButton } from './erase-end-user';
import { RequestLedger } from '@/features/ledger/components/request-ledger';

type Tab = 'overview' | 'messages' | 'request' | 'response' | 'trace' | 'guardrails';
const TABS: Tab[] = ['overview', 'messages', 'request', 'response', 'trace', 'guardrails'];

function Section({ title, aside, children }: { title: string; aside?: ReactNode; children: ReactNode }) {
  return (
    <section>
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <h3 className="text-ui font-semibold text-fg">{title}</h3>
        {aside ? <span className="text-caption text-muted">{aside}</span> : null}
      </div>
      {children}
    </section>
  );
}

/** The prototype's key-value list: labels left in sans, values right in mono where they are codes. */
function Facts({ items }: { items: { label: string; value: ReactNode; mono?: boolean }[] }) {
  return (
    <dl className="grid grid-cols-[minmax(120px,160px)_minmax(0,1fr)] gap-x-4 border-y border-line max-sm:grid-cols-1">
      {items.map((item, i) => (
        <div key={item.label} className={cn('contents', i > 0 && '[&>*]:border-t [&>*]:border-line')}>
          <dt className="py-2 text-caption text-muted max-sm:pb-0">{item.label}</dt>
          <dd
            className={cn(
              'min-w-0 py-2 text-ui break-words text-fg max-sm:border-t-0 max-sm:pt-0.5',
              item.mono !== false && 'mono tabular-nums',
            )}
          >
            {item.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** "São Paulo, SP, BR" from the record's GeoIP lookup, when the gateway has a database. */
function location(r: LogRecord): string | null {
  const g = r.client_geo;
  if (!g) return null;
  const parts = [g.city, g.region, g.country].filter((p): p is string => Boolean(p));
  return parts.length ? parts.join(', ') : null;
}

/** The deployments tried, in order, when more than one was: "a → b". */
function pathTaken(r: LogRecord): string | null {
  const deployments = r.attempts.map((a) => a.deployment);
  const distinct = deployments.filter((d, i) => d !== deployments[i - 1]);
  return distinct.length > 1 ? distinct.join(' → ') : null;
}

/** The record's cache outcome in words, with the semantic similarity when there was one. */
function cacheText(r: LogRecord): string {
  const status = capitalize(cacheStatus(r).replace(/_/g, ' '));
  const similarity = num(r.cache.similarity);
  const layer = typeof r.cache.layer === 'string' ? r.cache.layer : null;
  return [
    status,
    layer ? `${layer} layer` : null,
    similarity !== null ? `similarity ${similarity.toFixed(3)}` : null,
  ]
    .filter(Boolean)
    .join(' · ');
}

/** The endpoints the request's key may call, read from the key when the reader may see keys. */
function useKeyScopes(name: string | null): string | null {
  const canRead = usePermission('keys:read');
  const [scopes, setScopes] = useState<{ name: string; text: string } | null>(null);
  useEffect(() => {
    if (!name || !canRead) return;
    let live = true;
    api
      .get('/keys/{name}', { path: { name } })
      .then((k) => {
        const endpoints = k.spec.endpoints;
        if (live) setScopes({ name, text: endpoints?.length ? endpoints.join(', ') : 'every endpoint' });
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [name, canRead]);
  return scopes?.name === name ? scopes.text : null;
}

/** The external trace viewer's link for the record, from the gateway's template. */
function traceLink(template: string | null | undefined, r: LogRecord): string | null {
  if (!template || !r.trace_id) return null;
  return template
    .replace(/\{trace_id\}/g, encodeURIComponent(r.trace_id))
    .replace(/\{span_id\}/g, encodeURIComponent(r.span_id));
}

function curlFor(record: LogRecord, dataPlaneUrl: string): string {
  const body = (record.content?.request as { body?: unknown } | undefined)?.body;
  return [
    `curl ${dataPlaneUrl.replace(/\/+$/, '')}${record.endpoint}`,
    `  -H "Authorization: Bearer $ANTIRION_API_KEY"`,
    `  -H "Content-Type: application/json"`,
    body !== undefined ? `  -d '${JSON.stringify(body).replace(/'/g, "'\\''")}'` : '',
  ]
    .filter(Boolean)
    .join(' \\\n');
}

function num(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

const PHASES: { key: string; label: string; bar: string }[] = [
  { key: 'overhead_ms', label: 'Gateway', bar: 'fill-accent' },
  { key: 'wait_ms', label: 'Waiting for capacity', bar: 'fill-warn' },
  { key: 'external_ms', label: 'Guardrails and extensions', bar: 'fill-info' },
  { key: 'upstream_ms', label: 'Provider', bar: 'fill-success' },
  { key: 'relay_ms', label: 'Streaming to the client', bar: 'fill-dim' },
];

/** Where the request's time went, as bars on one timeline (the prototype's latency breakdown). */
function LatencyBreakdown({ record }: { record: LogRecord }) {
  const total = latencyMs(record);
  const phases = PHASES.map((p) => ({ ...p, ms: num(record.timing[p.key]) ?? 0 })).filter((p) => p.ms > 0);
  if (!total || phases.length === 0) return null;
  const sum = phases.reduce((a, p) => a + p.ms, 0) || 1;
  const starts = phases.map((_, i) => phases.slice(0, i).reduce((a, p) => a + p.ms, 0));
  return (
    <Section title="Latency breakdown" aside={`${formatLatency(total)} in total`}>
      <ul className="flex flex-col gap-1.5">
        {phases.map((p, i) => {
          const x = ((starts[i] ?? 0) / sum) * 100;
          const w = Math.max(0.6, (p.ms / sum) * 100);
          return (
            <li
              key={p.key}
              className="grid grid-cols-[minmax(0,180px)_minmax(0,1fr)_72px] items-center gap-3 text-ui max-sm:grid-cols-[minmax(0,1fr)_72px]"
            >
              <span className="truncate text-muted">{p.label}</span>
              <svg
                viewBox="0 0 100 8"
                preserveAspectRatio="none"
                className="block h-2 w-full max-sm:hidden"
                aria-hidden="true"
              >
                <rect x="0" y="0" width="100" height="8" rx="1" className="fill-sunken" />
                <rect x={x} y="0" width={w} height="8" rx="1" className={p.bar} />
              </svg>
              <span className="mono text-right text-fg tabular-nums">{formatLatency(p.ms)}</span>
            </li>
          );
        })}
      </ul>
    </Section>
  );
}

/** `upstream_ms` → "Provider"; `ttft_ms` → "Time to first token"; others in words. */
function timingLabel(key: string): string {
  const known: Record<string, string> = {
    total_ms: 'Total',
    ttft_ms: 'Time to first token',
    overhead_ms: 'Gateway',
    wait_ms: 'Waiting for capacity',
    external_ms: 'Guardrails and extensions',
    upstream_ms: 'Provider',
    relay_ms: 'Streaming to the client',
    upstream_ttft_ms: "Provider's first token",
    tpot_ms: 'Time per output token',
    ttfb_overhead_ms: 'Gateway before the first byte',
  };
  return known[key] ?? capitalize(key.replace(/_ms$/, '').replace(/_/g, ' '));
}

/** A timing value: milliseconds formatted, a stage that did not happen as "—". */
function timingValue(value: unknown): string {
  const ms = num(value);
  if (ms !== null) return formatLatency(ms);
  if (typeof value === 'string' || typeof value === 'boolean') return String(value);
  return '—';
}

/**
 * The record's timing as rows: each measure, and the pipeline's stages (`stage`) one per row,
 * milliseconds formatted; a measure that did not happen reads "—".
 */
function TimingTable({ timing }: { timing: Record<string, unknown> }) {
  const rows = Object.entries(timing).flatMap(([key, value]): [string, string, string][] =>
    value !== null && typeof value === 'object' && !Array.isArray(value)
      ? Object.entries(value as Record<string, unknown>).map(([stage, v]) => [
          `${key}.${stage}`,
          // `stage_ms.accept` reads "Accept stage"; another group keeps its name in front.
          key === 'stage_ms' || key === 'stages'
            ? `${capitalize(stage.replace(/_/g, ' '))} stage`
            : `${capitalize(key.replace(/_ms$/, '').replace(/_/g, ' '))}: ${stage.replace(/_/g, ' ')}`,
          timingValue(v),
        ])
      : [[key, timingLabel(key), timingValue(value)]],
  );
  if (rows.length === 0) return null;
  return (
    <Section
      title="Timing"
      aside={<CopyButton value={JSON.stringify(timing, null, 2)} label="Copy the timing" />}
    >
      <dl className="grid grid-cols-[minmax(0,1fr)_auto] text-ui">
        {rows.map(([id, label, shown]) => (
          <Fragment key={id}>
            <dt className="border-b border-line py-1.5 pr-4 text-muted">{label}</dt>
            <dd className="mono border-b border-line py-1.5 text-right text-fg tabular-nums">{shown}</dd>
          </Fragment>
        ))}
      </dl>
    </Section>
  );
}

/** The error a request ended with, in words first and its code as a muted chip. */
function Failure({ record }: { record: LogRecord }) {
  if (!record.error_message && !record.code) return null;
  const model = record.model ?? record.requested_model ?? 'this model';
  const title =
    record.code === 'no_healthy_deployment'
      ? 'No healthy deployment'
      : record.code
        ? capitalize(record.code.replace(/_/g, ' '))
        : `Failed with ${record.status_code}`;
  const body =
    record.code === 'no_healthy_deployment'
      ? `Every deployment of ${model} was failing when this request arrived.`
      : record.error_message;
  return (
    <div
      role="status"
      className="flex items-start gap-2.5 rounded-control border border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-error)_8%,transparent)] px-3 py-2.5"
    >
      <Dot tone="error" className="mt-1.5" />
      <div className="min-w-0 flex-1">
        <p className="text-ui font-semibold text-fg">{title}</p>
        {body ? <p className="mt-0.5 text-ui text-muted">{body}</p> : null}
      </div>
      {record.code ? (
        <Badge mono className="shrink-0">
          {record.code}
        </Badge>
      ) : null}
    </div>
  );
}

function money(value: unknown): number | null {
  const n = typeof value === 'string' ? Number(value) : typeof value === 'number' ? value : NaN;
  return Number.isFinite(n) ? n : null;
}

function guardrailVerdict(g: Record<string, unknown>): string {
  return typeof g.verdict === 'string' ? g.verdict : typeof g.action === 'string' ? g.action : 'pass';
}

/** Replay: offered to operators who may send through the playground and read content. */
function ReplayButton({ record, onOpenRecord }: { record: LogRecord; onOpenRecord: (id: string) => void }) {
  const canSend = usePermission('playground:send');
  const canRead = usePermission('logs:read_content');
  const [open, setOpen] = useState(false);
  if (!canSend || !canRead) return null;
  const stored = storedBody(record.content?.request);
  const body =
    stored &&
    !stored.truncated &&
    typeof stored.body === 'object' &&
    stored.body !== null &&
    !Array.isArray(stored.body)
      ? (stored.body as Record<string, unknown>)
      : null;
  const reason = body
    ? null
    : stored?.truncated
      ? 'The stored body was cut at the logging limit, so it cannot be sent again.'
      : 'The request body was not stored, so it cannot be sent again.';
  return (
    <>
      <Tooltip content={reason ?? 'Send this request again through the gateway'} side="bottom">
        <span className="inline-flex [&>button:disabled]:pointer-events-none">
          <Button size="sm" icon="refresh" disabled={!body} onClick={() => setOpen(true)}>
            Replay
          </Button>
        </span>
      </Tooltip>
      {body ? (
        <ReplayDialog
          record={record}
          body={body}
          open={open}
          onClose={() => setOpen(false)}
          onOpenRecord={onOpenRecord}
        />
      ) : null}
    </>
  );
}

/** One request: its outcome, route, cost and time, content for content readers, attempts and guardrails. */
export function LogDrawer({
  id,
  onClose,
  onOpen,
}: {
  id: string | null;
  onClose: () => void;
  /** Opens another record (a replay's new request). */
  onOpen?: (id: string) => void;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const detail = useLogs((s) => s.detail);
  const loadDetail = useLogs((s) => s.loadDetail);
  const dataPlaneUrl = useShell((s) => s.status?.data_plane_url ?? null);
  const traceViewer = useShell((s) => s.status?.trace_viewer_url ?? null);
  const canReadContent = usePermission('logs:read_content');
  // The tab is in the URL, so a reload or a copied link reopens it; another request opens on its
  // overview.
  const [tabParam, setTabParam] = useQueryParam('tab');
  const tab: Tab = TABS.includes(tabParam as Tab) ? (tabParam as Tab) : 'overview';
  const setTab = (next: Tab) => setTabParam(next === 'overview' ? null : next);
  const opened = useRef(id);
  useEffect(() => {
    if (opened.current !== id && opened.current !== null && tabParam) setTabParam(null);
    opened.current = id;
  }, [id, tabParam, setTabParam]);

  useEffect(() => {
    if (id) void loadDetail(id);
  }, [id, loadDetail]);

  // `id` is the record id, or the request id clients see, which the read resolves.
  const r = detail.data && (detail.data.id === id || detail.data.request_id === id) ? detail.data : null;
  const flagged = r ? flags(r).length > 0 : false;
  const cost: Record<string, unknown> | undefined = r?.cost;
  const scopes = useKeyScopes(r?.key ?? null);
  const messageCount =
    r && canReadContent
      ? requestMessages(storedBody(r.content?.request)?.body).length +
        responseView(storedBody(r.content?.response)?.body).messages.length
      : 0;
  const trace = r ? traceLink(traceViewer, r) : null;

  return (
    <Drawer
      open={id !== null}
      onClose={onClose}
      width="lg"
      title={
        r ? (
          <span className="flex min-w-0 items-center gap-2">
            <StatusCode code={r.status_code} />
            <span className="mono truncate text-ui">{r.request_id}</span>
          </span>
        ) : (
          'Request'
        )
      }
      subtitle={
        r ? (
          <span className="flex flex-col gap-2">
            <span className="text-caption">
              <span className="mono">
                {formatDate(r.started_at, timeZone)}, {formatClock(r.started_at, timeZone)}.
                {/\.(\d{3})/.exec(r.started_at)?.[1] ?? '000'} {zoneLabel(timeZone)}
              </span>{' '}
              · took {formatLatency(latencyMs(r))}
              {r.replay_of ? (
                <>
                  {' '}
                  ·{' '}
                  <button
                    type="button"
                    className="rounded-xs text-accent hover:underline"
                    onClick={() => (r.replay_of && onOpen ? onOpen(r.replay_of) : undefined)}
                  >
                    replay of an earlier request
                  </button>
                </>
              ) : null}
            </span>
            <span className="flex flex-wrap items-center gap-1.5">
              <Badge mono>
                <ModelMark model={r.model ?? r.requested_model} upstream={r.upstream_model} size="xs" />
                {r.model ?? r.requested_model ?? '—'}
              </Badge>
              {r.key ? <Badge mono>{r.key}</Badge> : null}
              <Badge mono>{formatMoney(costUsd(r))}</Badge>
              {r.team ? <Badge>{r.team}</Badge> : null}
            </span>
          </span>
        ) : undefined
      }
      headerActions={
        <>
          {r && onOpen ? <ReplayButton record={r} onOpenRecord={onOpen} /> : null}
          <CopyLinkButton />
        </>
      }
    >
      {detail.status === 'loading' && !r ? <LoadingRows rows={6} label="Loading the request" /> : null}
      {detail.error && !r ? <ErrorState error={detail.error} subject="This request" size="sm" /> : null}
      {r ? (
        <div className="-mx-5 -mt-4">
          <Tabs<Tab>
            label="Request details"
            value={tab}
            onChange={setTab}
            className="px-3"
            items={[
              { value: 'overview', label: 'Overview' },
              { value: 'messages', label: 'Messages', count: messageCount > 0 ? messageCount : undefined },
              { value: 'request', label: 'Request' },
              { value: 'response', label: 'Response' },
              { value: 'trace', label: 'Trace', count: r.attempt_count > 1 ? r.attempt_count : undefined },
              {
                value: 'guardrails',
                label: flagged ? 'Guardrails · flagged' : 'Guardrails',
                count: r.guardrails.length > 0 ? r.guardrails.length : undefined,
              },
            ]}
          />
          <div className="flex flex-col gap-6 px-5 py-5">
            {tab === 'overview' ? (
              <>
                <Failure record={r} />
                <Section title="Request">
                  <Facts
                    items={[
                      { label: 'Endpoint', value: `${r.method} ${r.endpoint}` },
                      {
                        label: 'Team · project',
                        value: `${r.team ?? '—'} · ${r.project ?? '—'}`,
                        mono: false,
                      },
                      {
                        label: 'API key',
                        value: (
                          <>
                            {r.key ?? '—'}
                            {scopes ? (
                              <span className="font-sans text-caption text-muted"> · may call {scopes}</span>
                            ) : null}
                          </>
                        ),
                      },
                      {
                        label: 'End user',
                        value: (
                          <>
                            {r.end_user ?? '—'}
                            {r.end_user ? (
                              <EraseEndUserButton endUser={r.end_user} onErased={onClose} />
                            ) : null}
                          </>
                        ),
                      },
                      {
                        label: 'Client',
                        value: (
                          <>
                            {r.client_ip ?? '—'}
                            {location(r) ? (
                              <span className="font-sans text-muted"> · {location(r)}</span>
                            ) : null}
                          </>
                        ),
                      },
                      { label: 'Route', value: r.route_path ?? r.route ?? '—' },
                      {
                        label: 'Deployment',
                        value: r.deployment
                          ? `${r.deployment}${r.upstream_model ? ` → ${r.upstream_model}` : ''}`
                          : '—',
                      },
                      ...(pathTaken(r) ? [{ label: 'Fallback path', value: pathTaken(r) }] : []),
                      { label: 'Cache', value: cacheText(r), mono: false },
                    ]}
                  />
                  {r.tags.length > 0 ? (
                    <div className="mt-2 flex flex-wrap gap-1.5">
                      {r.tags.map((t) => (
                        <Badge key={t} mono>
                          {t}
                        </Badge>
                      ))}
                    </div>
                  ) : null}
                </Section>
                <Section title="Usage">
                  <Facts
                    items={[
                      {
                        label: 'Input tokens',
                        value: `${r.usage.input != null ? formatCount(r.usage.input) : '—'}${
                          money(cost?.input) ? ` · ${formatMoney(money(cost?.input))}` : ''
                        }`,
                      },
                      {
                        label: 'Output tokens',
                        value: `${r.usage.output != null ? formatCount(r.usage.output) : '—'}${
                          money(cost?.output) ? ` · ${formatMoney(money(cost?.output))}` : ''
                        }`,
                      },
                      ...(r.usage.input_cache_read
                        ? [{ label: 'Cached input tokens', value: formatTokens(r.usage.input_cache_read) }]
                        : []),
                      { label: 'Time to first token', value: formatLatency(ttftMs(r)) },
                      {
                        label: 'Total cost',
                        value: (
                          <span className="font-semibold">
                            {formatMoney(costUsd(r))}
                            {r.team ? (
                              <span className="font-normal text-muted"> · billed to {r.team}</span>
                            ) : null}
                          </span>
                        ),
                      },
                    ]}
                  />
                </Section>
                <RequestLedger id={r.id} />
                <LatencyBreakdown record={r} />
                <Section title="Reproduce with curl">
                  {canReadContent ? (
                    dataPlaneUrl ? (
                      <>
                        <p className="mb-2 text-caption text-muted">
                          Replace $ANTIRION_API_KEY with one of your keys.
                        </p>
                        <Json value={curlFor(r, dataPlaneUrl)} label="Copy the curl command" />
                      </>
                    ) : (
                      <p className="text-ui text-muted">Set the gateway URL in Settings to fill this in.</p>
                    )
                  ) : (
                    <p className="text-ui text-muted">
                      You don&apos;t have permission to see prompts and responses, so the request body
                      can&apos;t be shown.
                    </p>
                  )}
                </Section>
              </>
            ) : null}
            {tab === 'messages' ? <MessagesTab record={r} /> : null}
            {tab === 'request' ? <RequestTab record={r} /> : null}
            {tab === 'response' ? <ResponseTab record={r} /> : null}
            {tab === 'trace' ? (
              <>
                <Section
                  title="Attempts"
                  aside={r.attempts.length > 1 ? `${r.attempts.length} attempts` : 'No retries'}
                >
                  <Table label="Attempts">
                    <thead>
                      <tr>
                        <Th>#</Th>
                        <Th>Deployment</Th>
                        <Th>Why</Th>
                        <Th>Outcome</Th>
                        <Th numeric>Duration</Th>
                      </tr>
                    </thead>
                    <tbody>
                      {r.attempts.map((a) => (
                        <Tr key={a.index}>
                          <Td mono muted>
                            {a.index + 1}
                          </Td>
                          <Td mono>{a.deployment}</Td>
                          <Td>{capitalize(a.reason.replace(/_/g, ' '))}</Td>
                          <Td>
                            <span className="flex items-center gap-1.5">
                              <Dot tone={a.outcome === 'success' ? 'success' : 'error'} />
                              {capitalize(a.outcome.replace(/_/g, ' '))}
                              {a.status_code !== null ? <StatusCode code={a.status_code} /> : null}
                              {a.failure_class ? (
                                <span className="mono text-caption text-muted">{a.failure_class}</span>
                              ) : null}
                            </span>
                          </Td>
                          <Td numeric>{formatLatency(a.duration_ms)}</Td>
                        </Tr>
                      ))}
                    </tbody>
                  </Table>
                </Section>

                <Section title="Distributed trace">
                  <div className="flex flex-wrap items-center gap-2">
                    {trace ? (
                      <a
                        href={trace}
                        target="_blank"
                        rel="noreferrer noopener"
                        className={buttonClass('secondary', 'sm')}
                      >
                        <Icon name="external" size={14} />
                        Open the trace
                      </a>
                    ) : null}
                    <span className="mono text-caption text-muted">
                      Trace {r.trace_id} · span {r.span_id}
                    </span>
                    <CopyButton value={r.trace_id} label="Copy the trace ID" />
                  </div>
                  {!trace ? (
                    <p className="mt-1.5 text-caption text-muted">
                      Set a trace viewer link in Settings → Observability to open traces from here.
                    </p>
                  ) : null}
                </Section>
                <TimingTable timing={r.timing} />
              </>
            ) : null}
            {tab === 'guardrails' ? (
              r.guardrails.length === 0 ? (
                <EmptyState size="sm" title="No guardrails ran on this request" />
              ) : (
                <Section title="Guardrails" aside={`${r.guardrails.length} checks`}>
                  <ul className="flex flex-col border-y border-line">
                    {r.guardrails.map((g, i) => {
                      const verdict = guardrailVerdict(g);
                      const tone = verdict === 'pass' ? 'success' : verdict === 'block' ? 'error' : 'warn';
                      const score = canReadContent ? num(g.score) : null;
                      return (
                        <li
                          key={i}
                          className={cn(
                            'grid grid-cols-[96px_minmax(0,1fr)_auto] items-center gap-3 px-3 py-2',
                            i > 0 && 'border-t border-line',
                          )}
                        >
                          <Badge tone={tone} dot caps>
                            {verdict}
                          </Badge>
                          <span className="mono truncate text-ui text-fg">
                            {typeof g.name === 'string'
                              ? g.name
                              : typeof g.guardrail === 'string'
                                ? g.guardrail
                                : '—'}
                            {typeof g.category === 'string' ? (
                              <span className="ml-2 font-sans text-caption text-muted">{g.category}</span>
                            ) : null}
                          </span>
                          <span className="mono text-caption text-muted tabular-nums">
                            {score !== null ? `score ${score.toFixed(2)}` : ''}
                            {num(g.duration_ms) !== null ? ` · ${formatLatency(num(g.duration_ms))}` : ''}
                          </span>
                        </li>
                      );
                    })}
                  </ul>
                  <details className="group mt-3">
                    <summary className="flex w-fit cursor-pointer list-none items-center gap-1 rounded-xs text-caption text-muted hover:text-fg [&::-webkit-details-marker]:hidden">
                      <Icon
                        name="chevronRight"
                        size={12}
                        className="transition-transform group-open:rotate-90"
                      />
                      Raw results
                    </summary>
                    <div className="mt-2">
                      <Json
                        value={
                          canReadContent
                            ? r.guardrails
                            : r.guardrails.map(({ score: _score, ...rest }) => rest)
                        }
                        label="Copy the results"
                      />
                    </div>
                  </details>
                </Section>
              )
            ) : null}
          </div>
        </div>
      ) : null}
    </Drawer>
  );
}
