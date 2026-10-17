'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Switch } from '@/components/ui/form';
import { Dialog } from '@/components/ui/overlay';
import { SavedViews } from '@/components/ui/saved-views';
import { InlineError } from '@/components/ui/states';
import type { LogView, Me } from '@/lib/api/types';
import { isGatewayPrincipal, useSession } from '@/stores/session';
import { notify } from '@/stores/ui';

import { resolveColumns } from '../columns';
import { useLogColumnsPreference } from '../hooks';
import { useLogs } from '../store';
import {
  decodeSort,
  EMPTY_LOG_FILTERS,
  encodeSort,
  filtersFromView,
  type LogFilters,
  sameFilters,
  viewFilters,
} from '../types';

export interface BuiltInView {
  id: string;
  name: string;
  starred: boolean;
  filters: LogFilters;
}

/** The views every gateway has; read-only, listed before the saved ones. */
export const BUILT_IN_VIEWS: BuiltInView[] = [
  { id: 'all', name: 'All requests', starred: true, filters: EMPTY_LOG_FILTERS },
  {
    id: 'errors',
    name: 'Errors',
    starred: true,
    filters: { ...EMPTY_LOG_FILTERS, statusClass: ['4xx', '5xx'] },
  },
  {
    id: 'slow',
    name: 'Slower than 2 s',
    starred: true,
    filters: { ...EMPTY_LOG_FILTERS, latencyMin: '2000' },
  },
  {
    id: 'expensive',
    name: 'Costlier than $0.05',
    starred: false,
    filters: { ...EMPTY_LOG_FILTERS, costMin: '0.05' },
  },
  {
    id: 'flagged',
    name: 'Flagged by guardrails',
    starred: false,
    filters: { ...EMPTY_LOG_FILTERS, flagged: true },
  },
  { id: 'misses', name: 'Cache misses', starred: false, filters: { ...EMPTY_LOG_FILTERS, cache: ['miss'] } },
];

/** Who the gateway stores a view under for this reader: the user, else the credential. */
function ownerName(me: Me | null): string | null {
  return me?.user?.name ?? me?.principal.id ?? null;
}

/** Whether the reader may change a view: its creator, a gateway admin, or an org admin of its organization. */
function mayChange(me: Me | null, view: LogView): boolean {
  if (!me) return false;
  if (view.created_by === ownerName(me) || isGatewayPrincipal(me)) return true;
  return (
    view.organization != null &&
    me.bindings.some((b) => b.role === 'org_admin' && b.scope === `organization/${view.organization}`)
  );
}

/** The current filters, sort and columns, as a view's members. */
function current(columns: string[] | null) {
  const s = useLogs.getState();
  const sort = encodeSort(s.sort, s.order);
  return {
    filters: viewFilters(s.filters),
    ...(sort ? { sort } : { sort: 'started_at:desc' }),
    columns: columns ?? [],
  };
}

