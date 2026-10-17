'use client';

import Link from 'next/link';
import { useEffect, useState } from 'react';

import { Dot } from '@/components/ui/badge';
import { Forbidden, useCan } from '@/components/ui/gate';
import { Input } from '@/components/ui/form';
import { FilterBar } from '@/components/ui/layout';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { CountNote, ExportCsvButton } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { usePolling } from '@/lib/hooks';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { activityLabel, activitySubject, activityTone } from '../facts';
import { useDirectory } from '../store';
import { auditActorBy, auditActorLabel } from '@/lib/audit-words';
import { useTokenNames } from '@/stores/actors';

/** The organization's identity events: people, invitations, roles, rotations, offboardings and flags. */
export function ActivityPanel({ onUser }: { onUser: (name: string) => void }) {
  const tokens = useTokenNames();
  const timeZone = usePreferences((s) => s.timeZone);
  const activity = useDirectory((s) => s.activity);
  const load = useDirectory((s) => s.loadActivity);
  const canRead = useCan('listAudit');
  const [query, setQuery] = useState('');
  useEffect(() => {
    if (canRead) void load();
  }, [canRead, load]);
  usePolling(() => void load({ quiet: true }), canRead);
  if (!canRead) return <Forbidden what="the audit log" />;
  if (activity.error)
    return <ErrorState error={activity.error} subject="Activity" onRetry={() => void load()} />;
  if (!activity.data) return <LoadingRows />;
  const q = query.trim().toLowerCase();
  // One acceptance can be recorded twice in a row (the invitation and the account): show it once.
  const once = activity.data.filter((e, i, all) => {
    const prev = all[i - 1];
    return !(
      prev?.action === e.action &&
      activitySubject(prev).name === activitySubject(e).name &&
      Math.abs(new Date(prev.time).getTime() - new Date(e.time).getTime()) < 5_000
    );
  });
  const rows = once.filter(
    (e) =>
      !q ||
      `${activitySubject(e).name} ${activityLabel(e)} ${auditActorLabel(e.actor, tokens)} ${e.reason ?? ''}`
        .toLowerCase()
        .includes(q),
  );
  return (
    <>
      <FilterBar>
        <div className="w-64 max-sm:w-full">
          <Input
            aria-label="Search activity"
            icon="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search activity"
          />
        </div>
        <span className="ml-auto flex items-center gap-3">
          <CountNote shown={rows.length} total={activity.data.length} noun="events" />
          <ExportCsvButton
            filename="directory-activity.csv"
            rows={rows}
            columns={[
              { header: 'time', value: (e) => e.time },
              { header: 'action', value: (e) => e.action },
              { header: 'subject', value: (e) => activitySubject(e).name },
              { header: 'actor', value: (e) => auditActorLabel(e.actor, tokens) },
              { header: 'outcome', value: (e) => e.outcome },
              { header: 'reason', value: (e) => e.reason ?? '' },
            ]}
          />
        </span>
      </FilterBar>
      {rows.length === 0 ? (
        <EmptyState
          title={q ? 'No activity matches' : 'No activity yet'}
          description={q ? undefined : 'Invitations, role changes and sign-outs show here.'}
        />
      ) : (
        <ol aria-label="Directory activity" className="border-b border-line bg-raised">
          {rows.map((e) => {
            const subject = activitySubject(e);
            return (
              <li
                key={e.id}
                className="flex flex-wrap items-center gap-x-3 gap-y-0.5 border-b border-line px-gutter py-2 text-ui last:border-0 max-sm:px-gutter-sm"
              >
                <Dot tone={activityTone(e)} />
                <span className="text-fg">{activityLabel(e)}</span>
                {subject.kind === 'user' ? (
                  <button
                    type="button"
                    onClick={() => onUser(subject.name)}
                    className="mono text-fg hover:underline"
                  >
                    {subject.name}
                  </button>
                ) : subject.kind === 'key' ? (
                  <Link
                    href={`/keys/?key=${encodeURIComponent(subject.name)}`}
                    className="mono text-fg hover:underline"
                  >
                    {subject.name}
                  </Link>
                ) : (
                  <span className="mono text-muted">{subject.name}</span>
                )}
                <span className="text-muted">by {auditActorBy(e.actor, tokens)}</span>
                {e.reason ? <span className="text-caption text-muted">“{e.reason}”</span> : null}
                <Tooltip content={formatDateTime(e.time, timeZone)}>
                  <span className="ml-auto text-caption text-dim">{formatRelative(e.time)}</span>
                </Tooltip>
              </li>
            );
          })}
        </ol>
      )}
    </>
  );
}
