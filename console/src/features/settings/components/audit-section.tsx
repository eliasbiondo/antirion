'use client';

import { useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Input, Select, Switch } from '@/components/ui/form';
import { Section } from '@/components/ui/layout';
import { Drawer } from '@/components/ui/overlay';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { auditActionLabel, auditActorKind, auditActorLabel, auditEntryLabel } from '@/lib/audit-words';
import type { AuditEntry } from '@/lib/api/types';
import { downloadCsv } from '@/lib/csv';
import { usePageShortcut } from '@/lib/shortcuts';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import type { AuditQuery } from '../api';
import { useAudit } from '../store';
import { useTokenNames } from '@/stores/actors';

function exportAudit(entries: AuditEntry[]) {
  downloadCsv(
    'audit.csv',
    [
      'time',
      'actor_type',
      'actor',
      'action',
      'resource',
      'outcome',
      'status',
      'generation_after',
      'request_id',
      'source_ip',
    ],
    entries.map((e) => [
      e.time,
      e.actor.type,
      e.actor.id,
      e.action,
      e.resource,
      e.outcome,
      e.status_code,
      e.generation_after,
      e.request_id,
      e.source_ip,
    ]),
  );
}

export const OUTCOME_LABEL: Record<string, string> = {
  success: 'Succeeded',
  denied: 'Denied',
  failed: 'Failed',
};

/** "keys.create" → "Key created"; "GET /admin/v1/auth/me/preferences" → "Read preferences". */
export const actionLabel = auditActionLabel;