function NameDialog({
  open,
  title,
  initial,
  initialShared,
  confirmLabel,
  withShare,
  onClose,
  onSubmit,
}: {
  open: boolean;
  title: string;
  initial: string;
  initialShared: boolean;
  confirmLabel: string;
  withShare: boolean;
  onClose: () => void;
  onSubmit: (name: string, shared: boolean) => Promise<void>;
}) {
  const [name, setName] = useState(initial);
  const [shared, setShared] = useState(initialShared);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const trimmed = name.trim();
  const invalid = trimmed.length === 0 || trimmed.length > 100;
  async function submit() {
    if (invalid) return;
    setBusy(true);
    setError(null);
    try {
      await onSubmit(trimmed, shared);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      open={open}
      onClose={onClose}
      size="sm"
      title={title}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" busy={busy} disabled={invalid} onClick={() => void submit()}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Field label="Name" error={trimmed.length > 100 ? 'At most 100 characters.' : undefined}>
          <Input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g. Support errors"
          />
        </Field>
        {withShare ? (
          <Switch checked={shared} onChange={setShared} label="Share with the organization" />
        ) : null}
        {withShare ? (
          <p className="-mt-2 text-caption text-muted">
            The view keeps these filters, the sort and the columns. Shared, everyone in the organization sees
            it; only you and admins can change it.
          </p>
        ) : null}
        <InlineError error={error} />
      </form>
    </Dialog>
  );
}

type Editing =
  { kind: 'create' } | { kind: 'rename'; view: LogView } | { kind: 'delete'; view: LogView } | null;

/**
 * The rail's saved views: the built-in ones (read-only) and the gateway's, starred first. A view
 * restores its filters, sort and columns; the reader stars any view they see, and the creator
 * renames, updates, shares and deletes their own.
 */
export function LogViews({ className }: { className?: string }) {
  const me = useSession((s) => s.me);
  const views = useLogs((s) => s.views.data) ?? [];
  const filters = useLogs((s) => s.filters);
  const sort = useLogs((s) => s.sort);
  const order = useLogs((s) => s.order);
  const setFilters = useLogs((s) => s.setFilters);
  const setSortOrder = useLogs((s) => s.setSortOrder);
  const createView = useLogs((s) => s.createView);
  const updateView = useLogs((s) => s.updateView);
  const deleteView = useLogs((s) => s.deleteView);
  const [columns, setColumns] = useLogColumnsPreference();
  const [editing, setEditing] = useState<Editing>(null);
  const me_ = ownerName(me);

  const encodedSort = encodeSort(sort, order) ?? 'started_at:desc';
  const isActive = (v: LogView) => {
    const s = decodeSort(v.sort);
    const cols = v.columns?.length ? resolveColumns(v.columns) : null;
    return (
      sameFilters(filtersFromView(v.filters), filters) &&
      (encodeSort(s.sort, s.order) ?? 'started_at:desc') === encodedSort &&
      (cols === null || JSON.stringify(cols) === JSON.stringify(resolveColumns(columns)))
    );
  };

  function apply(id: string) {
    const builtIn = BUILT_IN_VIEWS.find((v) => v.id === id);
    if (builtIn) {
      setFilters(builtIn.filters);
      setSortOrder('started_at', 'desc');
      return;
    }
    const view = views.find((v) => `view-${v.id}` === id);
    if (!view) return;
    setFilters(filtersFromView(view.filters));
    const s = decodeSort(view.sort);
    setSortOrder(s.sort, s.order);
    if (view.columns?.length) setColumns(resolveColumns(view.columns));
  }

  async function toggleStar(id: string) {
    const view = views.find((v) => `view-${v.id}` === id);
    if (!view) return;
    try {
      await updateView(view.id, { starred: !view.starred });
    } catch {
      notify('error', `Couldn't ${view.starred ? 'unstar' : 'star'} ${view.name}`);
    }
  }

  async function change(view: LogView, body: Parameters<typeof updateView>[1], done: string) {
    try {
      await updateView(view.id, body);
      notify('success', done);
    } catch {
      notify('error', `Couldn't change ${view.name}`);
    }
  }

  return (
    <>
      <SavedViews
        className={className}
        views={[
          ...BUILT_IN_VIEWS.map((v) => ({
            id: v.id,
            name: v.name,
            starred: v.starred,
            starrable: false,
            active: sameFilters(v.filters, filters) && encodedSort === 'started_at:desc',
          })),
          ...views.map((v) => ({
            id: `view-${v.id}`,
            name: v.name,
            starred: v.starred,
            shared: v.shared,
            active: isActive(v),
            note: v.created_by !== me_ ? `Shared by ${v.created_by}` : undefined,
          })),
        ]}
        onSelect={apply}
        onToggleStar={(id) => void toggleStar(id)}
        menu={(id) => {
          const view = views.find((v) => `view-${v.id}` === id);
          if (!view || !mayChange(me, view)) return [];
          return [
            {
              label: 'Update with the current filters',
              icon: 'refresh',
              onSelect: () =>
                void change(view, current(columns), `${view.name} now shows the current filters`),
            },
            { label: 'Rename', icon: 'edit', onSelect: () => setEditing({ kind: 'rename', view }) },
            view.shared
              ? {
                  label: 'Stop sharing',
                  icon: 'lock',
                  onSelect: () => void change(view, { shared: false }, `${view.name} is now only yours`),
                }
              : {
                  label: 'Share with the organization',
                  icon: 'users',
                  onSelect: () =>
                    void change(view, { shared: true }, `${view.name} is shared with the organization`),
                },
            {
              label: 'Delete',
              icon: 'trash',
              danger: true,
              onSelect: () => setEditing({ kind: 'delete', view }),
            },
          ];
        }}
        action={
          <button
            type="button"
            onClick={() => setEditing({ kind: 'create' })}
            className="rounded-xs text-caption text-muted hover:text-fg"
          >
            Save view
          </button>
        }
      />
      {editing?.kind === 'create' ? (
        <NameDialog
          open
          title="Save the current view"
          initial=""
          initialShared={false}
          confirmLabel="Save view"
          withShare
          onClose={() => setEditing(null)}
          onSubmit={async (name, shared) => {
            await createView({ name, shared, starred: true, ...current(columns) });
            notify('success', `Saved ${name}`);
          }}
        />
      ) : null}
      {editing?.kind === 'rename' ? (
        <NameDialog
          open
          title={`Rename ${editing.view.name}`}
          initial={editing.view.name}
          initialShared={editing.view.shared}
          confirmLabel="Rename view"
          withShare={false}
          onClose={() => setEditing(null)}
          onSubmit={async (name) => {
            await updateView(editing.view.id, { name });
          }}
        />
      ) : null}
      {editing?.kind === 'delete' ? (
        <ConfirmDialog
          open
          onClose={() => setEditing(null)}
          title={`Delete view ${editing.view.name}?`}
          objectName={editing.view.name}
          effect={
            editing.view.shared
              ? 'It disappears for everyone in the organization it is shared with. The requests stay.'
              : 'Only the saved view goes; the requests stay.'
          }
          confirmLabel="Delete view"
          withReason={false}
          onConfirm={async () => {
            await deleteView(editing.view.id);
            notify('success', `Deleted ${editing.view.name}`);
          }}
        />
      ) : null}
    </>
  );
}
