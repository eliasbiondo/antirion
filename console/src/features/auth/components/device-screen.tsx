'use client';

import Link from 'next/link';
import { useSearchParams } from 'next/navigation';
import { useEffect, useState } from 'react';

import { Button, buttonClass, Spinner } from '@/components/ui/button';
import { Field, Input } from '@/components/ui/form';
import { api, ApiError } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';
import { cn } from '@/lib/cn';
import { formatRelative } from '@/lib/time';
import { useSession } from '@/stores/session';

import { Frame } from './login-screen';
import { ErrorStrip, MoreOptions, Note, StepHeader } from './parts';

type DeviceState = components['schemas']['DeviceAuthorizationState'];

/** A code as the terminal prints it, two groups of four letters (`BCDF-GHJK`), whatever the reader typed. */
export function formatUserCode(typed: string): string {
  const letters = typed.replace(/[\s-]/g, '').toUpperCase();
  return letters.length > 4 ? `${letters.slice(0, 4)}-${letters.slice(4, 8)}` : letters;
}

function message(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 404) return "That code isn't valid or has expired. Check the code in your terminal.";
    if (error.status === 409) return 'This sign-in was already approved or denied.';
    if (error.status === 403) return 'Only a person can approve a sign-in. Sign in with your own account.';
    if (error.status === 429) return 'Too many attempts. Wait a minute, then try again.';
    return error.message;
  }
  return 'Something went wrong. Try again.';
}

/**
 * `/device`: where a person approves a command-line sign-in (the device authorization grant). The
 * code comes from the terminal or the link it printed (`?code=`); the token goes to that terminal
 * only once it is approved here.
 */
export function DeviceScreen() {
  const params = useSearchParams();
  const status = useSession((s) => s.status);
  const me = useSession((s) => s.me);
  const load = useSession((s) => s.load);
  const [code, setCode] = useState(() => formatUserCode(params.get('code') ?? ''));
  const [request, setRequest] = useState<DeviceState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (status === 'unknown') void load();
  }, [status, load]);

  async function run(action: 'check' | 'approve' | 'deny') {
    setBusy(true);
    setError(null);
    try {
      const body = { user_code: code };
      const answer =
        action === 'check'
          ? await api.post('/auth/device:check', { body })
          : action === 'approve'
            ? await api.post('/auth/device:approve', { body })
            : await api.post('/auth/device:deny', { body });
      setRequest(answer);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }

  const next = `/device/?code=${encodeURIComponent(code)}${params.get('sso') === '1' ? '&sso=1' : ''}`;
  const user = me?.user?.display_name ?? me?.user?.name ?? me?.principal.id ?? '';

  let body;
  if (status === 'unknown' || status === 'loading') {
    body = (
      <p className="flex min-h-40 items-center justify-center gap-2 text-ui text-muted" role="status">
        <Spinner /> Loading…
      </p>
    );
  } else if (status !== 'authenticated') {
    body = (
      <>
        <StepHeader
          title="Approve a terminal sign-in"
          subtitle="Sign in first, then approve the code your terminal shows."
        />
        <Link
          href={`/login/?next=${encodeURIComponent(next)}`}
          className={cn(buttonClass('primary'), 'h-10 w-full text-base')}
        >
          Sign in to continue
        </Link>
      </>
    );
  } else if (request && request.status !== 'pending') {
    body = (
      <>
        <StepHeader
          title={request.status === 'approved' ? 'Sign-in approved' : 'Sign-in denied'}
          subtitle={
            request.status === 'approved'
              ? 'Return to your terminal: it now has an admin token for you.'
              : 'The terminal gets no token. You can close this page.'
          }
        />
        <Note icon={request.status === 'approved' ? 'checkCircle' : 'info'}>
          <p>
            Code <span className="mono text-fg">{request.user_code}</span>
            {request.client_ip ? ` from ${request.client_ip}` : ''}.
          </p>
        </Note>
      </>
    );
  } else if (request) {
    body = (
      <>
        <StepHeader
          title="Approve this sign-in?"
          subtitle={
            <span>
              A terminal asks for an admin token as <span className="font-medium text-fg">{user}</span>, with
              your permissions.
            </span>
          }
        />
        <div className="flex flex-col gap-4">
          <ErrorStrip message={error} />
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-ui">
            <dt className="text-muted">Code</dt>
            <dd className="mono text-fg">{request.user_code}</dd>
            <dt className="text-muted">Client</dt>
            <dd className="text-fg">{request.client_id ?? 'Unnamed client'}</dd>
            <dt className="text-muted">From</dt>
            <dd className="mono text-fg">{request.client_ip ?? 'Unknown address'}</dd>
            <dt className="text-muted">Requested</dt>
            <dd className="text-fg">{formatRelative(request.requested_at)}</dd>
            <dt className="text-muted">Expires</dt>
            <dd className="text-fg">{formatRelative(request.expires_at)}</dd>
            {request.organization ? (
              <>
                <dt className="text-muted">Organization</dt>
                <dd className="text-fg">{request.organization}</dd>
              </>
            ) : null}
          </dl>
          <Note icon="alert">
            <p>Approve only a code you started yourself, in a terminal you trust.</p>
          </Note>
          <div className="flex gap-2">
            <Button variant="primary" busy={busy} className="h-10 flex-1" onClick={() => void run('approve')}>
              Approve
            </Button>
            <Button className="h-10 flex-1" disabled={busy} onClick={() => void run('deny')}>
              Deny
            </Button>
          </div>
        </div>
      </>
    );
  } else {
    body = (
      <>
        <StepHeader
          title="Approve a terminal sign-in"
          subtitle="Enter the code your terminal shows after antirion auth login."
        />
        <form
          className="flex flex-col gap-4"
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            if (code.replace(/[\s-]/g, '').length !== 8) {
              setError('Enter the eight letters your terminal shows.');
              return;
            }
            void run('check');
          }}
        >
          <ErrorStrip message={error} />
          <Field label="Code">
            <Input
              mono
              autoComplete="off"
              spellCheck={false}
              autoCapitalize="characters"
              maxLength={9}
              value={code}
              onChange={(e) => setCode(formatUserCode(e.target.value))}
              placeholder="BCDF-GHJK"
              autoFocus
              className="h-11 text-center text-title tracking-[0.2em]"
            />
          </Field>
          <Button type="submit" variant="primary" busy={busy} className="h-10 w-full text-base">
            Continue
          </Button>
        </form>
      </>
    );
  }

  return (
    <Frame footer={null}>
      {body}
      {status === 'authenticated' ? (
        <MoreOptions>
          <Link
            href="/"
            className="self-start text-ui text-muted underline-offset-4 hover:text-fg hover:underline"
          >
            Go to the console
          </Link>
        </MoreOptions>
      ) : null}
    </Frame>
  );
}
