'use client';

import { useState, type ReactNode } from 'react';

import { Badge, StatusCode } from '@/components/ui/badge';
import { Segmented } from '@/components/ui/form';
import { usePermission } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { CopyButton } from '@/components/ui/misc';
import { EmptyState } from '@/components/ui/states';
import type { LogRecord } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatBytes, formatCount, formatLatency, plural } from '@/lib/format';

import { ttftMs } from '../types';
import { requestMessages, responseView, storedBody, type TranscriptMessage } from '../transcript';

/** A block of code or JSON with copy and, optionally, a wrap switch. */
export function CodeBlock({
  value,
  label = 'Copy',
  wrap = true,
  className,
}: {
  value: unknown;
  label?: string;
  wrap?: boolean;
  className?: string;
}) {
  const text = typeof value === 'string' ? value : JSON.stringify(value, null, 2);
  return (
    <div className={cn('relative', className)}>
      <span className="absolute top-2 right-2">
        <CopyButton value={text} label={label} />
      </span>
      <pre
        className={cn(
          'mono max-h-[420px] overflow-auto rounded-control border border-line bg-sunken p-3 pr-10 text-caption text-fg',
          wrap ? 'break-words whitespace-pre-wrap' : 'whitespace-pre',
        )}
      >
        {text}
      </pre>
    </div>
  );
}

function Meta({ children }: { children: ReactNode }) {
  return (
    <div className="mb-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-caption text-muted">{children}</div>
  );
}

/** Why the content of a request cannot be shown, or null when it can. */
export function useContentBlock(record: LogRecord, part: 'request' | 'response'): ReactNode | null {
  const canRead = usePermission('logs:read_content');
  if (!canRead)
    return (
      <EmptyState
        size="sm"
        title="You can't see prompts and responses"
        description="Ask a gateway admin for access."
      />
    );
  if (!storedBody(record.content?.[part]))
    return (
      <EmptyState
        size="sm"
        title={
          part === 'response' && record.content?.request
            ? 'No response was stored'
            : "Content wasn't stored for this request"
        }
        description={
          part === 'response' && record.content?.request
            ? "The request failed first, or the answer wasn't kept."
            : 'Turn on content logging in Settings → Data & privacy.'
        }
      />
    );
  return null;
}

function Truncated({ bytes }: { bytes: number | null }) {
  return (
    <p className="mb-2 text-caption text-warn">
      Cut at the logging limit{bytes !== null ? `; the full body was ${formatBytes(bytes)}` : ''}.
    </p>
  );
}

