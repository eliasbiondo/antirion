'use client';

import { useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { DefinitionGrid } from '@/components/ui/layout';
import { DetailHeader, DetailSection } from '@/components/ui/list-detail';
import { CopyLinkButton } from '@/components/ui/misc';
import { InlineError } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import type { ChangeRequest } from '@/lib/api/client';
import { plural } from '@/lib/format';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useSession } from '@/stores/session';
import { notify } from '@/stores/ui';

import { approvalsLeft, changeLine, STATE_LABEL, stateTone } from '../describe';
import { useChangeRequests } from '../store';

function json(value: unknown): string {
  if (value === null || value === undefined) return '—';
  return JSON.stringify(value, null, 2);
}

type Pending = 'reject' | 'cancel' | 'apply' | null;

/** One change request: what it changes, who approved it, and the actions the viewer may take. */
export function ChangeRequestDetail({ request: cr }: { request: ChangeRequest }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const me = useSession((s) => s.me);
  const approve = useChangeRequests((s) => s.approve);
  const reject = useChangeRequests((s) => s.reject);
  const apply = useChangeRequests((s) => s.apply);
  const cancel = useChangeRequests((s) => s.cancel);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [confirm, setConfirm] = useState<Pending>(null);

  // The gateway records the person behind a credential (a user's every token is one person);
  // requests opened before that carry the credential's id.
  const person = me?.user?.name ?? me?.principal.id;
  const isMe = (who: string) => who === person || who === me?.principal.id;
  const mine = isMe(cr.author);
  const pending = cr.state === 'open' || cr.state === 'approved';
  const approvedByMe = cr.approvals.some((a) => isMe(a.by));
  const left = approvalsLeft(cr);
  const rejection = (cr as { rejection_reason?: unknown }).rejection_reason;

  async function run(action: () => Promise<void>, done: string) {
    setBusy(true);
    setError(null);
    try {
      await action();
      notify('success', done, cr.title);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const authorReason = 'You opened this request; someone else has to review it.';
  const timeline: { at: string | null; text: string }[] = [
    { at: cr.created_at, text: `Opened by ${cr.author}` },
    ...cr.approvals.map((a) => ({ at: a.at, text: `Approved by ${a.by}` })),
    ...(cr.state === 'applied'
      ? [
          {
            at: null,
            text: `Applied${cr.applied_generation != null ? ` as generation ${cr.applied_generation}` : ''}`,
          },
        ]
      : cr.state !== 'open' && cr.state !== 'approved'
        ? [{ at: null, text: STATE_LABEL[cr.state] }]
        : []),
  ];

  return (
    <div>
      <DetailHeader
        eyebrow={
          <>
            <Badge tone={stateTone(cr.state)} dot>
              {STATE_LABEL[cr.state]}
            </Badge>
            <Badge>{cr.kind === 'action' ? 'Held action' : 'Configuration'}</Badge>
            <span>{cr.id}</span>
          </>
        }
        title={cr.title}
        subtitle={`${cr.author} · opened ${formatRelative(cr.created_at)}`}
        aside={<CopyLinkButton />}
        actions={
          pending ? (
            <>
              {cr.state === 'open' ? (
                <Button
                  variant="primary"
                  icon="check"
                  busy={busy}
                  disabled={mine || approvedByMe}
                  disabledReason={mine ? authorReason : approvedByMe ? 'You approved it already.' : undefined}
                  onClick={() => void run(() => approve(cr.id), 'Change request approved')}
                >
                  Approve
                </Button>
              ) : (
                <Button variant="primary" icon="play" busy={busy} onClick={() => setConfirm('apply')}>
                  Apply change
                </Button>
              )}
              <Button
                variant="secondary"
                disabled={mine}
                disabledReason={mine ? authorReason : undefined}
                onClick={() => setConfirm('reject')}
              >
                Reject
              </Button>
              {mine ? (
                <Button variant="ghost" onClick={() => setConfirm('cancel')}>
                  Cancel request
                </Button>
              ) : null}
            </>
          ) : undefined
        }
      />
      {error ? (
        <div className="px-7 pt-4">
          <InlineError error={error} />
        </div>
      ) : null}
      <DetailSection title="Summary">
        <DefinitionGrid
          items={[
            { label: 'State', value: STATE_LABEL[cr.state] },
            {
              label: 'Approvals',
              value: `${cr.approvals.length} of ${cr.approvals_required}${
                pending && left > 0 ? ` · needs ${plural(left, 'more approval')}` : ''
              }`,
            },
            { label: 'Opened', value: formatDateTime(cr.created_at, timeZone) },
            {
              label: pending ? 'Expires' : 'Expired after',
              value: cr.expires_at ? formatDateTime(cr.expires_at, timeZone) : '—',
            },
            { label: 'Based on generation', value: cr.base_generation ?? '—' },
            { label: 'Applied as generation', value: cr.applied_generation ?? '—' },
          ]}
        />
        {cr.description ? (
          <p className="mt-3 text-body whitespace-pre-wrap text-fg">{cr.description}</p>
        ) : null}
        {typeof rejection === 'string' && rejection ? (
          <p className="mt-3 text-ui text-muted">Reason for rejecting: {rejection}</p>
        ) : null}
      </DetailSection>
      <DetailSection title={`Changes (${cr.changes.length})`}>
        <ul className="flex flex-col gap-2">
          {cr.changes.map((c, i) => (
            <li key={i} className="flex flex-col gap-1.5">
              <span className="mono rounded-control border border-line bg-sunken px-2.5 py-1.5 text-caption break-all text-fg">
                {changeLine(c)}
              </span>
              {c.body !== undefined ? (
                <pre className="mono max-h-56 overflow-auto rounded-control border border-line bg-sunken px-2.5 py-2 text-caption text-muted">
                  {json(c.body)}
                </pre>
              ) : null}
              {json(c.body).includes('[REDACTED]') ? (
                <span className="text-caption text-muted">
                  Secret values are hidden. The gateway keeps them encrypted until the request is applied.
                </span>
              ) : null}
            </li>
          ))}
        </ul>
      </DetailSection>
      {cr.diff && cr.diff.length > 0 ? (
        <DetailSection title="What it changes in the configuration">
          <Table label="Configuration changes">
            <thead>
              <tr>
                <Th>Change</Th>
                <Th>Path</Th>
                <Th>Before</Th>
                <Th>After</Th>
              </tr>
            </thead>
            <tbody>
              {cr.diff.map((d, i) => (
                <Tr key={i}>
                  <Td>{d.op === 'create' ? 'Added' : d.op === 'delete' ? 'Removed' : 'Changed'}</Td>
                  <Td mono>{d.path}</Td>
                  <Td mono muted className="max-w-64 truncate" title={json(d.before)}>
                    {d.before === null ? '—' : JSON.stringify(d.before)}
                  </Td>
                  <Td mono className="max-w-64 truncate" title={json(d.after)}>
                    {d.after === null ? '—' : JSON.stringify(d.after)}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        </DetailSection>
      ) : null}
      <DetailSection title="Timeline">
        <ol className="flex flex-col gap-2">
          {timeline.map((t, i) => (
            <li key={i} className="flex items-baseline gap-3 text-ui">
              <span className="mono w-44 shrink-0 text-caption text-muted">
                {t.at ? formatDateTime(t.at, timeZone) : ''}
              </span>
              <span className="text-fg">{t.text}</span>
            </li>
          ))}
          {pending && cr.expires_at ? (
            <li className="flex items-baseline gap-3 text-ui">
              <span className="mono w-44 shrink-0 text-caption text-muted">
                {formatDateTime(cr.expires_at, timeZone)}
              </span>
              <span className="text-muted">Expires unless applied</span>
            </li>
          ) : null}
        </ol>
      </DetailSection>
      <ConfirmDialog
        open={confirm === 'apply'}
        onClose={() => setConfirm(null)}
        tone="primary"
        withReason={false}
        title={`Apply ${cr.title}?`}
        effect={
          cr.kind === 'action'
            ? 'The held action runs now, as you.'
            : 'The change becomes the running configuration on every node.'
        }
        confirmLabel="Apply change"
        onConfirm={async () => {
          await apply(cr.id);
          notify('success', 'Change applied', cr.title);
        }}
      />
      <ConfirmDialog
        open={confirm === 'reject'}
        onClose={() => setConfirm(null)}
        title={`Reject ${cr.title}?`}
        effect="The request closes and nothing changes. Its author can open a new one."
        confirmLabel="Reject request"
        onConfirm={async (reason) => {
          await reject(cr.id, reason);
          notify('success', 'Change request rejected', cr.title);
        }}
      />
      <ConfirmDialog
        open={confirm === 'cancel'}
        onClose={() => setConfirm(null)}
        title={`Cancel ${cr.title}?`}
        effect="The request closes and nothing changes."
        confirmLabel="Cancel request"
        onConfirm={async (reason) => {
          await cancel(cr.id, reason);
          notify('success', 'Change request cancelled', cr.title);
        }}
      />
    </div>
  );
}
