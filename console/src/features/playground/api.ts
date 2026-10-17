import { api, API_BASE, ApiError, errorOf, listAll } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';
import type { LogRecord, ProjectResource } from '@/lib/api/types';

import { splitEvents, type Endpoint, type StreamEvent } from './body';

export type PlaygroundResult = components['schemas']['PlaygroundResult'];

/** What the gateway says before the answer: status, decision headers, the record and the key. */
export interface SendHead {
  status: number;
  headers: Record<string, string>;
  logId: string | null;
  key: string;
}

export interface SendInput {
  project: string;
  endpoint: Endpoint;
  body: Record<string, unknown>;
  stream: boolean;
}

export interface SendHandlers {
  onHead: (head: SendHead) => void;
  /** One event of a streamed answer, as it arrives. */
  onEvent: (event: StreamEvent) => void;
  /** The whole answer of an unstreamed send (or the events of one a gateway buffered). */
  onBody: (body: unknown, events: StreamEvent[] | null) => void;
}

const OPERATION = 'POST /playground:send';

function headOf(r: {
  status: number;
  headers?: Record<string, string>;
  log_id?: string | null;
  key: string;
}): SendHead {
  return { status: r.status, headers: r.headers ?? {}, logId: r.log_id ?? null, key: r.key };
}

/**
 * Sends through the playground. A streamed send asks for an event stream and hands each event over
 * as it arrives, after the gateway's `playground` event; a gateway that answers with JSON instead
 * (an older one, or an answer that was not a stream) is read whole.
 */
export async function sendPlayground(
  input: SendInput,
  handlers: SendHandlers,
  signal: AbortSignal,
): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`${API_BASE}/playground:send`, {
      method: 'POST',
      headers: {
        accept: input.stream ? 'text/event-stream, application/json' : 'application/json',
        'content-type': 'application/json',
      },
      body: JSON.stringify(input),
      credentials: 'same-origin',
      cache: 'no-store',
      signal,
    });
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === 'AbortError') throw cause;
    throw new ApiError({
      status: 0,
      code: 'network_error',
      message: 'The gateway could not be reached.',
      requestId: null,
      operation: OPERATION,
    });
  }
  if (!response.ok) throw await errorOf(response, OPERATION);
  const type = response.headers.get('content-type') ?? '';
  if (!type.startsWith('text/event-stream') || !response.body) {
    const result = (await response.json()) as PlaygroundResult;
    handlers.onHead(headOf(result));
    handlers.onBody(result.body, (result.events as StreamEvent[] | undefined) ?? null);
    return;
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let headed = false;
  for (;;) {
    const { done, value } = await reader.read();
    buffer += decoder.decode(value, { stream: !done });
    const { events, rest } = splitEvents(done ? `${buffer}\n\n` : buffer);
    buffer = rest;
    for (const e of events) {
      if (!headed && e.event === 'playground' && typeof e.data === 'object' && e.data !== null) {
        headed = true;
        handlers.onHead(headOf(e.data as PlaygroundResult));
      } else {
        handlers.onEvent(e);
      }
    }
    if (done) return;
  }
}

export const playgroundApi = {
  projects: (): Promise<ProjectResource[]> =>
    listAll((cursor) => api.get('/projects', { query: { limit: 500, cursor } }), 4),
  /** A record, or null while the gateway has not written it yet. */
  record: async (id: string): Promise<LogRecord | null> => {
    try {
      return await api.get('/logs/{id}', { path: { id } });
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return null;
      throw e;
    }
  },
};