/** The audit log: who did what to which resource, with filters, paging, export and the change set. */
export function AuditSection() {
  const tokens = useTokenNames();
  const timeZone = usePreferences((s) => s.timeZone);
  const query = useAudit((s) => s.query);
  const entries = useAudit((s) => s.entries);
  const cursor = useAudit((s) => s.cursor);
  const setQuery = useAudit((s) => s.setQuery);
  const load = useAudit((s) => s.load);
  const loadMore = useAudit((s) => s.loadMore);
  const [draft, setDraft] = useState<AuditQuery>(query);
  const [open, setOpen] = useState<AuditEntry | null>(null);
  const showReads = query.kind !== 'write';
  const setShowReads = (on: boolean) => {
    const { kind: _, ...rest } = query;
    setQuery(on ? rest : { ...rest, kind: 'write' });
  };
  const shown = entries.data ?? [];

  useEffect(() => {
    void load();
  }, [query, load]);
  usePageShortcut('e', () => exportAudit(entries.data ?? []));

  return (
    <>
      <Section
        title="Entries"
        subtitle={
          showReads
            ? 'Changes, denied actions and reads'
            : 'Changes and denied actions. Turn on Show reads to include reads.'
        }
        actions={
          <>
            <Switch label="Show reads" checked={showReads} onChange={setShowReads} />
            <Button size="sm" icon="download" variant="ghost" onClick={() => exportAudit(entries.data ?? [])}>
              Export CSV
            </Button>
          </>
        }
      >
        <form
          className="mb-3 flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            const { kind: _, ...filters } = draft;
            setQuery({
              ...Object.fromEntries(Object.entries(filters).filter(([, v]) => v)),
              ...(showReads ? {} : { kind: 'write' }),
            });
          }}
        >
          <div className="min-w-0 flex-[1_1_8rem] max-sm:basis-full">
            <Input
              aria-label="Who"
              placeholder="Who, e.g. ada"
              mono
              value={draft.actor ?? ''}
              onChange={(e) => setDraft({ ...draft, actor: e.target.value })}
            />
          </div>
          <div className="min-w-0 flex-[1.4_1_11rem] max-sm:basis-full">
            <Input
              aria-label="Resource"
              data-page-search
              placeholder="Resource, e.g. keys/ci-bot"
              mono
              value={draft.resource ?? ''}
              onChange={(e) => setDraft({ ...draft, resource: e.target.value })}
            />
          </div>
          <div className="min-w-0 flex-[1.2_1_10rem] max-sm:basis-full">
            <Input
              aria-label="Action"
              placeholder="Action, e.g. keys.create"
              mono
              value={draft.action ?? ''}
              onChange={(e) => setDraft({ ...draft, action: e.target.value })}
            />
          </div>
          <div className="w-36 shrink-0 max-sm:w-full">
            <Select
              aria-label="Result"
              value={draft.outcome ?? ''}
              onChange={(v) => setDraft({ ...draft, outcome: (v || undefined) as AuditQuery['outcome'] })}
              emptyOption="Any result"
              options={[
                { value: 'success', label: 'Succeeded' },
                { value: 'denied', label: 'Denied' },
                { value: 'failed', label: 'Failed' },
              ]}
            />
          </div>
          <Button size="sm" type="submit">
            Filter
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setDraft({});
              setQuery(showReads ? {} : { kind: 'write' });
            }}
          >
            Clear
          </Button>
        </form>
        {entries.status === 'loading' && !entries.data ? <LoadingRows rows={8} /> : null}
        {entries.error ? (
          <ErrorState error={entries.error} subject="The audit log" onRetry={() => void load()} />
        ) : null}
        {entries.data?.length === 0 ? <EmptyState title="No entries match these filters" size="sm" /> : null}
        {entries.data && shown.length > 0 ? (
          <>
            <Table label="Audit entries">
              <thead>
                <tr>
                  <Th>Time</Th>
                  <Th>Who</Th>
                  <Th>Action</Th>
                  <Th>Resource</Th>
                  <Th>Result</Th>
                </tr>
              </thead>
              <tbody>
                {shown.map((e) => (
                  <Tr
                    key={e.id}
                    interactive
                    tabIndex={0}
                    onClick={() => setOpen(e)}
                    onKeyDown={(ev) => {
                      if (ev.key === 'Enter') setOpen(e);
                    }}
                  >
                    <Td mono muted title={formatDateTime(e.time, timeZone)}>
                      {formatRelative(e.time)}
                    </Td>
                    <Td>
                      <span className={auditActorKind(e.actor) === null ? undefined : 'mono'}>
                        {auditActorLabel(e.actor, tokens)}
                      </span>{' '}
                      {auditActorKind(e.actor) ? (
                        <span className="text-caption text-dim">{auditActorKind(e.actor)}</span>
                      ) : null}
                    </Td>
                    <Td title={e.action}>{auditEntryLabel(e)}</Td>
                    <Td mono muted>
                      {e.resource ?? '—'}
                    </Td>
                    <Td>
                      <Badge
                        tone={e.outcome === 'success' ? 'success' : e.outcome === 'denied' ? 'warn' : 'error'}
                        dot
                      >
                        {OUTCOME_LABEL[e.outcome] ?? e.outcome}
                      </Badge>
                    </Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
            {cursor ? (
              <div className="mt-3 flex justify-center">
                <Button size="sm" onClick={() => void loadMore()}>
                  Load more
                </Button>
              </div>
            ) : null}
          </>
        ) : null}
      </Section>
      <Drawer
        open={open !== null}
        onClose={() => setOpen(null)}
        title={open ? auditEntryLabel(open) : ''}
        subtitle={
          open ? `${auditActorLabel(open.actor, tokens)} · ${formatDateTime(open.time, timeZone)}` : undefined
        }
      >
        {open ? (
          <div className="flex flex-col gap-3">
            <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-ui">
              <dt className="text-muted">Result</dt>
              <dd className="text-fg">
                {OUTCOME_LABEL[open.outcome] ?? open.outcome}
                {open.status_code ? <span className="mono text-muted"> ({open.status_code})</span> : null}
              </dd>
              <dt className="text-muted">Action</dt>
              <dd className="mono break-all text-fg">{open.action}</dd>
              <dt className="text-muted">Resource</dt>
              <dd className="mono break-all text-fg">{open.resource ?? '—'}</dd>
              <dt className="text-muted">Request ID</dt>
              <dd className="mono break-all text-fg">{open.request_id}</dd>
              <dt className="text-muted">From IP address</dt>
              <dd className="mono text-fg">{open.source_ip}</dd>
              <dt className="text-muted">Configuration version</dt>
              <dd className="mono text-fg">
                {open.generation_before ?? '—'} → {open.generation_after ?? '—'}
              </dd>
            </dl>
            <div className="text-ui font-medium text-fg">Changes</div>
            {open.changes && open.changes.length > 0 ? (
              <pre className="mono max-h-[60vh] overflow-auto rounded-sm border border-line bg-sunken p-3 text-caption whitespace-pre-wrap text-fg">
                {JSON.stringify(open.changes, null, 2)}
              </pre>
            ) : (
              <p className="text-ui text-muted">Nothing was changed by this action.</p>
            )}
          </div>
        ) : null}
      </Drawer>
    </>
  );
}
