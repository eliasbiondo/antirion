'use client';

import { useRouter, useSearchParams } from 'next/navigation';
import { useEffect, useState, useSyncExternalStore, type ReactNode } from 'react';

import { PreferencesApplier } from '@/components/layout/preferences-applier';
import { Spinner } from '@/components/ui/button';
import { BrandMark } from '@/components/ui/icon';
import { formatCount, formatLatency, formatPercent } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { useSession } from '@/stores/session';
import { LAST_ORGANIZATION_KEY } from '@/stores/scope';

import { safeNext } from '../next';
import { useLogin } from '../store';
import { EnrollStep, ForgotStep, MfaStep, RecoveryCodesStep, ResetStep } from './login-steps';
import { CliStep } from './cli-step';
import { MethodsStep, OrganizationStep, TokenStep } from './login-methods';

/**
 * The gateway's coarse traffic figures, only when the operator turned the sign-in ticker on
 * (`login_options.ticker` is null otherwise). One quiet line under the card, never a dashboard.
 */
function Ticker() {
  const ticker = useLogin((s) => s.options?.ticker);
  if (!ticker) return null;
  const parts = [
    ticker.requests_24h !== undefined
      ? `${formatCount(ticker.requests_24h, { compact: true })} requests`
      : null,
    ticker.error_rate !== undefined ? `${formatPercent(ticker.error_rate, 2)} errors` : null,
    ticker.p95_ms !== undefined ? `p95 ${formatLatency(ticker.p95_ms)}` : null,
    ticker.active_keys !== undefined ? `${formatCount(ticker.active_keys)} active keys` : null,
  ].filter(Boolean);
  if (parts.length === 0) return null;
  return (
    <p className="text-center text-caption leading-relaxed text-dim" aria-label="Gateway traffic">
      Last 24 h · {parts.join(' · ')}
      {ticker.as_of ? ` · updated ${formatRelative(ticker.as_of)}` : null}
    </p>
  );
}

/** The host the browser is talking to, so people with several gateways know which one this is. */
function useHost(): string {
  return useSyncExternalStore(
    noSubscription,
    () => window.location.host,
    () => '',
  );
}

function noSubscription(): () => void {
  return () => undefined;
}

/** The sign-in pages' frame: the brand, the host, one card and a footer. */
export function Frame({ children, footer }: { children: ReactNode; footer: ReactNode }) {
  const host = useHost();
  return (
    <div className="relative isolate flex min-h-screen flex-col items-center overflow-x-hidden bg-canvas px-4 pt-10 pb-8 sm:pt-[12vh]">
      <PreferencesApplier />
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 -z-10 bg-[linear-gradient(var(--ant-grid-line)_1px,transparent_1px),linear-gradient(90deg,var(--ant-grid-line)_1px,transparent_1px)] [mask-image:radial-gradient(ellipse_70%_60%_at_50%_40%,transparent_15%,black_75%)] bg-[size:40px_40px] print:hidden"
      />
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 -z-10 bg-[radial-gradient(circle_at_20%_15%,color-mix(in_oklab,var(--ant-accent)_10%,transparent),transparent_40%),radial-gradient(circle_at_80%_85%,color-mix(in_oklab,var(--ant-accent)_6%,transparent),transparent_45%)] print:hidden"
      />
      <header className="mb-6 flex flex-col items-center gap-1 text-center">
        <div className="flex items-center gap-2 text-fg">
          <span className="text-accent">
            <BrandMark size={20} />
          </span>
          <span className="text-base font-semibold tracking-tight">Antirion</span>
        </div>
        {host ? <p className="mono text-caption text-muted">{host}</p> : null}
      </header>
      <main className="w-full max-w-[400px] rounded-lg border border-line bg-raised p-6 shadow-[0_20px_60px_-20px_rgb(0_0_0/0.18)] sm:p-8 dark:shadow-[0_20px_60px_-20px_rgb(0_0_0/0.6),0_0_40px_-12px_color-mix(in_oklab,var(--ant-accent)_18%,transparent)] print:border-0 print:shadow-none">
        {children}
      </main>
      <footer className="mt-6 flex w-full max-w-[400px] flex-col items-center gap-1.5 print:hidden">
        {footer}
      </footer>
    </div>
  );
}

