'use client';

import { useState } from 'react';

import { StatusCode } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Field, Switch, Textarea } from '@/components/ui/form';
import { Dialog } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import type { LogRecord } from '@/lib/api/types';

import { endpointOfPath } from '@/features/playground/body';
import { PlaygroundLink } from '@/features/playground/components/playground-link';

import { logsApi, type ReplayResult } from '../api';

type Json = Record<string, unknown>;

/** An RFC 6902 patch that turns `from` into `to`, member by member at the top level. */
export function bodyPatch(from: Json, to: Json): Record<string, unknown>[] {
  const ops: Record<string, unknown>[] = [];
  const pointer = (k: string) => `/${k.replace(/~/g, '~0').replace(/\//g, '~1')}`;
  for (const k of Object.keys(from)) if (!(k in to)) ops.push({ op: 'remove', path: pointer(k) });
  for (const [k, v] of Object.entries(to))
    if (JSON.stringify(from[k]) !== JSON.stringify(v)) ops.push({ op: 'add', path: pointer(k), value: v });
  return ops;
}

function parseObject(text: string): Json | null {
  try {
    const v: unknown = JSON.parse(text);
    return typeof v === 'object' && v !== null && !Array.isArray(v) ? (v as Json) : null;
  } catch {
    return null;
  }
}

/**
 * Sends a logged request again through the gateway, as it was or after editing its body, and
 * links to the record of the new request.
 */
export function ReplayDialog({
  record,
  body,
  open,
  onClose,
  onOpenRecord,
}: {
  record: LogRecord;
  body: Json;
  open: boolean;
  onClose: () => void;
  onOpenRecord: (id: string) => void;
}) {
  const original = JSON.stringify(body, null, 2);
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(original);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [result, setResult] = useState<ReplayResult | null>(null);
  const edited = editing ? parseObject(text) : body;
  const invalid = editing && edited === null;
  const endpoint = endpointOfPath(record.endpoint);

  function close() {
    setEditing(false);
    setText(original);
    setError(null);
    setResult(null);
    onClose();
  }

  async function send() {
    if (!edited) return;
    setBusy(true);
    setError(null);
    try {
      setResult(await logsApi.replay(record.id, editing ? bodyPatch(body, edited) : []));
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      open={open}
      onClose={close}
      size="lg"
      title={result ? 'Request sent again' : 'Replay this request?'}
      description={
        result
          ? undefined
          : `Sends it to ${record.endpoint} again through the gateway with a short-lived key of project ${record.project ?? '—'}. The new request is billed and logged like any other.`
      }
      footer={
        result ? (
          <>
            <Button variant="ghost" onClick={close}>
              Close
            </Button>
            {result.log_id ? (
              <Button
                variant="primary"
                icon="arrowRight"
                onClick={() => {
                  const id = result.log_id;
                  close();
                  if (id) onOpenRecord(id);
                }}
              >
                Open the new request
              </Button>
            ) : null}
          </>
        ) : (
          <>
            <Button variant="ghost" onClick={close}>
              Cancel
            </Button>
            {endpoint && edited ? (
              <PlaygroundLink
                endpoint={endpoint}
                body={edited}
                project={record.project}
                source={record.id}
                onOpen={close}
              >
                Open in playground
              </PlaygroundLink>
            ) : null}
            <Button variant="primary" icon="send" busy={busy} disabled={invalid} onClick={() => void send()}>
              {editing ? 'Send the edited request' : 'Send again'}
            </Button>
          </>
        )
      }
    >
      {result ? (
        <div className="flex flex-col gap-2 text-ui text-fg">
          <p className="flex items-center gap-2">
            The gateway answered <StatusCode code={result.status} />
          </p>
          <p className="text-muted">
            {result.log_id
              ? 'Its record is in the log, marked as a replay of this one.'
              : 'The gateway did not log it, so there is no record to open.'}
          </p>
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          <Switch checked={editing} onChange={setEditing} label="Edit the body before sending" />
          {editing ? (
            <Field label="Request body" error={invalid ? 'The body must be a JSON object.' : undefined}>
              <Textarea
                mono
                spellCheck={false}
                rows={16}
                value={text}
                onChange={(e) => setText(e.target.value)}
                aria-invalid={invalid || undefined}
              />
            </Field>
          ) : null}
          <InlineError error={error} />
        </div>
      )}
    </Dialog>
  );
}
