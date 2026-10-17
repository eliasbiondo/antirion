'use client';

import { useSyncExternalStore } from 'react';

import { CopyButton } from '@/components/ui/misc';

import { useLogin } from '../store';
import { MoreOptions, Note, StepHeader, TextButton } from './parts';

function noSubscription(): () => void {
  return () => undefined;
}

/** The admin listener's address as the browser reaches it: the command signs in against the same one. */
function useOrigin(): string {
  return useSyncExternalStore(
    noSubscription,
    () => window.location.origin,
    () => '',
  );
}

/** `antirion auth login` for this gateway, with the organization chosen on the sign-in page. */
export function loginCommand(origin: string, organization: string, sso: boolean): string {
  const parts = ['antirion auth login', `--admin-url ${origin}`];
  if (sso) parts.push('--sso');
  if (organization) parts.push(`--organization ${organization}`);
  return parts.join(' ');
}

/** The terminal variant of the sign-in page: how the command line signs in with a code approved here. */
export function CliStep() {
  const organization = useLogin((s) => s.organization);
  const options = useLogin((s) => s.options);
  const go = useLogin((s) => s.go);
  const origin = useOrigin();
  const sso = (options?.methods ?? []).some(
    (m) => m.type === 'oidc' || m.type === 'saml' || m.type === 'oauth',
  );
  const command = loginCommand(origin, organization, false);
  const uiPath = process.env.NEXT_PUBLIC_UI_PATH ?? '';

  return (
    <>
      <StepHeader
        title="Sign in from a terminal"
        subtitle="The command line gets its own admin token once you approve its code in this console."
      />
      <div className="flex flex-col gap-4">
        <div className="overflow-hidden rounded-sm border border-line bg-sunken">
          <div className="flex items-center justify-between gap-2 border-b border-line px-3 py-1.5">
            <span className="text-caption text-dim">Terminal</span>
            <CopyButton value={command} label="Copy the command" />
          </div>
          <pre className="mono overflow-x-auto px-3 py-2.5 text-ui leading-relaxed whitespace-pre-wrap text-fg">
            <span className="text-dim select-none">$ </span>
            {command}
          </pre>
        </div>
        <ol className="flex list-decimal flex-col gap-1.5 pl-5 text-ui leading-relaxed text-muted">
          <li>Run the command. It prints a code and a link.</li>
          <li>
            Open <span className="mono text-fg">{`${origin}${uiPath}/device`}</span>, sign in, and approve the
            code.
          </li>
          <li>The terminal keeps a token that expires on its own. Run antirion auth logout to revoke it.</li>
        </ol>
        {sso ? (
          <Note icon="lock">
            <p>
              Add <code className="mono text-fg">--sso</code> to open the approval page with single sign-on.
            </p>
          </Note>
        ) : null}
      </div>
      <MoreOptions>
        <TextButton icon="lock" className="self-start" onClick={() => go({ kind: 'start' })}>
          Back to sign in
        </TextButton>
      </MoreOptions>
    </>
  );
}
