'use client';

import { useState } from 'react';

import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Select } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { scopeOrganization, useScope } from '@/stores/scope';
import { notify } from '@/stores/ui';

import type { InvalidateLevel } from '../api';
import { useCache } from '../store';

const LEVELS: { value: InvalidateLevel; label: string }[] = [
  { value: 'gateway', label: 'The whole gateway' },
  { value: 'organization', label: 'One organization' },
  { value: 'team', label: 'One team' },
  { value: 'project', label: 'One project' },
  { value: 'route', label: 'One route' },
  { value: 'model', label: 'One model' },
];

/**
 * Clears cached responses at a level. Opened for one Model (`fixed`), it clears that Model only;
 * otherwise the operator picks the level. Clearing the whole gateway may need a second person's
 * approval: the dialog says so, and the shell reports the request the gateway opens instead.
 */
export function ClearDialog({
  open,
  onClose,
  fixed,
}: {
  open: boolean;
  onClose: () => void;
  fixed?: { level: InvalidateLevel; name: string } | undefined;
}) {
  const scope = useScope((s) => s.scope);
  const invalidate = useCache((s) => s.invalidate);
  const dualControl = useCache((s) => s.flush.data?.dualControl === true);
  const [picked, setLevel] = useState<InvalidateLevel>(
    scope.kind === 'organization' ? 'organization' : 'gateway',
  );
  const [typedName, setName] = useState(scopeOrganization(scope) ?? '');
  const level = fixed?.level ?? picked;
  const name = fixed?.name ?? typedName;
  const whole = level === 'gateway';
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title={
        whole
          ? 'Clear the whole cache?'
          : fixed
            ? `Clear the cache of ${name}?`
            : `Clear the cache of this ${level}?`
      }
      confirmLabel="Clear cache"
      typeToConfirm={fixed ? undefined : whole ? 'gateway' : name}
      objectName={fixed ? name : undefined}
      effect={
        fixed
          ? 'Its cached responses stop being served at once. Its requests go to the provider until the cache fills again.'
          : 'Every cached response at this level is removed. Requests go to providers until the cache fills again. This can’t be undone.'
      }
      onConfirm={async (reason) => {
        await invalidate(level, whole ? undefined : name, reason);
        notify('success', 'Cache cleared', whole ? 'For the whole gateway.' : `For ${level} ${name}.`);
      }}
    >
      {fixed ? null : (
        <div className="flex flex-col gap-3">
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Clear">
              <Select value={picked} onChange={(v) => setLevel(v as InvalidateLevel)} options={LEVELS} />
            </Field>
            {whole ? null : (
              <Field label={`Name of the ${level}`}>
                <Input mono value={typedName} onChange={(e) => setName(e.target.value)} />
              </Field>
            )}
          </div>
          {whole && dualControl ? (
            <p className="flex items-start gap-2 rounded-control border border-line bg-canvas px-3 py-2 text-caption text-muted">
              <Icon name="info" size={14} className="mt-0.5 shrink-0 text-info" />
              <span>
                A second person approves this. Confirming opens a change request; the cache is cleared once
                another admin approves it.
              </span>
            </p>
          ) : null}
        </div>
      )}
    </ConfirmDialog>
  );
}
