'use client';

import { useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, MultiSelect } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { DetailSection } from '@/components/ui/list-detail';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import type { SelectOption } from '@/components/ui/select';
import { ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { Tooltip } from '@/components/ui/tooltip';
import { api, listAll } from '@/lib/api/client';
import type { ModelSpec } from '@/lib/api/types';
import { plural } from '@/lib/format';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { type AccessRequest, parseScope, SCOPE_NOUN } from '../catalog';
import { decideAccess, patchModelSpec, useCatalog } from '../store';
import type { ModelView } from '../types';
import { useEdited } from '@/lib/hooks';

type Access = NonNullable<ModelSpec['access']>;
type AccessKind = 'organizations' | 'teams' | 'projects' | 'keys';

const KINDS: { kind: AccessKind; label: string; noun: string }[] = [
  { kind: 'organizations', label: 'Organizations', noun: 'organization' },
  { kind: 'teams', label: 'Teams', noun: 'team' },
  { kind: 'projects', label: 'Projects', noun: 'project' },
  { kind: 'keys', label: 'API keys', noun: 'key' },
];

/** The names of one kind of tenant or key, as options; empty when the reader may not list them. */
function useNameOptions(kind: AccessKind, enabled: boolean): SelectOption[] {
  const [options, setOptions] = useState<SelectOption[]>([]);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    const read = (): Promise<{ name: string }[]> => {
      switch (kind) {
        case 'organizations':
          return listAll((cursor) => api.get('/organizations', { query: { limit: 200, cursor } }));
        case 'teams':
          return listAll((cursor) => api.get('/teams', { query: { limit: 200, cursor } }));
        case 'projects':
          return listAll((cursor) => api.get('/projects', { query: { limit: 200, cursor } }));
        case 'keys':
          return listAll((cursor) => api.get('/keys', { query: { limit: 200, cursor } }));
      }
    };
    read().then(
      (items) => live && setOptions(items.map((i) => ({ value: i.name, label: i.name }))),
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, [kind, enabled]);
  return options;
}

/** A Model's access list in words: "2 teams, 1 project", or "No one yet". */
export function accessSummary(access: Access | undefined): string {
  const parts = KINDS.map((k) => ({ n: access?.[k.kind]?.length ?? 0, noun: k.noun })).filter((p) => p.n > 0);
  return parts.length === 0 ? 'No one yet' : parts.map((p) => plural(p.n, p.noun)).join(', ');
}

function KindField({
  kind,
  label,
  noun,
  value,
  onChange,
}: {
  kind: AccessKind;
  label: string;
  noun: string;
  value: string[];
  onChange: (v: string[]) => void;
}) {
  const options = useNameOptions(kind, true);
  return (
    <Field label={label} optional>
      <MultiSelect
        mono
        creatable
        noun={noun}
        placeholder={`Choose ${noun === 'key' ? 'keys' : `${noun}s`}`}
        options={options}
        value={value}
        onChange={onChange}
      />
    </Field>
  );
}

/** Edits who may use a preview or restricted Model: organizations, teams, projects and keys. */
export function AccessDrawer({ view, onClose }: { view: ModelView; onClose: () => void }) {
  const access = view.model.spec.access ?? {};
  const [values, setValues] = useState<Record<AccessKind, string[]>>({
    organizations: access.organizations ?? [],
    teams: access.teams ?? [],
    projects: access.projects ?? [],
    keys: access.keys ?? [],
  });
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  async function save() {
    setBusy(true);
    setError(null);
    try {
      await patchModelSpec(view.name, { access: values });
      notify('success', 'Access saved', view.name);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      title={`Access to ${view.name}`}
      subtitle={
        view.lifecycle === 'restricted'
          ? 'Only these can use it; others ask for access.'
          : 'Only these can use it while it is in preview.'
      }
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void save()}>
            Save access
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <InlineError error={error} />
        <p className="text-ui text-muted">
          A request is let through when its key, or the project, team or organization the key belongs to, is
          listed here. The key’s own model list still applies.
        </p>
        {KINDS.map((k) => (
          <KindField
            key={k.kind}
            kind={k.kind}
            label={k.label}
            noun={k.noun}
            value={values[k.kind]}
            onChange={(v) => setValues((s) => ({ ...s, [k.kind]: v }))}
          />
        ))}
      </div>
    </Drawer>
  );
}

function ScopeLabel({ scope }: { scope: string }) {
  const s = parseScope(scope);
  if (!s) return <span className="mono">{scope}</span>;
  return (
    <span>
      <span className="text-muted">{SCOPE_NOUN[s.kind]} </span>
      <span className="mono text-fg">{s.name}</span>
    </span>
  );
}

/**
 * Who may use a preview or restricted Model, and the queue of requests for access: open ones with
 * approve and deny (an approval adds the scope to the access list), then the decided ones.
 */
export function AccessPanel({ view, onEdit }: { view: ModelView; onEdit: () => void }) {
  const timeZone = usePreferences((s) => s.timeZone);
  const slot = useCatalog((s) => s.access[view.name]);
  const loadAccess = useCatalog((s) => s.loadAccess);
  const canRead = useCan('listAccessRequests');
  const canDecide = useCan('approveAccessRequest');
  const canEdit = useCan('patchModel');
  const [decision, setDecision] = useState<{ request: AccessRequest; verb: 'approve' | 'deny' } | null>(null);
  const [showDecided, setShowDecided] = useState(false);
  const gated = view.lifecycle === 'preview' || view.lifecycle === 'restricted';
  useEffect(() => {
    if (canRead) void loadAccess(view.name);
  }, [canRead, loadAccess, view.name]);
  const requests = slot?.data ?? [];
  const open = requests.filter((r) => r.state === 'open');
  const decided = requests.filter((r) => r.state !== 'open');
  if (!gated && requests.length === 0) return null;
  const access = view.model.spec.access ?? {};

  return (
    <DetailSection
      title="Access"
      aside={
        gated && canEdit ? (
          <Button size="sm" icon="edit" onClick={onEdit}>
            Edit access
          </Button>
        ) : null
      }
    >
      {gated ? (
        <dl className="grid gap-x-6 gap-y-1.5 text-ui sm:grid-cols-[120px_minmax(0,1fr)]">
          {KINDS.map((k) => (
            <div key={k.kind} className="contents">
              <dt className="text-muted">{k.label}</dt>
              <dd className="mono min-w-0 truncate text-fg">
                {(access[k.kind] ?? []).length > 0 ? (
                  (access[k.kind] ?? []).join(', ')
                ) : (
                  <span className="text-dim">—</span>
                )}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-ui text-muted">
          Every app can use this model, so its access list doesn’t apply. Requests made while it was gated
          stay listed.
        </p>
      )}
      <div className="mt-4">
        <div className="mb-1.5 flex items-center gap-2 text-caption text-muted">
          Requests for access
          {open.length > 0 ? <Badge tone="warn">{open.length} waiting</Badge> : null}
        </div>
        {!canRead ? <p className="text-ui text-muted">You need to read models to see requests.</p> : null}
        {slot?.status === 'loading' && !slot.data ? <LoadingRows rows={2} /> : null}
        {slot?.error ? <ErrorState error={slot.error} subject="Access requests" size="sm" /> : null}
        {slot?.data && open.length === 0 ? (
          <p className="text-ui text-muted">
            No one is waiting. Teams ask for access from their own tools or the API.
          </p>
        ) : null}
        <ul className="flex flex-col">
          {open.map((r) => (
            <li
              key={r.id}
              className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-line py-2 text-ui last:border-0"
            >
              <ScopeLabel scope={r.scope} />
              <span className="min-w-0 flex-1 truncate text-muted">
                {r.reason ? `“${r.reason}”` : 'No reason given'} · by {r.requested_by}
              </span>
              <Tooltip content={formatDateTime(r.requested_at, timeZone)}>
                <span className="text-caption text-dim">{formatRelative(r.requested_at)}</span>
              </Tooltip>
              {canDecide ? (
                <span className="flex gap-1.5">
                  <Button
                    size="sm"
                    variant="primary"
                    onClick={() => setDecision({ request: r, verb: 'approve' })}
                  >
                    Approve
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => setDecision({ request: r, verb: 'deny' })}>
                    Deny
                  </Button>
                </span>
              ) : null}
            </li>
          ))}
        </ul>
        {decided.length > 0 ? (
          <div className="mt-2">
            <Button
              size="sm"
              variant="ghost"
              iconAfter={showDecided ? 'chevronUp' : 'chevronDown'}
              onClick={() => setShowDecided((v) => !v)}
            >
              {plural(decided.length, 'decided request')}
            </Button>
            {showDecided ? (
              <ul className="mt-1 flex flex-col">
                {decided.map((r) => (
                  <li
                    key={r.id}
                    className="flex flex-wrap items-center gap-x-3 border-b border-line py-1.5 text-ui last:border-0"
                  >
                    <Badge tone={r.state === 'approved' ? 'success' : 'error'}>
                      {r.state === 'approved' ? 'Approved' : 'Denied'}
                    </Badge>
                    <ScopeLabel scope={r.scope} />
                    <span className="min-w-0 flex-1 truncate text-muted">
                      {r.decided_by ? `by ${r.decided_by}` : ''}
                    </span>
                    <span className="text-caption text-dim">
                      {formatRelative(r.decided_at ?? r.requested_at)}
                    </span>
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : null}
      </div>
      {decision ? (
        <ConfirmDialog
          open
          onClose={() => setDecision(null)}
          tone={decision.verb === 'approve' ? 'primary' : 'danger'}
          title={
            decision.verb === 'approve' ? `Approve access to ${view.name}?` : `Deny access to ${view.name}?`
          }
          confirmLabel={decision.verb === 'approve' ? 'Approve access' : 'Deny access'}
          effect={
            decision.verb === 'approve' ? (
              <>
                <ScopeLabel scope={decision.request.scope} /> is added to the model’s access list and can use
                it right away.
              </>
            ) : (
              <>
                The request from <ScopeLabel scope={decision.request.scope} /> is closed; they can ask again.
              </>
            )
          }
          onConfirm={async (reason) => {
            await decideAccess(view.name, decision.request.id, decision.verb, reason);
            notify(
              'success',
              decision.verb === 'approve' ? 'Access approved' : 'Access denied',
              decision.request.scope,
            );
          }}
        />
      ) : null}
    </DetailSection>
  );
}
