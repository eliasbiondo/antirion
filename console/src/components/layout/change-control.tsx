'use client';

import { useRouter } from 'next/navigation';
import { useEffect } from 'react';

import { OpenChangeRequestDialog } from '@/features/change-requests/components/open-change-request-dialog';
import { setChangeControlHandler } from '@/lib/api/client';
import { useShell } from '@/stores/shell';
import { notify, useUi } from '@/stores/ui';

import { SHARED_PARAMS } from './url-sync';

/** The Change requests screen at one request, keeping the page's scope and range. */
export function changeRequestHref(id: string): string {
  const current = new URLSearchParams(window.location.search);
  const next = new URLSearchParams();
  for (const k of SHARED_PARAMS) {
    const v = current.get(k);
    if (v) next.set(k, v);
  }
  next.set('request', id);
  return `/change-requests/?${next.toString()}`;
}

/**
 * Turns change control into next steps: a write it refused (409) offers a change request with the
 * same change; one the two-person rule held (202) says so and links to the request.
 */
export function ChangeControlHost() {
  const router = useRouter();
  const draft = useUi((s) => s.changeDraft);
  const setDraft = useUi((s) => s.setChangeDraft);

  useEffect(() => {
    setChangeControlHandler((event) => {
      if (event.kind === 'required') {
        setDraft(event.attempt);
        return;
      }
      void useShell.getState().loadPending();
      notify('info', 'Approval requested', 'This runs once a second person approves it.', {
        label: 'View request',
        onClick: () => router.push(changeRequestHref(event.request.id)),
      });
    });
    return () => setChangeControlHandler(null);
  }, [router, setDraft]);

  if (!draft) return null;
  return (
    <OpenChangeRequestDialog
      change={draft}
      onClose={() => setDraft(null)}
      onOpened={(id) => {
        setDraft(null);
        notify('success', 'Change request opened', 'A reviewer can now approve it.', {
          label: 'View request',
          onClick: () => router.push(changeRequestHref(id)),
        });
      }}
    />
  );
}
