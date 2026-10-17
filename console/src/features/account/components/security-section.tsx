'use client';

import { useEffect, useState, type ReactNode } from 'react';
import { z } from 'zod';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input } from '@/components/ui/form';
import { RevealOnceDialog } from '@/components/ui/reveal-once';
import { ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { CodeInput, ErrorStrip, SecretInput } from '@/features/auth/components/parts';
import { QrCode } from '@/features/auth/components/qr-code';
import { policyMessage } from '@/features/auth/store';
import { ApiError } from '@/lib/api/client';
import type { MfaFactor, Session } from '@/lib/api/types';
import { formatDate, formatDateTime, formatRelative } from '@/lib/time';
import { webauthnAvailable, webauthnCreate } from '@/lib/webauthn';
import { validate, type FieldErrors } from '@/lib/validation';
import { usePreferences } from '@/stores/preferences';
import { useSession } from '@/stores/session';
import { notify } from '@/stores/ui';

import { accountApi } from '../api';
import { describeAgent, describeLocation, hasAccount, methodLabel } from '../describe';
import { useAccount } from '../store';
import { Tooltip } from '@/components/ui/tooltip';

function Block({
  title,
  description,
  children,
}: {
  title: string;
  description?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section aria-label={title} className="flex flex-col gap-3">
      <div>
        <h3 className="text-body font-semibold text-fg">{title}</h3>
        {description ? <p className="mt-1 text-ui text-muted">{description}</p> : null}
      </div>
      {children}
    </section>
  );
}

/** Plain words for a failed account write; the gateway's message is kept for anything unexpected. */
function messageOf(e: unknown, unauthorized: string): string {
  if (e instanceof ApiError) {
    if (e.status === 0) return "Can't reach the gateway. Check your connection and try again.";
    if (e.status === 401) return unauthorized;
    if (e.status === 429) return 'Too many attempts. Wait a few minutes, then try again.';
    const policy = policyMessage(e);
    if (policy) return policy;
    return e.message;
  }
  return 'Something went wrong. Try again.';
}

const passwordSchema = z
  .object({
    current: z.string().min(1, 'Enter your current password'),
    next: z.string().min(12, 'Use at least 12 characters'),
    confirm: z.string(),
    code: z.string().trim().min(6, 'Enter the code from your authenticator app, or a recovery code'),
  })
  .refine((v) => v.next === v.confirm, { message: "Passwords don't match", path: ['confirm'] });

const EMPTY_PASSWORD = { current: '', next: '', confirm: '', code: '' };

function ChangePassword() {
  const change = useAccount((s) => s.changePassword);
  const [values, setValues] = useState(EMPTY_PASSWORD);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  function set(patch: Partial<typeof values>) {
    setValues((v) => ({ ...v, ...patch }));
    setErrors({});
  }

  async function submit() {
    const result = validate(passwordSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      await change(result.data.current, result.data.next, result.data.code);
      setValues(EMPTY_PASSWORD);
      notify('success', 'Password changed', 'Your other sessions were signed out.');
    } catch (e) {
      setValues((v) => ({ ...v, code: '' }));
      setError(messageOf(e, "Your current password or the code didn't match. Try again."));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      className="grid grid-cols-1 gap-4 sm:grid-cols-2"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {error ? (
        <div className="sm:col-span-2">
          <ErrorStrip message={error} />
        </div>
      ) : null}
      <Field label="Current password" error={errors.current}>
        <SecretInput
          autoComplete="current-password"
          compact
          value={values.current}
          onChange={(e) => set({ current: e.target.value })}
        />
      </Field>
      <Field
        label="Verification code"
        error={errors.code}
        hint="From your authenticator app, or a recovery code."
      >
        <CodeInput recovery compact value={values.code} onChange={(e) => set({ code: e.target.value })} />
      </Field>
      <Field label="New password" error={errors.next} hint="At least 12 characters.">
        <SecretInput
          autoComplete="new-password"
          compact
          value={values.next}
          onChange={(e) => set({ next: e.target.value })}
        />
      </Field>
      <Field label="Confirm password" error={errors.confirm}>
        <SecretInput
          autoComplete="new-password"
          compact
          value={values.confirm}
          onChange={(e) => set({ confirm: e.target.value })}
        />
      </Field>
      <div className="flex items-center gap-3 sm:col-span-2">
        <Button type="submit" busy={busy}>
          Change password
        </Button>
        <span className="text-caption text-muted">Your other sessions will be signed out.</span>
      </div>
    </form>
  );
}

const FACTOR_NAME: Record<MfaFactor['type'], string> = {
  totp: 'Authenticator app',
  webauthn: 'Security key',
};

interface Enrolment {
  factorId: string;
  secret: string | null;
  uri: string | null;
}

/** Adds an authenticator app to a signed-in account: scan, confirm one code, keep any new codes. */
function AddAuthenticator({ onDone, onCancel }: { onDone: (codes: string[]) => void; onCancel: () => void }) {
  const [enrolment, setEnrolment] = useState<Enrolment | null>(null);
  const [code, setCode] = useState('');
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    accountApi.enrollTotp().then(
      (e) => {
        if (live)
          setEnrolment({ factorId: e.factor_id, secret: e.secret ?? null, uri: e.otpauth_uri ?? null });
      },
      (e: unknown) => {
        if (live) setError(messageOf(e, 'Your session ended. Sign in again to add an authenticator.'));
      },
    );
    return () => {
      live = false;
    };
  }, []);

  async function confirm() {
    if (!enrolment) return;
    const digits = code.replace(/[\s-]/g, '');
    if (!/^\d{6}$/.test(digits)) {
      setError('Enter the 6-digit code from your app.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await accountApi.confirmTotp({
        factor_id: enrolment.factorId,
        totp: digits,
        ...(name.trim() ? { name: name.trim() } : {}),
      });
      onDone(result.recovery_codes);
    } catch (e) {
      setCode('');
      setError(messageOf(e, "That code didn't work. Enter the newest code from your app."));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-4 rounded-sm border border-line bg-sunken p-4">
      <ErrorStrip message={error} />
      {enrolment ? (
        <div className="flex flex-col gap-4 sm:flex-row sm:items-start">
          {enrolment.uri ? (
            <QrCode
              value={enrolment.uri}
              label="QR code to add Antirion to your authenticator app"
              size={148}
            />
          ) : null}
          <div className="flex min-w-0 flex-1 flex-col gap-3">
            <p className="text-ui text-fg">
              Scan the QR code with your authenticator app, then enter the 6-digit code it shows.
            </p>
            {enrolment.secret ? (
              <p className="text-caption text-muted">
                Can&apos;t scan? Enter this key:{' '}
                <span className="mono break-all text-fg select-all" data-secret>
                  {enrolment.secret}
                </span>
              </p>
            ) : null}
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label="Verification code">
                <CodeInput compact value={code} onChange={(e) => setCode(e.target.value)} autoFocus />
              </Field>
              <Field label="Name" optional>
                <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Work phone" />
              </Field>
            </div>
          </div>
        </div>
      ) : error ? null : (
        <LoadingRows rows={2} />
      )}
      <div className="flex gap-2">
        <Button variant="primary" busy={busy} disabled={!enrolment} onClick={() => void confirm()}>
          Add authenticator
        </Button>
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

/** Registers a security key or a passkey on a signed-in account; the browser runs the ceremony. */
function AddSecurityKey({ onDone, onCancel }: { onDone: (codes: string[]) => void; onCancel: () => void }) {
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function register() {
    setBusy(true);
    setError(null);
    try {
      const start = await accountApi.enrollWebauthn();
      if (!start.webauthn_options) throw new Error('no options');
      const credential = await webauthnCreate(start.webauthn_options);
      const result = await accountApi.confirmWebauthn({
        factor_id: start.factor_id,
        webauthn: credential,
        ...(name.trim() ? { name: name.trim() } : {}),
      });
      onDone(result.recovery_codes);
    } catch (e) {
      setError(
        e instanceof ApiError
          ? messageOf(e, 'Your session ended. Sign in again to add a security key.')
          : "Your security key wasn't registered. Try again, or use another key.",
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      className="flex flex-col gap-3 rounded-sm border border-line bg-sunken p-4"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        void register();
      }}
    >
      <ErrorStrip message={error} />
      <p className="text-ui text-fg">
        Connect or touch your security key when your browser asks. A passkey saved on this device works too.
      </p>
      <div className="max-w-[280px]">
        <Field label="Name" optional>
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Blue YubiKey" />
        </Field>
      </div>
      <div className="flex gap-2">
        <Button type="submit" variant="primary" icon="key" busy={busy}>
          Register security key
        </Button>
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </form>
  );
}

/** New recovery codes replace the old ones; the gateway asks for a current code first. */
function NewRecoveryCodes({
  factor,
  onDone,
  onCancel,
}: {
  factor: MfaFactor;
  onDone: (codes: string[]) => void;
  onCancel: () => void;
}) {
  const [code, setCode] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  async function submit() {
    const digits = code.replace(/[\s-]/g, '');
    if (!/^\d{6}$/.test(digits)) {
      setError('Enter the 6-digit code from your app.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await accountApi.regenerateRecoveryCodes({ factor_id: factor.id, totp: digits });
      onDone(result.recovery_codes);
    } catch (e) {
      setCode('');
      setError(messageOf(e, "That code didn't work. Enter the newest code from your app."));
    } finally {
      setBusy(false);
    }
  }
  return (
    <form
      className="flex flex-col gap-3 rounded-sm border border-line bg-sunken p-4"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <ErrorStrip message={error} />
      <p className="text-ui text-fg">
        Your current recovery codes will stop working. Enter a code from your authenticator app to continue.
      </p>
      <div className="max-w-[220px]">
        <Field label="Verification code">
          <CodeInput compact value={code} onChange={(e) => setCode(e.target.value)} autoFocus />
        </Field>
      </div>
      <div className="flex gap-2">
        <Button type="submit" variant="primary" busy={busy}>
          Get new codes
        </Button>
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </form>
  );
}

function TwoStep() {
  const factors = useAccount((s) => s.factors);
  const load = useAccount((s) => s.loadFactors);
  const remove = useAccount((s) => s.deleteFactor);
  const timeZone = usePreferences((s) => s.timeZone);
  const [panel, setPanel] = useState<'add' | 'key' | 'codes' | null>(null);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [removing, setRemoving] = useState<MfaFactor | null>(null);
  const items = factors.data?.items ?? [];
  const totp = items.find((f) => f.type === 'totp');
  const remaining = factors.data?.recoveryCodesRemaining ?? 0;

  function done(next: string[]) {
    setPanel(null);
    void load();
    if (next.length > 0) setCodes(next);
    else notify('success', panel === 'key' ? 'Security key added' : 'Authenticator added');
  }

  return (
    <Block
      title="Two-step verification"
      description="Password sign-in always asks for a second step. Keep at least one way to verify."
    >
      {factors.status === 'loading' && !factors.data ? <LoadingRows rows={2} /> : null}
      {factors.error ? (
        <ErrorState error={factors.error} subject="two-step verification" onRetry={() => void load()} />
      ) : null}
      {factors.data ? (
        <ul className="flex flex-col divide-y divide-line rounded-sm border border-line">
          {items.length === 0 ? (
            <li className="px-3 py-2.5 text-ui text-muted">
              Nothing set up yet. You&apos;ll be asked to add an authenticator app at your next sign-in.
            </li>
          ) : null}
          {items.map((f) => (
            <li key={f.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2.5">
              <div className="min-w-0 flex-1">
                <p className="text-ui font-medium text-fg">
                  {FACTOR_NAME[f.type]}
                  {f.name ? <span className="font-normal text-muted"> · {f.name}</span> : null}
                </p>
                <p className="text-caption text-muted">
                  Added {formatDate(f.created_at, timeZone)} · last used{' '}
                  {formatRelative(f.last_used_at ?? null)}
                </p>
              </div>
              <Button
                size="sm"
                variant="ghost"
                disabled={items.length < 2}
                disabledReason="Add another way to verify before removing this one."
                onClick={() => setRemoving(f)}
              >
                Remove
              </Button>
            </li>
          ))}
          <li className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2.5">
            <div className="min-w-0 flex-1">
              <p className="text-ui font-medium text-fg">Recovery codes</p>
              <p className="text-caption text-muted">
                {remaining === 1 ? '1 code left' : `${remaining} codes left`}. Each signs you in once if you
                lose your phone.
              </p>
            </div>
            {totp ? (
              <Button size="sm" variant="ghost" onClick={() => setPanel(panel === 'codes' ? null : 'codes')}>
                Get new codes
              </Button>
            ) : null}
          </li>
        </ul>
      ) : null}
      {panel === 'codes' && totp ? (
        <NewRecoveryCodes factor={totp} onDone={done} onCancel={() => setPanel(null)} />
      ) : null}
      {panel === 'add' ? <AddAuthenticator onDone={done} onCancel={() => setPanel(null)} /> : null}
      {panel === 'key' ? <AddSecurityKey onDone={done} onCancel={() => setPanel(null)} /> : null}
      {panel === null && factors.data ? (
        <div className="flex flex-wrap gap-2">
          <Button size="sm" icon="plus" onClick={() => setPanel('add')}>
            Add authenticator app
          </Button>
          {webauthnAvailable() ? (
            <Button size="sm" icon="key" onClick={() => setPanel('key')}>
              Add security key
            </Button>
          ) : null}
        </div>
      ) : null}
      <RevealOnceDialog
        secret={codes ? codes.join('\n') : null}
        onClose={() => setCodes(null)}
        title="Your new recovery codes"
        label="Recovery codes"
        description="Your old codes no longer work. Each of these signs you in once if you lose your phone."
      />
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title={`Remove this ${removing ? FACTOR_NAME[removing.type].toLowerCase() : 'way to verify'}?`}
        objectName={removing?.name ?? (removing ? FACTOR_NAME[removing.type] : undefined)}
        effect="You won't be able to use it to sign in. You can add it again later."
        confirmLabel={`Remove ${removing ? FACTOR_NAME[removing.type].toLowerCase() : ''}`.trim()}
        withReason={false}
        onConfirm={async () => {
          if (removing) await remove(removing.id);
        }}
      />
    </Block>
  );
}

function Sessions() {
  const sessions = useAccount((s) => s.sessions);
  const load = useAccount((s) => s.loadSessions);
  const revoke = useAccount((s) => s.revokeSession);
  const revokeOthers = useAccount((s) => s.revokeOthers);
  const others = (sessions.data ?? []).filter((s) => !s.current).length;
  // This browser first, then the most recently active.
  const ordered = [...(sessions.data ?? [])].sort(
    (a, b) =>
      Number(b.current) - Number(a.current) ||
      new Date(b.last_seen_at).getTime() - new Date(a.last_seen_at).getTime(),
  );
  const located = (sessions.data ?? []).some((s) => describeLocation(s.location) !== null);

  return (
    <Block title="Where you're signed in">
      {sessions.status === 'loading' && !sessions.data ? <LoadingRows rows={2} /> : null}
      {sessions.error ? (
        <ErrorState error={sessions.error} subject="sessions" onRetry={() => void load()} />
      ) : null}
      {sessions.data ? (
        <Table label="Your sessions">
          <thead>
            <tr>
              <Th>Device</Th>
              <Th>Signed in with</Th>
              <Th>{located ? 'Location' : 'IP address'}</Th>
              <Th>Last active</Th>
              <Th>
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {ordered.map((s) => (
              <Tr key={s.id}>
                <Td>
                  <span className="flex items-center gap-2">
                    <Tooltip content={s.user_agent}>
                      <span className="max-w-[220px] truncate">{describeAgent(s.user_agent)}</span>
                    </Tooltip>
                    {s.current ? <Badge tone="accent">This browser</Badge> : null}
                  </span>
                </Td>
                <Td>
                  {methodLabel(s.method)}
                  {s.mfa || s.trusted ? (
                    <span className="block text-caption text-muted">
                      {[s.mfa ? 'Two-step verified' : null, s.trusted ? 'Trusted browser' : null]
                        .filter(Boolean)
                        .join(' · ')}
                    </span>
                  ) : null}
                </Td>
                {located ? (
                  <Td>
                    {describeLocation(s.location) ?? 'Unknown'}
                    <span className="mono block text-caption text-muted">{s.client_ip ?? '—'}</span>
                  </Td>
                ) : (
                  <Td mono>{s.client_ip ?? '—'}</Td>
                )}
                <Td muted>{s.current ? 'Now' : formatRelative(s.last_seen_at)}</Td>
                <Td className="text-right">
                  {s.current ? null : (
                    <Button size="sm" variant="ghost" onClick={() => void revoke(s.id)}>
                      Sign out
                    </Button>
                  )}
                </Td>
              </Tr>
            ))}
          </tbody>
        </Table>
      ) : null}
      {sessions.data ? (
        <div className="flex items-center gap-3">
          <Button
            size="sm"
            onClick={() => void revokeOthers().then(() => notify('success', 'Other sessions signed out'))}
            disabled={others === 0}
            disabledReason="You're only signed in here."
          >
            Sign out other sessions
          </Button>
          {others > 0 ? (
            <span className="text-caption text-muted">
              {others === 1 ? '1 other session' : `${others} other sessions`}
            </span>
          ) : null}
        </div>
      ) : null}
    </Block>
  );
}

/**
 * Browsers that skip the second step: the sessions started with "Trust this browser". Forgetting one
 * ends that session, so its next sign-in asks for the second step again.
 */
function TrustedBrowsers() {
  const sessions = useAccount((s) => s.sessions);
  const revoke = useAccount((s) => s.revokeSession);
  const timeZone = usePreferences((s) => s.timeZone);
  const [forgetting, setForgetting] = useState<Session | null>(null);
  const trusted = (sessions.data ?? []).filter((s) => s.trusted);
  if (!sessions.data) return null;
  return (
    <Block
      title="Trusted browsers"
      description="These browsers skip the second step until their trust ends. Forget one you no longer use, or any you don't recognise."
    >
      {trusted.length === 0 ? (
        <p className="rounded-sm border border-line px-3 py-2.5 text-ui text-muted">
          No trusted browsers. Tick &ldquo;Trust this browser&rdquo; when you sign in to add this one.
        </p>
      ) : (
        <ul className="flex flex-col divide-y divide-line rounded-sm border border-line">
          {trusted.map((s) => {
            const where = describeLocation(s.location);
            return (
              <li key={s.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <p className="flex items-center gap-2 text-ui font-medium text-fg">
                    <Tooltip content={s.user_agent}>
                      <span className="truncate">{describeAgent(s.user_agent)}</span>
                    </Tooltip>
                    {s.current ? <Badge tone="accent">This browser</Badge> : null}
                  </p>
                  <p className="text-caption text-muted">
                    {[where, s.client_ip ?? null].filter(Boolean).join(' · ')}
                    {where || s.client_ip ? ' · ' : ''}
                    Trusted since {formatDate(s.created_at, timeZone)} · until{' '}
                    {formatDateTime(s.expires_at, timeZone)}
                  </p>
                </div>
                <Button size="sm" variant="ghost" onClick={() => setForgetting(s)}>
                  Forget
                </Button>
              </li>
            );
          })}
        </ul>
      )}
      <ConfirmDialog
        open={forgetting !== null}
        onClose={() => setForgetting(null)}
        title={forgetting?.current ? 'Forget this browser?' : 'Forget this trusted browser?'}
        objectName={forgetting ? describeAgent(forgetting.user_agent) : undefined}
        effect={
          forgetting?.current
            ? "You'll be signed out here, and your next sign-in on this browser will ask for the second step."
            : 'That browser is signed out, and its next sign-in will ask for the second step.'
        }
        confirmLabel="Forget browser"
        withReason={false}
        onConfirm={async () => {
          if (!forgetting) return;
          await revoke(forgetting.id);
          // The session just ended: reading it again sends the console to the sign-in page.
          if (forgetting.current) await useSession.getState().load();
          else notify('success', 'Browser forgotten');
        }}
      />
    </Block>
  );
}

/** Password, second factors and sessions for a person; only the sessions for an admin token. */
export function SecuritySection() {
  const me = useSession((s) => s.me);
  const loadSessions = useAccount((s) => s.loadSessions);
  const loadFactors = useAccount((s) => s.loadFactors);
  const account = hasAccount(me);
  const password = account && (me?.user?.mfa_methods?.length ?? 0) > 0;

  useEffect(() => {
    void loadSessions();
    if (password) void loadFactors();
  }, [password, loadSessions, loadFactors]);

  return (
    <div className="flex flex-col gap-8">
      {account ? null : (
        <p className="text-ui text-muted">
          Admin tokens have no password or two-step verification. These are the sessions this token started.
        </p>
      )}
      {password ? (
        <Block title="Password">
          <ChangePassword />
        </Block>
      ) : null}
      {password ? <TwoStep /> : null}
      {account ? <TrustedBrowsers /> : null}
      <Sessions />
    </div>
  );
}
