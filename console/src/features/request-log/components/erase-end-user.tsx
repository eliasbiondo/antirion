'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { MenuButton } from '@/components/ui/menu';
import { api } from '@/lib/api/client';
import { plural } from '@/lib/format';
import { scopeOrganization, useScope } from '@/stores/scope';
import { notify } from '@/stores/ui';

import { useLogs } from '../store';

/** Deletes every request record of one end user in the caller's scope; returns how many went. */
export async function eraseEndUser(endUser: string, reason?: string): Promise<number> {
  const { deleted } = await api.delete('/logs', { query: { end_user: endUser }, reason });
  useLogs.setState((s) => ({
    records: { ...s.records, data: s.records.data?.filter((r) => r.end_user !== endUser) ?? null },
    incoming: s.incoming.filter((r) => r.end_user !== endUser),
  }));
  return deleted;
}

/**
 * "Erase an end user's records": what is deleted and what stays, the end user typed to confirm, and
 * an optional reason for the audit log. `endUser` set (from a record) shows it and asks to type it.
 */
export function EraseEndUserDialog({
  open,
  endUser,
  onClose,
  onErased,
}: {
  open: boolean;
  endUser: string | null;
  onClose: () => void;
  onErased?: (endUser: string) => void;
}) {
  const scope = useScope((s) => s.scope);
  const [typed, setTyped] = useState('');
  const organization = scopeOrganization(scope);
  const where = organization ? `in organization ${organization}` : 'across the whole gateway';
  const target = (endUser ?? typed).trim();
  return (
    <ConfirmDialog
      open={open}
      onClose={() => {
        setTyped('');
        onClose();
      }}
      title={endUser ? `Erase the records of ${endUser}?` : "Erase an end user's records?"}
      objectName={endUser ?? undefined}
      typeToConfirm={endUser ?? undefined}
      confirmLabel="Erase records"
      effect={
        <>
          Every request record of this end user {where} is deleted, with its prompts and responses, and
          can&apos;t be restored. Cost ledger lines and usage figures stay, since they hold no content. The
          audit log records the erasure.
        </>
      }
      onConfirm={async (reason) => {
        if (!target) throw new Error('Enter the end user ID, as your app sends it.');
        const deleted = await eraseEndUser(target, reason);
        setTyped('');
        notify(
          'success',
          deleted > 0 ? `${plural(deleted, 'record')} erased` : 'Nothing to erase',
          deleted > 0 ? `The records of ${target} are gone.` : `No records of ${target} were found ${where}.`,
        );
        onErased?.(target);
      }}
    >
      {endUser ? null : (
        <Field
          label="End user ID"
          required
          hint="The ID your app sends as the end user (the user field or x-antirion-end-user)."
        >
          <Input
            mono
            autoComplete="off"
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            data-autofocus
          />
        </Field>
      )}
    </ConfirmDialog>
  );
}

/** Next to a record's end user: erase every record of that end user (with `logs:delete`). */
export function EraseEndUserButton({ endUser, onErased }: { endUser: string; onErased?: () => void }) {
  const canErase = useCan('deleteEndUserLogs');
  const [open, setOpen] = useState(false);
  if (!canErase) return null;
  return (
    <>
      <Button
        size="sm"
        variant="ghost"
        icon="eraser"
        className="ml-2 font-sans"
        onClick={() => setOpen(true)}
      >
        Erase records
      </Button>
      <EraseEndUserDialog
        open={open}
        endUser={endUser}
        onClose={() => setOpen(false)}
        onErased={() => onErased?.()}
      />
    </>
  );
}

/** The request log header's "More" menu: erase an end user's records by ID. */
export function RequestLogMoreMenu() {
  const canErase = useCan('deleteEndUserLogs');
  const [open, setOpen] = useState(false);
  if (!canErase) return null;
  return (
    <>
      <MenuButton
        label="More actions"
        icon="more"
        iconOnly
        variant="ghost"
        items={[
          {
            label: "Erase an end user's records",
            icon: 'eraser',
            danger: true,
            onSelect: () => setOpen(true),
          },
        ]}
      />
      <EraseEndUserDialog open={open} endUser={null} onClose={() => setOpen(false)} />
    </>
  );
}
