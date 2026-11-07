'use client';

import { useEffect, useMemo, useState } from 'react';

import { Button, IconButton } from '@/components/ui/button';
import { Combobox, Select } from '@/components/ui/form';
import { LoadError } from '@/components/ui/states';
import { describeError, isChangeControl } from '@/lib/api/client';
import { OPERATIONS } from '@/lib/api/operations';
import { scopeOrganization, useScope } from '@/stores/scope';
import { hasPermission, useSession } from '@/stores/session';
import { notify } from '@/stores/ui';

import { attachmentName, type Owner, OWNER_KINDS, type OwnerKind } from '../api';
import { usePlugins } from '../store';
import { translateNow } from '@/lib/i18n';

/** The levels the principal may read, and those it may change. */
export function useOwnerAccess(): { read: OwnerKind[]; write: Set<OwnerKind> } {
  const me = useSession((s) => s.me);
  const scope = useScope((s) => s.scope);
  return useMemo(() => {
    const org = scopeOrganization(scope);
    const may = (op: (typeof OWNER_KINDS)[number]['list']) =>
      hasPermission(me, OPERATIONS[op].permission, org);
    return {
      read: OWNER_KINDS.filter((k) => may(k.list)).map((k) => k.value),
      write: new Set(OWNER_KINDS.filter((k) => may(k.patch)).map((k) => k.value)),
    };
  }, [me, scope]);
}

const LABEL = Object.fromEntries(OWNER_KINDS.map((k) => [k.value, k.label])) as Record<OwnerKind, string>;

/**
 * Where a plugin runs below the gateway: each organization, team, project, key, Model or route
 * whose own policies attach (or detach) it, and a picker to attach it to one more.
 */
export function AttachedTo({ name }: { name: string }) {
  const owners = usePlugins((s) => s.owners);
  const loadOwners = usePlugins((s) => s.loadOwners);
  const attachAt = usePlugins((s) => s.attachAt);
  const access = useOwnerAccess();
  const readKey = access.read.join(',');
  const [kind, setKind] = useState<OwnerKind | ''>('');
  const [target, setTarget] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (readKey) void loadOwners(readKey.split(',') as OwnerKind[]);
  }, [readKey, loadOwners]);

  const list = owners.data ?? [];
  const attached = list.filter((o) => o.attach.some((a) => attachmentName(a) === name));
  const detached = list.filter((o) => o.detach.includes(name));
  const writable = OWNER_KINDS.filter((k) => access.write.has(k.value) && access.read.includes(k.value));
  const candidates = list.filter((o) => o.kind === kind && !attached.includes(o));

  async function change(owner: Owner, on: boolean) {
    setBusy(true);
    try {
      await attachAt(owner, name, on);
      notify(
        'success',
        on ? translateNow('{0} attached', name) : translateNow('{0} taken off', name),
        `${LABEL[owner.kind]} ${owner.name}`,
      );
      if (on) setTarget('');
    } catch (e) {
      if (!isChangeControl(e)) notify('error', translateNow("Couldn't change {0}", name), describeError(e));
    } finally {
      setBusy(false);
    }
  }

  if (access.read.length === 0) return null;
  const locked = (o: Owner) =>
    o.attach.some((a) => attachmentName(a) === name && typeof a === 'object' && a.locked === true);
  return (
    <div className="flex flex-col gap-2" role="group" aria-label="Attached below the gateway">
      <span className="text-ui font-medium text-fg">Attached below the gateway</span>
      <LoadError of={owners} subject="The levels" size="sm" />
      {owners.data && attached.length === 0 ? (
        <p className="text-caption text-muted">
          No organization, team, project, key, Model or route attaches it.
        </p>
      ) : null}
      {attached.length > 0 ? (
        <ul className="flex flex-wrap gap-1.5">
          {attached.map((o) => (
            <li
              key={`${o.kind}/${o.name}`}
              className="inline-flex items-center gap-1 rounded-full border border-line bg-sunken py-0.5 pr-0.5 pl-2.5 text-caption text-fg"
            >
              <span className="text-muted">{LABEL[o.kind]}</span>
              <span className="mono">{o.name}</span>
              {access.write.has(o.kind) && !locked(o) ? (
                <IconButton
                  icon="x"
                  size={14}
                  label={`Take ${name} off ${LABEL[o.kind].toLowerCase()} ${o.name}`}
                  disabled={busy}
                  onClick={() => void change(o, false)}
                />
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
      {detached.length > 0 ? (
        <p className="text-caption text-muted">
          Detached by {detached.map((o) => `${LABEL[o.kind].toLowerCase()} ${o.name}`).join(', ')}.
        </p>
      ) : null}
      {writable.length > 0 && owners.data ? (
        <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,2fr)_auto] items-center gap-2 max-sm:grid-cols-1">
          <Select
            aria-label="Attach to a level"
            value={kind}
            placeholder="Level"
            onChange={(v) => {
              setKind(v as OwnerKind);
              setTarget('');
            }}
            options={writable.map((k) => ({ value: k.value, label: k.label }))}
          />
          <Combobox
            aria-label="Which one"
            value={target}
            disabled={!kind}
            placeholder={kind ? `Choose a ${LABEL[kind].toLowerCase()}` : 'Choose a level first'}
            onChange={setTarget}
            options={candidates.map((o) => ({ value: o.name, label: o.name }))}
          />
          <Button
            size="sm"
            busy={busy}
            disabled={!target}
            onClick={() => {
              const owner = candidates.find((o) => o.name === target);
              if (owner) void change(owner, true);
            }}
          >
            Attach
          </Button>
        </div>
      ) : null}
    </div>
  );
}