function KeyValues({ items, empty }: { items: [string, string][]; empty: string }) {
  if (items.length === 0) return <EmptyState size="sm" title={empty} />;
  return (
    <dl className="grid grid-cols-[minmax(120px,200px)_minmax(0,1fr)] gap-x-4 border-y border-line max-sm:grid-cols-1">
      {items.map(([k, v], i) => (
        <div key={k} className={cn('contents', i > 0 && '[&>*]:border-t [&>*]:border-line')}>
          <dt className="mono py-1.5 text-caption break-all text-muted">{k}</dt>
          <dd className="mono min-w-0 py-1.5 text-caption break-all text-fg">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

type RequestPart = 'body' | 'headers' | 'params';

/** The request: its body with type, size, copy and wrap; the recorded headers; the query parameters. */
export function RequestTab({ record }: { record: LogRecord }) {
  const [part, setPart] = useState<RequestPart>('body');
  const [wrap, setWrap] = useState(true);
  const blocked = useContentBlock(record, 'request');
  const stored = storedBody(record.content?.request);
  const headers = Object.entries(record.request_headers).sort(([a], [b]) => a.localeCompare(b));
  const params = Object.entries(record.query ?? {}).sort(([a], [b]) => a.localeCompare(b));
  const type = record.request_headers['content-type'] || 'application/json';
  return (
    <div className="flex flex-col gap-3">
      <Segmented<RequestPart>
        label="Part of the request"
        value={part}
        onChange={setPart}
        className="self-start"
        options={[
          { value: 'body', label: 'Body' },
          { value: 'headers', label: 'Headers', count: headers.length },
          { value: 'params', label: 'Params', count: params.length },
        ]}
      />
      {part === 'body'
        ? (blocked ?? (
            <div>
              <Meta>
                <Badge mono>{type}</Badge>
                <span className="mono">
                  {record.method} {record.endpoint}
                </span>
                {stored?.bytes !== null && stored?.bytes !== undefined ? (
                  <span className="mono">{formatBytes(stored.bytes)}</span>
                ) : null}
                <button
                  type="button"
                  aria-pressed={!wrap}
                  onClick={() => setWrap(!wrap)}
                  className="ml-auto rounded-xs text-caption text-muted hover:text-fg"
                >
                  {wrap ? 'No wrap' : 'Wrap lines'}
                </button>
              </Meta>
              {stored?.truncated ? <Truncated bytes={stored.bytes} /> : null}
              <CodeBlock value={stored?.body ?? null} label="Copy the body" wrap={wrap} />
            </div>
          ))
        : null}
      {part === 'headers' ? (
        <div>
          <p className="mb-2 text-caption text-muted">
            The headers the gateway recorded. Credentials are never recorded.
          </p>
          <KeyValues items={headers} empty="No headers were recorded" />
        </div>
      ) : null}
      {part === 'params' ? <KeyValues items={params} empty="No query parameters" /> : null}
    </div>
  );
}

const ROLE_TONE: Record<string, string> = {
  user: 'text-accent',
  assistant: 'text-success',
  tool: 'text-info',
  system: 'text-muted',
  developer: 'text-muted',
};

function Message({ m, index }: { m: TranscriptMessage; index: number }) {
  return (
    <li className="overflow-hidden rounded-control border border-line">
      <div className="flex items-center gap-2 border-b border-line bg-sunken px-3 py-1.5 text-caption">
        <span className={cn('mono font-semibold', ROLE_TONE[m.role] ?? 'text-fg')}>{m.role}</span>
        {m.response ? <Badge tone="success">Response</Badge> : null}
        {m.toolCallId ? <span className="mono truncate text-dim">for {m.toolCallId}</span> : null}
        <span className="ml-auto text-dim">#{index + 1}</span>
      </div>
      <div className="flex flex-col gap-2 px-3 py-2.5">
        {m.text ? (
          <p
            className={cn(
              'text-ui break-words whitespace-pre-wrap text-fg',
              m.role === 'tool' && 'mono text-caption',
            )}
          >
            {m.text}
          </p>
        ) : null}
        {m.attachments.length > 0 ? (
          <p className="text-caption text-muted">{m.attachments.map((a) => `[${a}]`).join(' ')}</p>
        ) : null}
        {m.toolCalls.map((c, i) => (
          <div key={c.id ?? i} className="rounded-sm border border-line bg-sunken px-2.5 py-2">
            <p className="mono flex items-center gap-1.5 text-caption text-info">
              <Icon name="arrowRight" size={12} />
              Calls {c.name}
              {c.id ? <span className="text-dim">· {c.id}</span> : null}
            </p>
            <pre className="mono mt-1 text-caption break-words whitespace-pre-wrap text-muted">
              {c.arguments}
            </pre>
          </div>
        ))}
        {!m.text && m.toolCalls.length === 0 && m.attachments.length === 0 ? (
          <p className="text-caption text-dim">Empty message</p>
        ) : null}
      </div>
    </li>
  );
}

/** The conversation by role, the response last, with tool calls and results. */
export function MessagesTab({ record }: { record: LogRecord }) {
  const blocked = useContentBlock(record, 'request');
  if (blocked) return blocked;
  const sent = requestMessages(storedBody(record.content?.request)?.body);
  const answer = responseView(storedBody(record.content?.response)?.body);
  const messages = [...sent, ...answer.messages];
  if (messages.length === 0)
    return (
      <EmptyState
        size="sm"
        title="No conversation in this request"
        description="The Request tab shows its body."
      />
    );
  const tools = messages.reduce((a, m) => a + m.toolCalls.length, 0);
  return (
    <div className="flex flex-col gap-3">
      <p className="text-caption text-muted">
        {plural(sent.length, 'message')} sent
        {answer.messages.length ? `, ${plural(answer.messages.length, 'message')} in the response` : ''}
        {tools ? ` · ${plural(tools, 'tool call')}` : ''}
        {record.usage.input != null ? ` · ${formatCount(record.usage.input)} input tokens` : ''}
      </p>
      <ol className="flex flex-col gap-2.5">
        {messages.map((m, i) => (
          <Message key={i} m={m} index={i} />
        ))}
      </ol>
    </div>
  );
}

/** The response: the answer in words (or the error), its tokens and timing, and the raw body. */
export function ResponseTab({ record }: { record: LogRecord }) {
  const [raw, setRaw] = useState(false);
  const blocked = useContentBlock(record, 'response');
  const stored = storedBody(record.content?.response);
  const view = responseView(stored?.body);
  const ttft = ttftMs(record);
  const meta = (
    <Meta>
      <StatusCode code={record.status_code} />
      <span className="mono">
        {record.stream
          ? 'text/event-stream, assembled'
          : record.response_headers['content-type'] || 'application/json'}
      </span>
      {record.usage.output != null ? <span>{plural(record.usage.output, 'output token')}</span> : null}
      {ttft !== null ? <span>First token after {formatLatency(ttft)}</span> : null}
      {view.finishReasons.length ? (
        <span>Finished: {view.finishReasons.join(', ').replace(/_/g, ' ')}</span>
      ) : null}
    </Meta>
  );
  if (blocked)
    return (
      <div>
        {meta}
        {blocked}
      </div>
    );
  const readable = view.error !== null || view.messages.length > 0;
  return (
    <div className="flex flex-col gap-3">
      {meta}
      {readable ? (
        <Segmented<'text' | 'raw'>
          label="Show the response as"
          value={raw ? 'raw' : 'text'}
          onChange={(v) => setRaw(v === 'raw')}
          className="self-start"
          options={[
            { value: 'text', label: 'Readable' },
            { value: 'raw', label: 'Raw JSON' },
          ]}
        />
      ) : null}
      {stored?.truncated ? <Truncated bytes={stored.bytes} /> : null}
      {readable && !raw ? (
        view.error ? (
          <div className="rounded-control border border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] px-3 py-2.5">
            <p className="text-ui font-semibold text-fg">{view.error.message}</p>
            {view.error.type ? <p className="mono mt-1 text-caption text-muted">{view.error.type}</p> : null}
          </div>
        ) : (
          <ol className="flex flex-col gap-2.5">
            {view.messages.map((m, i) => (
              <Message key={i} m={m} index={i} />
            ))}
          </ol>
        )
      ) : (
        <CodeBlock value={stored?.body ?? null} label="Copy the response" />
      )}
    </div>
  );
}
