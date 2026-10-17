'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Field, Input, Textarea } from '@/components/ui/form';
import { Dialog } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import type { AttemptedChange } from '@/lib/api/client';
import { useShell } from '@/stores/shell';

import { changeRequestsApi } from '../api';
import { changeLine, titleFor } from '../describe';

export interface OpenChangeRequestDialogProps {
  change: AttemptedChange;
  onClose: () => void;
  /** Called with the new request's id once it is opened. */
  onOpened: (id: string) => void;
}

/**
 * The offer that follows a write change control refused: nothing changed yet, and the same change
 * can go to a reviewer as a change request.
 */
export function OpenChangeRequestDialog({ change, onClose, onOpened }: OpenChangeRequestDialogProps) {
  const [title, setTitle] = useState(() => titleFor(change));
  const [description, setDescription] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const body = change.body === undefined ? null : JSON.stringify(change.body, null, 2);

  async function submit() {
    if (!title.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const created = await changeRequestsApi.open(title.trim(), description.trim(), change);
      void useShell.getState().loadPending();
      onOpened(created.id);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      open
      onClose={onClose}
      title="This change needs a reviewer"
      description="Change control covers it, so nothing has changed yet. Open a change request and someone else can approve and apply it."
      footer={
        <>
          <Button variant="ghost" onClick={onClose} data-autofocus>
            Cancel
          </Button>
          <Button variant="primary" busy={busy} disabled={!title.trim()} onClick={() => void submit()}>
            Open change request
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
        <Field label="Title" error={title.trim() ? undefined : 'Enter a title for the reviewer'}>
          <Input value={title} onChange={(e) => setTitle(e.target.value)} maxLength={200} />
        </Field>
        <Field label="What and why" optional hint="Reviewers see this next to the change.">
          <Textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={3}
            maxLength={5000}
          />
        </Field>
        <div className="flex flex-col gap-1.5">
          <span className="text-caption font-medium text-muted">The change</span>
          <div className="mono rounded-control border border-line bg-sunken px-2.5 py-1.5 text-caption break-all text-fg">
            {changeLine(change)}
          </div>
          {body ? (
            <pre className="mono max-h-48 overflow-auto rounded-control border border-line bg-sunken px-2.5 py-2 text-caption text-muted">
              {body}
            </pre>
          ) : null}
        </div>
        <InlineError error={error} />
      </form>
    </Dialog>
  );
}
