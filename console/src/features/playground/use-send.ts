'use client';

import { useCallback, useEffect, useRef, useState } from 'react';

import type { LogRecord } from '@/lib/api/types';

import { costUsd, latencyMs, ttftMs } from '@/features/request-log/types';
import { responseView } from '@/features/request-log/transcript';

import { playgroundApi, sendPlayground, type SendHead, type SendInput } from './api';
import { addEvent, assembleBody, emptyAssembled, type Assembled, type StreamEvent } from './body';

export type RunStatus = 'sending' | 'streaming' | 'done' | 'failed' | 'stopped';

export interface Run {
  status: RunStatus;
  input: SendInput;
  head: SendHead | null;
  answer: Assembled;
  /** The whole answer (unstreamed), or null. */
  body: unknown;
  events: StreamEvent[];
  error: unknown;
  startedAt: number;
  /** When the first event or the answer arrived (the browser's clock). */
  firstAt: number | null;
  endedAt: number | null;
  /** The gateway's record of the request, once written. */
  record: LogRecord | null;
}

/** The figures under the answer: the record's when it is written, the browser's until then. */
export interface Readout {
  input: number | null;
  output: number | null;
  cost: number | null;
  latency: number | null;
  ttft: number | null;
  /** Whether the figures come from the gateway's record. */
  recorded: boolean;
}

export function readout(run: Run): Readout {
  const r = run.record;
  if (r) {
    return {
      input: r.usage.input ?? run.answer.usage.input,
      output: r.usage.output ?? run.answer.usage.output,
      cost: costUsd(r),
      latency: latencyMs(r),
      ttft: run.input.stream ? ttftMs(r) : null,
      recorded: true,
    };
  }
  const cost = run.head?.headers['x-antirion-cost'];
  return {
    input: run.answer.usage.input,
    output: run.answer.usage.output,
    cost: cost !== undefined && Number.isFinite(Number(cost)) ? Number(cost) : null,
    latency: run.endedAt !== null ? run.endedAt - run.startedAt : null,
    ttft: run.input.stream && run.firstAt !== null ? run.firstAt - run.startedAt : null,
    recorded: false,
  };
}

function answerText(body: unknown): string {
  return responseView(body)
    .messages.map((m) => m.text)
    .filter(Boolean)
    .join('\n\n');
}

const RECORD_TRIES = 12;
const RECORD_EVERY_MS = 800;

/** Sends through the playground, keeps the answer as it streams, then reads the request's record. */
export function useSend(): {
  run: Run | null;
  send: (input: SendInput) => void;
  stop: () => void;
  clear: () => void;
} {
  const [run, setRun] = useState<Run | null>(null);
  const abort = useRef<AbortController | null>(null);
  const seq = useRef(0);

  useEffect(() => () => abort.current?.abort(), []);

  const send = useCallback((input: SendInput) => {
    abort.current?.abort();
    const controller = new AbortController();
    abort.current = controller;
    const id = ++seq.current;
    const update = (f: (r: Run) => Run) => setRun((r) => (r && seq.current === id ? f(r) : r));
    setRun({
      status: 'sending',
      input,
      head: null,
      answer: emptyAssembled(),
      body: null,
      events: [],
      error: null,
      startedAt: performance.now(),
      firstAt: null,
      endedAt: null,
      record: null,
    });
    const now = () => performance.now();
    let logId: string | null = null;
    const readRecord = async (signal: AbortSignal) => {
      const recordId = logId;
      if (!recordId) return;
      for (let i = 0; i < RECORD_TRIES && !signal.aborted && seq.current === id; i += 1) {
        await new Promise((resolve) => setTimeout(resolve, RECORD_EVERY_MS));
        try {
          const record = await playgroundApi.record(recordId);
          if (record) {
            update((r) => ({ ...r, record }));
            return;
          }
        } catch {
          return;
        }
      }
    };
    void sendPlayground(
      input,
      {
        onHead: (head) => {
          logId = head.logId;
          update((r) => ({ ...r, head, status: input.stream ? 'streaming' : r.status }));
        },
        onEvent: (event) =>
          update((r) => ({
            ...r,
            firstAt: r.firstAt ?? now(),
            events: [...r.events, event],
            answer: addEvent(r.answer, event),
          })),
        onBody: (body, events) =>
          update((r) => ({
            ...r,
            firstAt: r.firstAt ?? now(),
            body,
            events: events ?? [],
            answer: events ? events.reduce(addEvent, emptyAssembled()) : assembleBody(body, answerText),
          })),
      },
      controller.signal,
    ).then(
      () => {
        update((r) => ({ ...r, status: 'done', endedAt: now() }));
        void readRecord(controller.signal);
      },
      (error: unknown) => {
        const stopped = error instanceof DOMException && error.name === 'AbortError';
        update((r) => ({
          ...r,
          status: stopped ? 'stopped' : 'failed',
          error: stopped ? null : error,
          endedAt: now(),
        }));
        // A stopped stream is still recorded (as cancelled); a new send aborts with its own sequence.
        if (stopped) void readRecord(new AbortController().signal);
      },
    );
  }, []);

  const stop = useCallback(() => abort.current?.abort(), []);
  const clear = useCallback(() => {
    abort.current?.abort();
    seq.current += 1;
    setRun(null);
  }, []);

  return { run, send, stop, clear };
}
