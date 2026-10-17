'use client';

import { useState, type ReactNode } from 'react';

import { describeError, isChangeControl } from '@/lib/api/client';
import { notify } from '@/stores/ui';

import { Button } from './button';
import { Field, Input, Textarea } from './form';
import { Icon } from './icon';
import { Dialog } from './overlay';

export interface ConfirmDialogProps {
  open: boolean;
  onClose: () => void;
  /** A question that names the action and the object: "Revoke key support-bot?". */
  title: string;
  /** What happens, in plain words: "Apps using this key will stop working immediately." */
  effect: ReactNode;
  /** The outcome as a verb, repeating the title's: "Revoke key". Never "OK" or "Yes". */
  confirmLabel: string;
  /** The way back, when "Cancel" would be unclear ("Keep editing" when the question is to discard). */
  cancelLabel?: string;
  /** The object's name, shown in the body so it cannot be mistaken ("support-bot"). */
  objectName?: string | undefined;
  /**
   * When set, the operator must type this (the object's name) to confirm. Only for irreversible
   * actions with a wide impact (deleting an organization, revoking a key with traffic).
   */
  typeToConfirm?: string | undefined;
  /** Offers an optional reason for the audit log, behind a link. */
  withReason?: boolean;
  /** `danger` (the default) fills the confirm button red; `primary` for a safe confirmation. */
  tone?: 'danger' | 'primary';
  onConfirm: (reason: string | undefined) => Promise<void>;
  children?: ReactNode;
}

/**
 * Confirmation of a consequential action: it names the object and the outcome, focus starts on
 * Cancel (or on the name to type), and the confirm button says what will happen.
 */
export function ConfirmDialog({
  open,
  onClose,
  title,
  effect,
  confirmLabel,
  cancelLabel = 'Cancel',
  objectName,
  typeToConfirm,
  withReason = true,
  tone = 'danger',
  onConfirm,
  children,
}: ConfirmDialogProps) {
  const [typed, setTyped] = useState('');
  const [reason, setReason] = useState('');
  const [reasonOpen, setReasonOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function close() {
    setTyped('');
    setReason('');
    setReasonOpen(false);
    setError(null);
    onClose();
  }

  const blocked = typeToConfirm !== undefined && typed !== typeToConfirm;

  async function submit() {
    if (blocked) return;
    setBusy(true);
    setError(null);
    try {
      await onConfirm(reason.trim() || undefined);
      close();
    } catch (e) {
      // A change request opened or offered instead: the shell says so, and this dialog is done.
      if (isChangeControl(e)) close();
      else setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      open={open}
      onClose={close}
      title={title}
      role="alertdialog"
      size="sm"
      footer={
        <>
          <Button
            variant="ghost"
            onClick={close}
            data-autofocus={typeToConfirm === undefined ? true : undefined}
          >
            {cancelLabel}
          </Button>
          <Button
            variant={tone === 'danger' ? 'danger-solid' : 'primary'}
            busy={busy}
            disabled={blocked}
            disabledReason={typeToConfirm !== undefined ? `Type ${typeToConfirm} to confirm` : undefined}
            onClick={() => void submit()}
          >
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
        {objectName ? (
          <div className="mono w-fit max-w-full truncate rounded-control border border-line bg-sunken px-2 py-1 text-ui text-fg">
            {objectName}
          </div>
        ) : null}
        <div className="text-body text-fg">{effect}</div>
        {children}
        {typeToConfirm !== undefined ? (
          <Field
            label={
              <>
                Type <span className="mono font-semibold text-fg">{typeToConfirm}</span> to confirm
              </>
            }
          >
            <Input
              mono
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              autoComplete="off"
              data-autofocus
            />
          </Field>
        ) : null}
        {withReason && reasonOpen ? (
          <Field label="Reason" optional hint="Saved in the audit log.">
            <Textarea
              value={reason}
              onChange={(e) => setReason(e.target.value)}
              rows={2}
              maxLength={500}
              autoFocus
            />
          </Field>
        ) : null}
        {withReason && !reasonOpen ? (
          <button
            type="button"
            onClick={() => setReasonOpen(true)}
            className="flex w-fit items-center gap-1.5 rounded-xs text-ui text-muted hover:text-fg"
          >
            <Icon name="plus" size={14} />
            Add a reason for the audit log
          </button>
        ) : null}
        {error ? (
          <p role="alert" className="flex items-start gap-1.5 text-ui text-error">
            <Icon name="alertCircle" size={16} className="mt-px shrink-0" />
            {error}
          </p>
        ) : null}
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Dialog>
  );
}

/** A switch's change: how to make it, how to take it back, and what to say about both. */
export interface SwitchChange {
  /** The thing switched ("audit-plugin"), named in the toasts. */
  name: string;
  apply: () => Promise<void>;
  undo: () => Promise<void>;
  /** "audit-plugin runs on every request". */
  done: string;
  /** "audit-plugin detached". */
  undone: string;
  /** With a question, the change waits for a confirmation that says what it will do. */
  ask?: { title: string; effect: ReactNode; confirmLabel: string } | undefined;
}

/**
 * Row switches that act at once: each change says what it did with an Undo, and one that puts
 * something in the path of every request asks first (as turning a provider off does).
 */
export function useSwitchChanges(): { change: (c: SwitchChange) => void; dialog: ReactNode } {
  const [pending, setPending] = useState<SwitchChange | null>(null);
  const failed = (c: SwitchChange) => (e: unknown) =>
    notify('error', `Couldn't change ${c.name}`, describeError(e));
  const run = async (c: SwitchChange) => {
    await c.apply();
    notify('success', c.done, undefined, {
      label: 'Undo',
      onClick: () => void c.undo().then(() => notify('success', c.undone), failed(c)),
    });
  };
  const change = (c: SwitchChange) => {
    if (c.ask) setPending(c);
    else void run(c).catch(failed(c));
  };
  const dialog = (
    <ConfirmDialog
      open={pending !== null}
      onClose={() => setPending(null)}
      title={pending?.ask?.title ?? ''}
      effect={pending?.ask?.effect}
      confirmLabel={pending?.ask?.confirmLabel ?? 'Confirm'}
      tone="primary"
      withReason={false}
      onConfirm={async () => {
        if (pending) await run(pending);
      }}
    />
  );
  return { change, dialog };
}