/** The sign-in page: one card, whose title and content follow the step of the flow. */
export function LoginScreen({ entry }: { entry?: 'reset' | 'invitation' } = {}) {
  const router = useRouter();
  const params = useSearchParams();
  const next = safeNext(params.get('next'));
  const status = useSession((s) => s.status);
  const loadSession = useSession((s) => s.load);
  const expired = useSession((s) => s.expiredAt);
  const step = useLogin((s) => s.step);
  const options = useLogin((s) => s.options);
  const loadOptions = useLogin((s) => s.loadOptions);
  const setOrganization = useLogin((s) => s.setOrganization);
  const go = useLogin((s) => s.go);
  const [checked, setChecked] = useState(false);

  useEffect(() => {
    let org = params.get('organization') ?? '';
    if (!org) {
      try {
        org = localStorage.getItem(LAST_ORGANIZATION_KEY) ?? '';
      } catch {
        org = '';
      }
    }
    setOrganization(org);
    void loadOptions(org || undefined);
    const token = entry ? params.get('token') : params.get('reset_token');
    // The flow starts over on every visit: a finished sign-in left before a sign-out must not redirect.
    if (entry || token) {
      const purpose = entry === 'invitation' ? 'invitation' : 'reset';
      go({ kind: 'reset', token: token ?? '', purpose, link: token ? 'checking' : 'missing' });
    } else go({ kind: 'start' });
    void loadSession().finally(() => setChecked(true));
  }, [params, setOrganization, loadOptions, loadSession, go, entry]);

  useEffect(() => {
    if (step.kind === 'done' || (checked && status === 'authenticated' && step.kind === 'start'))
      router.replace(next);
  }, [step, status, checked, router, next]);

  const onlyToken = options?.methods.every((m) => m.type === 'token') ?? false;
  // No account exists yet: only the bootstrap token can sign in, so the page opens on it.
  const setup = options?.setup_required ?? false;
  const showExpired =
    expired !== null && (step.kind === 'start' || step.kind === 'methods' || step.kind === 'token');

  return (
    <Frame
      footer={
        <>
          {options?.version ? <p className="text-caption text-dim">Antirion v{options.version}</p> : null}
          <Ticker />
        </>
      }
    >
      {showExpired ? (
        <p
          role="status"
          className="mb-5 rounded-sm border border-line bg-sunken px-3 py-2 text-ui leading-snug text-muted"
        >
          Your session expired. Sign in to continue.
        </p>
      ) : null}
      {options === null ? (
        <p className="flex min-h-40 items-center justify-center gap-2 text-ui text-muted" role="status">
          <Spinner /> Loading…
        </p>
      ) : step.kind === 'start' && (onlyToken || setup) ? (
        <TokenStep />
      ) : step.kind === 'start' || step.kind === 'methods' ? (
        <MethodsStep next={next} />
      ) : step.kind === 'token' ? (
        <TokenStep />
      ) : step.kind === 'cli' ? (
        <CliStep />
      ) : step.kind === 'organization' ? (
        <OrganizationStep />
      ) : step.kind === 'mfa' ? (
        <MfaStep />
      ) : step.kind === 'enroll' ? (
        <EnrollStep />
      ) : step.kind === 'recovery-codes' ? (
        <RecoveryCodesStep />
      ) : step.kind === 'forgot' || step.kind === 'forgot-sent' ? (
        <ForgotStep />
      ) : step.kind === 'reset' ? (
        <ResetStep />
      ) : (
        <p className="flex min-h-40 items-center justify-center gap-2 text-ui text-muted" role="status">
          <Spinner /> Signing in…
        </p>
      )}
    </Frame>
  );
}
