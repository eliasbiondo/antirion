'use client';

import Link from 'next/link';
import { useEffect, useState } from 'react';

import { useScopedHref, useSettingsHref } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { usePermission } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { Panel } from '@/components/ui/layout';
import { Drawer } from '@/components/ui/overlay';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import type { ChangeRequest } from '@/lib/api/client';
import { plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';

import { PENDING } from '@/features/change-requests/api';
import { ChangeRequestDetail } from '@/features/change-requests/components/change-request-detail';
import { approvalsLeft, STATE_LABEL, stateTone } from '@/features/change-requests/describe';
import { useChangeRequests } from '@/features/change-requests/store';

import { feedOf } from '../insights';
import { useRouting } from '../store';
import type { Rule } from '../types';

/** Whether a change request touches routing: routes, or the resilience, policy or server settings. */
export function isRoutingChange(cr: ChangeRequest): boolean {
  return cr.changes.some((c) =>
    /^\/(routes(\/|$)|routing\/|settings\/(resilience|policies|routing|server))/.test(c.path),
  );
}

/** Routing's open change requests, from the change-request flow (RO-26). */
export function PendingChanges() {
  const list = useChangeRequests((s) => s.list);
  const load = useChangeRequests((s) => s.load);
  const canRead = usePermission('config:read');
  const allHref = useScopedHref('/change-requests/');
  const [openId, setOpenId] = useState<string | null>(null);
  useEffect(() => {
    if (canRead) void load({ quiet: true });
  }, [canRead, load]);
  if (!canRead) return null;
  const pending = (list.data ?? []).filter((cr) => PENDING.includes(cr.state) && isRoutingChange(cr));
  const open = (list.data ?? []).find((cr) => cr.id === openId) ?? null;
  return (
    <Panel
      title="Pending changes"
      subtitle="Waiting for review before they apply"
      actions={
        <Link href={allHref} className="text-caption text-accent hover:underline">
          All change requests
        </Link>
      }
    >
      {list.error ? <ErrorState error={list.error} subject="Change requests" size="sm" /> : null}
      {!list.data && !list.error ? <LoadingRows rows={2} /> : null}
      {list.data && pending.length === 0 ? (
        <p className="text-ui text-muted">
          Nothing waiting. When change control covers routes, saving a rule opens a request here for review.
        </p>
      ) : null}
      {pending.length > 0 ? (
        <ul aria-label="Pending routing changes" className="flex flex-col">
          {pending.map((cr) => {
            const left = approvalsLeft(cr);
            return (
              <li key={cr.id} className="border-b border-line last:border-0">
                <button
                  type="button"
                  onClick={() => setOpenId(cr.id)}
                  className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 py-2.5 text-left hover:bg-hover"
                >
                  <Icon name="pullRequest" size={14} className="text-warn" />
                  <span className="min-w-0 flex-1 truncate text-ui font-medium text-fg">{cr.title}</span>
                  <Badge tone={stateTone(cr.state)}>{STATE_LABEL[cr.state]}</Badge>
                  <span className="text-caption text-muted">
                    {cr.author} · {formatRelative(cr.created_at)} ·{' '}
                    {left > 0 ? `${plural(left, 'approval')} to go` : 'ready to apply'}
                    {cr.approvals.length > 0
                      ? ` · approved by ${cr.approvals.map((a) => a.by).join(', ')}`
                      : ''}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      ) : null}
      <Drawer open={open !== null} onClose={() => setOpenId(null)} title="Change request" width="lg">
        {open ? <ChangeRequestDetail request={open} /> : null}
      </Drawer>
    </Panel>
  );
}

const KIND_ICON = { config: 'edit', alert: 'alert', canary: 'trendUp' } as const;
const TONE_CLASS = {
  neutral: 'text-muted',
  warn: 'text-warn',
  error: 'text-error',
  success: 'text-success',
} as const;

/** Route and default changes, breaker and canary events, newest first (RO-33). */
export function ChangeFeed({ route, rules }: { route: string | null; rules: Rule[] }) {
  const audit = useRouting((s) => s.audit);
  const alerts = useRouting((s) => s.alerts);
  const state = useRouting((s) => s.state);
  const auditHref = useSettingsHref('audit');
  const items = feedOf(audit.data ?? [], alerts.data ?? [], state.data ?? [], route, rules).slice(0, 40);
  return (
    <Panel
      title="Recent policy changes"
      subtitle="Route and default edits, breaker trips and canary events"
      actions={
        <Link href={auditHref} className="text-caption text-accent hover:underline">
          Audit log
        </Link>
      }
    >
      {audit.error && alerts.error ? <ErrorState error={audit.error} subject="Changes" size="sm" /> : null}
      {!audit.data && !audit.error ? <LoadingRows rows={3} /> : null}
      {audit.data && items.length === 0 ? (
        <EmptyState
          title="No changes yet"
          description="Edits to routes and defaults show here with who made them."
        />
      ) : null}
      {items.length > 0 ? (
        <ol aria-label="Recent policy changes" className="flex flex-col">
          {items.map((item, i) => (
            <li key={`${item.at}-${i}`} className="flex gap-3 border-b border-line py-2.5 last:border-0">
              <Icon
                name={KIND_ICON[item.kind]}
                size={14}
                className={`mt-0.5 shrink-0 ${TONE_CLASS[item.tone]}`}
              />
              <div className="min-w-0 flex-1">
                <p className="text-ui text-fg">{item.title}</p>
                <p className="text-caption text-muted">
                  {[item.detail, item.actor ? `by ${item.actor}` : null].filter(Boolean).join(' · ')}
                </p>
              </div>
              <span className="shrink-0 text-caption text-muted tabular-nums">
                {item.at ? formatRelative(item.at) : 'now'}
              </span>
            </li>
          ))}
        </ol>
      ) : null}
      {audit.error && !alerts.error ? (
        <p className="mt-2 text-caption text-muted">Configuration changes need access to the audit log.</p>
      ) : null}
    </Panel>
  );
}
