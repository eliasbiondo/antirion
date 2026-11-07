'use client';

import { Fragment, useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { DatePicker, TimeField } from '@/components/ui/date-picker';
import { Field, Input, Select, Switch } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Section } from '@/components/ui/layout';
import { Drawer } from '@/components/ui/overlay';
import { EmptyState, LoadError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import {
  auditActionLabel,
  auditActorKind,
  auditActorLabel,
  auditChangeList,
  auditEntryLabel,
  auditRecordedFacts,
} from '@/lib/audit-words';
import type { AuditEntry } from '@/lib/api/types';
import { downloadCsv } from '@/lib/csv';
import { formatCount, formatDuration, parseDuration } from '@/lib/format';
import { SETTINGS_DEFAULTS } from '@/lib/settings-defaults';
import { usePageShortcut } from '@/lib/shortcuts';
import { formatDateTime, formatRelative, zoneLabel } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import type { AuditQuery } from '../api';
import { AUDIT_RANGES, type AuditRange, auditBounds, type CustomRange } from '../audit-range';
import { AUDIT_EXPORT_MAX, useAudit, useGatewaySettings } from '../store';
import { useTokenNames } from '@/stores/actors';
import { translateNow } from '@/lib/i18n';

function downloadAudit(entries: AuditEntry[]) {
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

/**
 * What an entry changed: its list of changes, or what it recorded in another shape (an export's
 * filters and format) as facts, so no entry reads as having recorded nothing.
 */
function EntryChanges({ changes }: { changes: unknown }) {
  const facts = auditRecordedFacts(changes);
  if (facts)
    return (
      <>
        <div className="text-ui font-medium text-fg">Recorded with this action</div>
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-ui">
          {facts.map((f) => (
            <Fragment key={f.label}>
              <dt className="text-muted">{f.label}</dt>
              <dd className="mono break-all text-fg">{f.value}</dd>
            </Fragment>
          ))}
        </dl>
      </>
    );
  return (
    <>
      <div className="text-ui font-medium text-fg">Changes</div>
      {auditChangeList(changes).length > 0 ? (
        <pre className="mono max-h-[60vh] overflow-auto rounded-sm border border-line bg-sunken p-3 text-caption whitespace-pre-wrap text-fg">
          {JSON.stringify(changes, null, 2)}
        </pre>
      ) : (
        <p className="text-ui text-muted">Nothing was changed by this action.</p>
      )}
    </>
  );
}

export const OUTCOME_LABEL: Record<string, string> = {
  success: 'Succeeded',
  denied: 'Denied',
  failed: 'Failed',
};

/** "keys.create" → "Key created"; "GET /admin/v1/auth/me/preferences" → "Read preferences". */
export const actionLabel = auditActionLabel;

/** How long the audit log keeps entries (`admin.audit.retention`, set in the configuration file). */
function useAuditRetention(): string | null {
  const canRead = useCan('getAdminSettings');
  const admin = useGatewaySettings((s) => s.admin);
  useEffect(() => {
    if (canRead && !admin.data) void useGatewaySettings.getState().loadAdmin();
  }, [canRead, admin.data]);
  if (!canRead) return null;
  const spec = admin.data?.spec;
  const retention = spec?.audit?.retention ?? SETTINGS_DEFAULTS.admin.audit.retention;
  const seconds = parseDuration(retention);
  if (seconds === null) return retention;
  const days = seconds / 86_400;
  return Number.isInteger(days) ? `${days} ${days === 1 ? 'day' : 'days'}` : formatDuration(seconds);
}

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
  const exportAll = useAudit((s) => s.exportAll);
  const [draft, setDraft] = useState<AuditQuery>(query);
  const [range, setRange] = useState<AuditRange>(query.from || query.to ? 'custom' : '');
  const [custom, setCustom] = useState<CustomRange>({
    fromDay: '',
    fromTime: '00:00',
    toDay: '',
    toTime: '23:59',
  });
  const [rangeError, setRangeError] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [open, setOpen] = useState<AuditEntry | null>(null);
  const showReads = query.kind !== 'write';
  const setShowReads = (on: boolean) => {
    const { kind: _, ...rest } = query;
    setQuery(on ? rest : { ...rest, kind: 'write' });
  };
  const shown = entries.data ?? [];
  const kept = useAuditRetention();

  useEffect(() => {
    void load();
  }, [query, load]);

  /** Every entry the filters and the range match, read page by page, as one CSV. */
  async function exportAudit() {
    if (exporting) return;
    setExporting(true);
    try {
      const { entries: all, complete } = await exportAll();
      downloadAudit(all);
      notify(
        complete ? 'success' : 'info',
        `Exported ${formatCount(all.length)} ${all.length === 1 ? 'entry' : 'entries'}`,
        complete
          ? undefined
          : translateNow(
              'The newest {0} only: narrow the time range for the rest.',
              formatCount(AUDIT_EXPORT_MAX),
            ),
      );
    } catch (e) {
      notify('error', "The audit log couldn't be exported", e instanceof Error ? e.message : undefined);
    } finally {
      setExporting(false);
    }
  }
  usePageShortcut('e', () => void exportAudit());
  const between =
    query.from && query.to
      ? ` Between ${formatDateTime(query.from, timeZone)} and ${formatDateTime(query.to, timeZone)}.`
      : query.from
        ? ` Since ${formatDateTime(query.from, timeZone)}.`
        : query.to
          ? ` Until ${formatDateTime(query.to, timeZone)}.`
          : '';

  return (
    <>
      <Section
        title="Entries"
        subtitle={`${
          showReads
            ? 'Changes, denied actions and reads.'
            : 'Changes and denied actions. Turn on Show reads to include reads.'
        }${between}${kept ? ` Kept for ${kept}.` : ''}`}
        actions={
          <>
            <Switch label="Show reads" checked={showReads} onChange={setShowReads} />
            <Button
              size="sm"
              icon="download"
              variant="ghost"
              busy={exporting}
              onClick={() => void exportAudit()}
            >
              Export CSV
            </Button>
          </>
        }
      >
        <form
          className="mb-3 flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            const bounds = auditBounds(range, custom, timeZone);
            setRangeError(bounds.error ?? null);
            if (bounds.error) return;
            const { kind: _, from: _from, to: _to, ...filters } = draft;
            setQuery({
              ...Object.fromEntries(Object.entries(filters).filter(([, v]) => v)),
              ...(bounds.from ? { from: bounds.from } : {}),
              ...(bounds.to ? { to: bounds.to } : {}),
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
          <div className="w-32 shrink-0 max-sm:w-full">
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
          <div className="w-32 shrink-0 max-sm:w-full">
            <Select
              aria-label="When"
              value={range}
              onChange={(v) => {
                setRange(v as AuditRange);
                setRangeError(null);
              }}
              options={AUDIT_RANGES}
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
              setRange('');
              setRangeError(null);
              setCustom({ fromDay: '', fromTime: '00:00', toDay: '', toTime: '23:59' });
              setQuery(showReads ? {} : { kind: 'write' });
            }}
          >
            Clear
          </Button>
          {range === 'custom' ? (
            <div className="flex basis-full flex-wrap items-end gap-2">
              <div className="grid grid-cols-[minmax(0,10rem)_5.5rem] gap-2">
                <Field label={`From (${zoneLabel(timeZone)})`} controlId="audit-from-day">
                  <DatePicker
                    id="audit-from-day"
                    value={custom.fromDay}
                    clearable
                    placeholder="Any start"
                    onChange={(v) => setCustom({ ...custom, fromDay: v })}
                  />
                </Field>
                <Field label="Time" controlId="audit-from-time">
                  <TimeField
                    id="audit-from-time"
                    value={custom.fromTime}
                    onChange={(v) => setCustom({ ...custom, fromTime: v })}
                  />
                </Field>
              </div>
              <div className="grid grid-cols-[minmax(0,10rem)_5.5rem] gap-2">
                <Field
                  label={`To (${zoneLabel(timeZone)})`}
                  controlId="audit-to-day"
                  error={rangeError ?? undefined}
                >
                  <DatePicker
                    id="audit-to-day"
                    value={custom.toDay}
                    clearable
                    placeholder="Now"
                    onChange={(v) => setCustom({ ...custom, toDay: v })}
                  />
                </Field>
                <Field label="Time" controlId="audit-to-time">
                  <TimeField
                    id="audit-to-time"
                    value={custom.toTime}
                    onChange={(v) => setCustom({ ...custom, toTime: v })}
                  />
                </Field>
              </div>
            </div>
          ) : null}
        </form>
        {entries.status === 'loading' && !entries.data ? <LoadingRows rows={8} /> : null}
        <LoadError of={entries} subject="The audit log" onRetry={() => void load()} />
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
            <EntryChanges changes={open.changes} />
          </div>
        ) : null}
      </Drawer>
    </>
  );
}
