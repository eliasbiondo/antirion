'use client';

import Link from 'next/link';
import { useSearchParams } from 'next/navigation';
import { useEffect, useRef, useState } from 'react';

import { buttonClass, Spinner } from '@/components/ui/button';
import { ApiError } from '@/lib/api/client';
import { cn } from '@/lib/cn';
import { useSession } from '@/stores/session';

import { authApi } from '../api';
import { Frame } from './login-screen';
import { ErrorStrip, Note, StepHeader } from './parts';

type Outcome =
  | { kind: 'checking' }
  | { kind: 'missing' }
  | { kind: 'sign-in' }
  | { kind: 'not-a-user' }
  | { kind: 'done'; email: string }
  | { kind: 'dead' }
  | { kind: 'failed'; message: string };

const PRIMARY = cn(buttonClass('primary'), 'h-10 w-full text-base');

/**
 * The landing page of the link mailed to a new email address: it confirms the change for the
 * signed-in user that asked for it, or asks them to sign in first and comes back here.
 */
export function ConfirmEmailScreen() {
  const params = useSearchParams();
  const token = params.get('token') ?? '';
  const status = useSession((s) => s.status);
  const me = useSession((s) => s.me);
  const load = useSession((s) => s.load);
  const adopt = useSession((s) => s.adopt);
  const [result, setResult] = useState<Outcome | null>(null);
  const sent = useRef(false);

  useEffect(() => {
    if (token) void load();
  }, [token, load]);

  const user = status === 'authenticated' && me?.principal.type === 'user';
  useEffect(() => {
    if (!token || !user || sent.current) return;
    sent.current = true;
    authApi.confirmEmail(token).then(
      (updated) => {
        adopt(updated);
        setResult({ kind: 'done', email: updated.user?.email ?? '' });
      },
      (e: unknown) => {
        if (e instanceof ApiError && e.code === 'invalid_parameter') setResult({ kind: 'dead' });
        else
          setResult({
            kind: 'failed',
            message:
              e instanceof ApiError && e.status === 0
                ? "Can't reach the gateway. Check your connection and reload this page."
                : 'Something went wrong. Reload this page to try again.',
          });
      },
    );
  }, [token, user, adopt]);

  const outcome: Outcome = !token
    ? { kind: 'missing' }
    : (result ??
      (status === 'anonymous' || status === 'expired'
        ? { kind: 'sign-in' }
        : status === 'authenticated' && !user
          ? { kind: 'not-a-user' }
          : { kind: 'checking' }));

  const here = `/confirm-email/?token=${encodeURIComponent(token)}`;
  return (
    <Frame footer={null}>
      {outcome.kind === 'checking' ? (
        <>
          <StepHeader title="Confirm your new email" />
          <p className="flex items-center gap-2 text-ui text-muted" role="status">
            <Spinner /> Confirming…
          </p>
        </>
      ) : outcome.kind === 'missing' ? (
        <>
          <StepHeader title="This link is incomplete" />
          <div className="flex flex-col gap-4">
            <p className="text-body leading-relaxed text-fg">
              Part of the link is missing. Open it again from the email, or copy the whole address into your
              browser.
            </p>
            <Link href="/" className={PRIMARY}>
              Go to Antirion
            </Link>
          </div>
        </>
      ) : outcome.kind === 'sign-in' ? (
        <>
          <StepHeader
            title="Sign in to confirm your new email"
            subtitle="Sign in to the account that asked for the change; you'll come back here to finish."
          />
          <Link href={`/login/?next=${encodeURIComponent(here)}`} className={PRIMARY}>
            Sign in
          </Link>
        </>
      ) : outcome.kind === 'not-a-user' ? (
        <>
          <StepHeader title="Sign in with your own account" />
          <div className="flex flex-col gap-4">
            <p className="text-body leading-relaxed text-fg">
              You&apos;re signed in with an admin token, which has no email. Sign out, then sign in to the
              account that asked for the change and open the link again.
            </p>
            <Link href="/" className={PRIMARY}>
              Go to Antirion
            </Link>
          </div>
        </>
      ) : outcome.kind === 'done' ? (
        <>
          <StepHeader title="Your email is confirmed" />
          <div className="flex flex-col gap-4">
            <p role="status" className="text-body leading-relaxed text-fg">
              {outcome.email ? (
                <>
                  Your account now uses <span className="font-medium">{outcome.email}</span>. Sign in with it
                  from now on.
                </>
              ) : (
                'Your account uses the new address. Sign in with it from now on.'
              )}
            </p>
            <Link href="/" className={PRIMARY}>
              Continue to Antirion
            </Link>
          </div>
        </>
      ) : outcome.kind === 'dead' ? (
        <>
          <StepHeader title="This link no longer works" />
          <div className="flex flex-col gap-4">
            <p className="text-body leading-relaxed text-fg">
              A confirmation link works once, for a short time, and only for the account that asked for the
              change. Change your email again from your profile to get a new link.
            </p>
            {me?.user?.email ? (
              <Note>
                You&apos;re signed in as <span className="font-medium text-fg">{me.user.email}</span>.
              </Note>
            ) : null}
            <Link href="/?account=profile" className={PRIMARY}>
              Open your profile
            </Link>
          </div>
        </>
      ) : (
        <>
          <StepHeader title="Confirm your new email" />
          <ErrorStrip message={outcome.message} />
        </>
      )}
    </Frame>
  );
}
