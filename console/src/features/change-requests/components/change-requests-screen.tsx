'use client';

import { useEffect, useMemo } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Combobox, Input } from '@/components/ui/form';
import { Forbidden, usePermission } from '@/components/ui/gate';
import { KeyboardHints } from '@/components/ui/keyboard-hints';
import { FilterBar, PageHeader, Tabs } from '@/components/ui/layout';
import { MasterDetail, SelectableList } from '@/components/ui/list-detail';
import { UpdatedAt } from '@/components/ui/misc';
import { EmptyState, ErrorState, LoadingRows, NoMatch } from '@/components/ui/states';
import { CountNote, ExportCsvButton } from '@/components/ui/table';
import type { ChangeRequest } from '@/lib/api/client';
import { type CsvColumn, exportCsv } from '@/lib/csv';
import { usePolling } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { formatRelative } from '@/lib/time';
import { useUrlFilters } from '@/lib/url-filters';

import { type ChangeRequestState, PENDING } from '../api';
import { STATE_LABEL, stateTone } from '../describe';
import { useChangeRequests } from '../store';
import { ChangeRequestDetail } from './change-request-detail';

type View = 'pending' | 'closed' | 'all';

const DEFAULTS = { view: 'pending', author: '', q: '' };

const CSV: CsvColumn<ChangeRequest>[] = [
  { header: 'id', value: (r) => r.id },
  { header: 'title', value: (r) => r.title },
  { header: 'kind', value: (r) => r.kind },
  { header: 'state', value: (r) => r.state },
  { header: 'author', value: (r) => r.author },
  { header: 'created_at', value: (r) => r.created_at },
  { header: 'base_generation', value: (r) => r.base_generation },
  { header: 'approvals', value: (r) => r.approvals.map((a) => a.by).join(' ') },
  { header: 'approvals_required', value: (r) => r.approvals_required },
  { header: 'expires_at', value: (r) => r.expires_at },
  { header: 'applied_generation', value: (r) => r.applied_generation },
];

function inView(view: View, state: ChangeRequestState): boolean {
  if (view === 'all') return true;
  return PENDING.includes(state) === (view === 'pending');
}

function Row({ cr }: { cr: ChangeRequest }) {
  return (
    <div className="flex items-start gap-3">
      <div className="min-w-0 flex-1">
        <div className="truncate text-body font-semibold text-fg">{cr.title}</div>
        <div className="mt-1 truncate text-caption text-muted">
          {cr.author} · {formatRelative(cr.created_at)}
          {cr.kind === 'action' ? ' · held action' : ''}
        </div>
      </div>
      <div className="flex shrink-0 flex-col items-end gap-1">
        <Badge tone={stateTone(cr.state)} dot>
          {STATE_LABEL[cr.state]}
        </Badge>
        <span className="mono text-caption text-muted tabular-nums">
          {cr.approvals.length}/{cr.approvals_required} approvals
        </span>
      </div>
    </div>
  );
}

/** Change requests: changes waiting for a second person, and what became of the others. */
export function ChangeRequestsScreen() {
  const canRead = usePermission('config:read');
  const list = useChangeRequests((s) => s.list);
  const load = useChangeRequests((s) => s.load);
  const { filters, set, clear, active } = useUrlFilters(DEFAULTS);
  const [selected, setSelected] = useQueryParam('request');
  const view = (['pending', 'closed', 'all'].includes(filters.view) ? filters.view : 'pending') as View;

  useEffect(() => {
    if (canRead) void load();
  }, [canRead, load]);
  usePolling(() => void load({ quiet: true }), canRead);

  const all = useMemo(
    () => [...(list.data ?? [])].sort((a, b) => b.created_at.localeCompare(a.created_at)),
    [list.data],
  );
  const authors = useMemo(() => [...new Set(all.map((r) => r.author))].sort(), [all]);
  const q = filters.q.trim().toLowerCase();
  const visible = all.filter(
    (r) =>
      inView(view, r.state) &&
      (!filters.author || r.author === filters.author) &&
      (!q || r.title.toLowerCase().includes(q) || r.id.toLowerCase().includes(q)),
  );
  const current = all.find((r) => r.id === selected) ?? visible[0];
  const count = (v: View) => all.filter((r) => inView(v, r.state)).length;
  const filtered = Boolean(filters.author || q);

  usePageShortcut('e', () => {
    if (visible.length > 0) exportCsv('change-requests', CSV, visible);
  });

  if (!canRead) return <Forbidden what="change requests" />;

  return (
    <>
      <PageHeader
        title="Change requests"
        description="Changes waiting for a second person, and what became of the others"
        actions={
          <>
            <UpdatedAt at={list.at} />
            <ExportCsvButton filename="change-requests" columns={CSV} rows={visible} />
          </>
        }
      />
      <Tabs<View>
        label="Change requests by state"
        value={view}
        onChange={(v) => set({ view: v })}
        items={[
          { value: 'pending', label: 'Pending', count: count('pending') },
          { value: 'closed', label: 'Closed', count: count('closed') },
          { value: 'all', label: 'All', count: all.length },
        ]}
      />
      {all.length > 0 ? (
        <FilterBar>
          <div className="w-64 max-sm:w-full">
            <Input
              data-page-search
              aria-label="Search change requests"
              icon="search"
              value={filters.q}
              onChange={(e) => set({ q: e.target.value })}
              placeholder="Search by title or id"
            />
          </div>
          {authors.length > 1 || filters.author ? (
            <div className="w-48 max-sm:flex-1">
              <Combobox
                aria-label="Author"
                value={filters.author}
                onChange={(v) => set({ author: v })}
                emptyOption="Any author"
                options={authors.map((a) => ({ value: a, label: a }))}
              />
            </div>
          ) : null}
          {active && filtered ? (
            <button
              type="button"
              className="text-ui text-muted hover:text-fg"
              onClick={() => set({ author: '', q: '' })}
            >
              Clear filters
            </button>
          ) : null}
          <span className="ml-auto">
            <CountNote
              shown={visible.length}
              total={all.length}
              noun="change requests"
              singular="change request"
            />
          </span>
        </FilterBar>
      ) : null}
      {list.status === 'loading' && !list.data ? <LoadingRows /> : null}
      {list.status === 'error' && !list.data ? (
        <ErrorState error={list.error} subject="change requests" onRetry={() => void load()} />
      ) : null}
      {list.data && all.length === 0 ? (
        <EmptyState
          title="No change requests yet"
          description="Changes that need a second approval wait here."
        />
      ) : null}
      {all.length > 0 ? (
        <MasterDetail
          sticky
          narrow={{
            open: Boolean(selected && current),
            onClose: () => setSelected(null),
            title: current?.title ?? '',
          }}
          list={
            visible.length === 0 ? (
              filtered ? (
                <NoMatch onClear={clear} />
              ) : (
                <EmptyState
                  size="sm"
                  title={view === 'pending' ? 'Nothing waiting for review' : 'No closed requests'}
                />
              )
            ) : (
              <SelectableList
                label="Change requests"
                items={visible}
                keyOf={(r) => r.id}
                selectedKey={current?.id ?? null}
                onSelect={(id) => setSelected(id)}
                toneOf={(r) => stateTone(r.state)}
                renderItem={(r) => <Row cr={r} />}
              />
            )
          }
          detail={
            current ? (
              <ChangeRequestDetail key={current.id} request={current} />
            ) : (
              <EmptyState title="Select a change request to see it" />
            )
          }
        />
      ) : null}
      {all.length > 0 ? <KeyboardHints keys={['search', 'move', 'open', 'export']} /> : null}
    </>
  );
}
