'use client';

import { useEffect, useRef, useState } from 'react';
import { z } from 'zod';

import { Button, Spinner } from '@/components/ui/button';
import { Checkbox, Field, Input } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { cn } from '@/lib/cn';
import { plural } from '@/lib/format';
import { validate } from '@/lib/validation';
import { webauthnAvailable } from '@/lib/webauthn';

import { useLogin, useTrustDays, type Flow, type LinkStatus } from '../store';
import { spanInWords, timeLeft } from '../words';
import { OrganizationLine } from './login-methods';
import {
  CodeInput,
  digitsOf,
  ErrorStrip,
  MoreOptions,
  Note,
  SecretInput,
  StepHeader,
  TextButton,
} from './parts';
import { QrCode } from './qr-code';

/** Where the second-factor setup and the recovery codes sit in the flow that led to them. */
function stepOf(flow: Flow, kind: 'enroll' | 'codes'): { current: number; total: number } {
  if (flow === 'invitation') return { current: kind === 'enroll' ? 2 : 3, total: 3 };
  return { current: kind === 'enroll' ? 1 : 2, total: 2 };
}

/** Submits the form as soon as a full 6-digit code is typed or pasted. */
function useAutoSubmit(code: string, enabled: boolean) {
  const formRef = useRef<HTMLFormElement>(null);
  const last = useRef('');
  useEffect(() => {
    const digits = digitsOf(code);
    if (!enabled || !/^\d{6}$/.test(digits) || digits === last.current) return;
    last.current = digits;
    formRef.current?.requestSubmit();
  }, [code, enabled]);
  return formRef;
}

/** The account the second factor is for, with a way back if it is the wrong one. */
function AccountLine() {
  const email = useLogin((s) => s.email);
  const go = useLogin((s) => s.go);
  if (!email) return null;
  return (
    <span className="inline-flex flex-wrap items-baseline gap-x-1.5">
      <span className="font-medium text-fg">{email}</span>
      <span aria-hidden="true">·</span>
      <TextButton tone="accent" onClick={() => go({ kind: 'start' })}>
        Not you?
      </TextButton>
    </span>
  );
}

type MfaMode = 'key' | 'code' | 'recovery';

/** Second factor: the security key when the account has one, an authenticator code, or a recovery code. */
export function MfaStep() {
  const verify = useLogin((s) => s.verifyMfa);
  const busy = useLogin((s) => s.busy);
  const error = useLogin((s) => s.error);
  const trust = useLogin((s) => s.trustDevice);
  const setTrust = useLogin((s) => s.setTrustDevice);
  const go = useLogin((s) => s.go);
  const trustDays = useTrustDays();
  const securityKey = useLogin(
    (s) => s.step.kind === 'mfa' && s.step.webauthn !== null && webauthnAvailable(),
  );
  const [mode, setMode] = useState<MfaMode>(securityKey ? 'key' : 'code');
  const [code, setCode] = useState('');
  const [fieldError, setFieldError] = useState<string | undefined>();
  const inputRef = useRef<HTMLInputElement>(null);
  const formRef = useAutoSubmit(code, mode === 'code' && !busy);

  function switchTo(next: MfaMode) {
    setMode(next);
    setCode('');
    setFieldError(undefined);
  }

  async function submit(e: { preventDefault: () => void }) {
    e.preventDefault();
    if (mode === 'key') {
      await verify({ securityKey: true });
      return;
    }
    let ok: boolean;
    if (mode === 'recovery') {
      const value = code.trim();
      if (value.length < 6) {
        setFieldError('Enter one of your recovery codes');
        return;
      }
      setFieldError(undefined);
      ok = await verify({ recoveryCode: value });
    } else {
      const digits = digitsOf(code);
      if (!/^\d{6}$/.test(digits)) {
        // Right after a refused code the field is empty again: the banner already says what to do.
        if (!(digits === '' && error)) setFieldError('Enter the 6-digit code from your app');
        return;
      }
      setFieldError(undefined);
      ok = await verify({ totp: digits });
    }
    if (!ok) {
      setCode('');
      inputRef.current?.focus();
    }
  }

  const title =
    mode === 'key'
      ? 'Use your security key'
      : mode === 'recovery'
        ? 'Enter a recovery code'
        : 'Enter your verification code';

  return (
    <>
      <StepHeader title={title} subtitle={<AccountLine />} />
      <form ref={formRef} className="flex flex-col gap-4" noValidate onSubmit={(e) => void submit(e)}>
        <ErrorStrip message={error} />
        {mode === 'key' ? (
          <p className="text-body leading-relaxed text-fg">
            Connect or touch your security key when your browser asks. You can also use a passkey saved on
            this device.
          </p>
        ) : (
          <>
            <p className="text-body leading-relaxed text-fg">
              {mode === 'recovery'
                ? 'Enter one of the recovery codes you saved when you set up two-step verification.'
                : 'Open your authenticator app and enter the 6-digit code for Antirion.'}
            </p>
            <Field label={mode === 'recovery' ? 'Recovery code' : 'Verification code'} error={fieldError}>
              <CodeInput
                ref={inputRef}
                recovery={mode === 'recovery'}
                value={code}
                onChange={(e) => {
                  setCode(e.target.value);
                  if (fieldError) setFieldError(undefined);
                }}
                autoFocus
              />
            </Field>
          </>
        )}
        {trustDays ? (
          <Checkbox
            label={`Trust this browser for ${plural(trustDays, 'day')}`}
            description="You won't be asked for a second step on this browser until then."
            checked={trust}
            onChange={(v) => setTrust(v)}
          />
        ) : null}
        <Button
          type="submit"
          variant="primary"
          icon={mode === 'key' ? 'key' : undefined}
          busy={busy}
          className="h-10 w-full text-base"
        >
          {mode === 'key' ? 'Use security key' : 'Verify'}
        </Button>
      </form>
      <MoreOptions>
        <div className="flex flex-wrap gap-x-5 gap-y-2">
          {mode !== 'code' ? (
            <TextButton onClick={() => switchTo('code')}>Use your authenticator app</TextButton>
          ) : null}
          {mode !== 'key' && securityKey ? (
            <TextButton onClick={() => switchTo('key')}>Use your security key</TextButton>
          ) : null}
          {mode !== 'recovery' ? (
            <TextButton onClick={() => switchTo('recovery')}>Use a recovery code</TextButton>
          ) : null}
        </div>
        <TextButton className="self-start" onClick={() => go({ kind: 'start' })}>
          Back to sign in
        </TextButton>
      </MoreOptions>
    </>
  );
}

/** "JBSWY3DP EHPK3PXP": easier to type, and the text still copies without spaces. */
function GroupedSecret({ value }: { value: string }) {
  const groups = value.match(/.{1,4}/g) ?? [];
  return (
    <output
      aria-label="Setup key"
      data-secret
      className="mono block text-body leading-relaxed break-all text-fg select-all"
    >
      {groups.map((g, i) => (
        <span key={i} className="mr-[0.6ch] inline-block last:mr-0">
          {g}
        </span>
      ))}
    </output>
  );
}

function CopySecret({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Button
      size="sm"
      icon={copied ? 'check' : 'copy'}
      onClick={() => {
        navigator.clipboard.writeText(value).then(
          () => setCopied(true),
          () => setCopied(false),
        );
      }}
    >
      {copied ? 'Copied' : 'Copy key'}
    </Button>
  );
}

/** First sign-in of a password account: scan the QR code, confirm one code; a security key instead. */
export function EnrollStep() {
  const step = useLogin((s) => s.step);
  const flow = useLogin((s) => s.flow);
  const start = useLogin((s) => s.startEnrollment);
  const confirm = useLogin((s) => s.confirmEnrollment);
  const enrollKey = useLogin((s) => s.enrollSecurityKey);
  const busy = useLogin((s) => s.busy);
  const error = useLogin((s) => s.error);
  const go = useLogin((s) => s.go);
  const [code, setCode] = useState('');
  const [fieldError, setFieldError] = useState<string | undefined>();
  const inputRef = useRef<HTMLInputElement>(null);
  const enrolment = step.kind === 'enroll' ? step : null;
  const formRef = useAutoSubmit(code, Boolean(enrolment?.factorId) && !busy);

  useEffect(() => {
    if (enrolment && !enrolment.factorId && !enrolment.secret) void start();
    // Only once per enrolment: a failed start shows its error instead of retrying in a loop.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enrolment?.challenge, start]);

  if (!enrolment) return null;
  return (
    <>
      <StepHeader
        title="Set up two-step verification"
        step={stepOf(flow, 'enroll')}
        subtitle="Password sign-in needs a second step: a code from an authenticator app on your phone."
      />
      <form
        ref={formRef}
        className="flex flex-col gap-5"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          const digits = digitsOf(code);
          if (!/^\d{6}$/.test(digits)) {
            setFieldError('Enter the 6-digit code from your app');
            return;
          }
          setFieldError(undefined);
          void confirm(digits).then((ok) => {
            if (ok) return;
            setCode('');
            inputRef.current?.focus();
          });
        }}
      >
        <ErrorStrip message={error} />
        {enrolment.secret ? (
          <div className="flex flex-col gap-4">
            <p className="text-body leading-relaxed text-fg">
              <span className="font-medium">1.</span> Scan this QR code with your authenticator app.
            </p>
            <div className="flex justify-center">
              {enrolment.uri ? (
                <QrCode value={enrolment.uri} label="QR code to add Antirion to your authenticator app" />
              ) : null}
            </div>
            <div className="rounded-sm border border-line bg-sunken p-3">
              <div className="mb-1.5 flex items-center justify-between gap-2">
                <span className="text-caption text-muted">Can&apos;t scan? Enter this key instead:</span>
                <CopySecret value={enrolment.secret} />
              </div>
              <GroupedSecret value={enrolment.secret} />
              {enrolment.uri ? (
                <a
                  href={enrolment.uri}
                  className="mt-2 inline-block text-caption text-accent underline-offset-4 hover:underline"
                >
                  Open in an authenticator app on this device
                </a>
              ) : null}
            </div>
            <p className="text-body leading-relaxed text-fg">
              <span className="font-medium">2.</span> Enter the 6-digit code the app shows.
            </p>
          </div>
        ) : error ? null : (
          <p className="flex items-center gap-2 text-ui text-muted" role="status">
            <Spinner /> Setting up…
          </p>
        )}
        <Field label="Verification code" error={fieldError}>
          <CodeInput
            ref={inputRef}
            value={code}
            disabled={!enrolment.factorId}
            onChange={(e) => {
              setCode(e.target.value);
              if (fieldError) setFieldError(undefined);
            }}
          />
        </Field>
        <Button
          type="submit"
          variant="primary"
          busy={busy}
          disabled={!enrolment.factorId}
          className="h-10 w-full text-base"
        >
          Verify and continue
        </Button>
      </form>
      <MoreOptions>
        {webauthnAvailable() ? (
          <TextButton icon="key" className="self-start" disabled={busy} onClick={() => void enrollKey()}>
            Use a security key instead
          </TextButton>
        ) : null}
        <TextButton className="self-start" onClick={() => go({ kind: 'start' })}>
          Back to sign in
        </TextButton>
      </MoreOptions>
    </>
  );
}

function downloadText(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: 'text/plain' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** After the first factor: the recovery codes, shown once, with ways to keep them. */
export function RecoveryCodesStep() {
  const step = useLogin((s) => s.step);
  const flow = useLogin((s) => s.flow);
  const email = useLogin((s) => s.email);
  const finish = useLogin((s) => s.finishEnrollment);
  const [saved, setSaved] = useState<'copied' | 'downloaded' | 'printed' | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const codes = step.kind === 'recovery-codes' ? step.codes : [];
  const text = [
    'Antirion recovery codes',
    ...(email ? [`Account: ${email}`] : []),
    `Gateway: ${typeof window === 'undefined' ? '' : window.location.host}`,
    '',
    ...codes,
    '',
    'Each code signs you in once if you lose your authenticator.',
    '',
  ].join('\n');

  return (
    <>
      <StepHeader
        title="Save your recovery codes"
        step={stepOf(flow, 'codes')}
        subtitle="If you lose your phone, each code signs you in once. Keep them somewhere safe, like your password manager. You won't see them again."
      />
      <div className="flex flex-col gap-4">
        <ol
          aria-label="Recovery codes"
          data-secret
          className="mono grid grid-cols-2 gap-x-6 gap-y-1.5 rounded-sm border border-line bg-sunken px-4 py-3 text-body text-fg select-all"
        >
          {codes.map((c) => (
            <li key={c}>{c}</li>
          ))}
        </ol>
        <div className="flex flex-wrap gap-2 print:hidden">
          <Button
            icon={saved === 'downloaded' ? 'check' : 'download'}
            onClick={() => {
              downloadText('antirion-recovery-codes.txt', text);
              setSaved('downloaded');
            }}
          >
            Download
          </Button>
          <Button
            icon={saved === 'copied' ? 'check' : 'copy'}
            onClick={() => {
              navigator.clipboard.writeText(codes.join('\n')).then(
                () => setSaved('copied'),
                () => undefined,
              );
            }}
          >
            {saved === 'copied' ? 'Copied' : 'Copy'}
          </Button>
          <Button
            icon={saved === 'printed' ? 'check' : 'printer'}
            onClick={() => {
              setSaved('printed');
              window.print();
            }}
          >
            Print
          </Button>
        </div>
        <div className="print:hidden">
          <Checkbox
            label="I've saved my recovery codes"
            checked={acknowledged}
            onChange={(v) => setAcknowledged(v)}
          />
        </div>
        <Button
          variant="primary"
          className="h-10 w-full text-base print:hidden"
          disabled={!acknowledged}
          disabledReason="Tick “I've saved my recovery codes” to continue"
          onClick={finish}
        >
          Continue to Antirion
        </Button>
      </div>
    </>
  );
}

const emailSchema = z
  .string()
  .trim()
  .min(1, 'Enter your email')
  .regex(/^[^@\s]+@[^@\s]+$/, 'Enter an email like name@example.com');

/** "Forgot password?": always answers the same way, whether or not the address has an account. */
export function ForgotStep() {
  const step = useLogin((s) => s.step);
  const request = useLogin((s) => s.requestReset);
  const busy = useLogin((s) => s.busy);
  const stored = useLogin((s) => s.email);
  const resetTtl = useLogin((s) => s.options?.password?.reset_ttl_seconds ?? null);
  const go = useLogin((s) => s.go);
  const [email, setEmail] = useState(stored);
  const [fieldError, setFieldError] = useState<string | undefined>();

  if (step.kind === 'forgot-sent') {
    return (
      <>
        <StepHeader title="Check your email" />
        <div className="flex flex-col gap-4">
          <p role="status" className="text-body leading-relaxed text-fg">
            If <span className="font-medium">{stored}</span> has an account, we&apos;ve sent it a link to
            reset the password.{' '}
            {resetTtl
              ? `The link works once, for ${spanInWords(resetTtl)}.`
              : 'The link works once and expires soon.'}
          </p>
          <Note icon="mail">
            Nothing after a few minutes? Check your spam folder, or ask your gateway admin to confirm the
            address on your account.
          </Note>
          <Button variant="primary" className="h-10 w-full text-base" onClick={() => go({ kind: 'start' })}>
            Back to sign in
          </Button>
        </div>
        <MoreOptions>
          <TextButton className="self-start" onClick={() => go({ kind: 'forgot' })}>
            Use a different email
          </TextButton>
        </MoreOptions>
      </>
    );
  }
  return (
    <>
      <StepHeader
        title="Reset your password"
        subtitle="Enter the email you sign in with and we'll send you a link to choose a new password."
      />
      <form
        className="flex flex-col gap-4"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          const result = validate(emailSchema, email);
          if (!result.ok) {
            setFieldError(result.errors[''] ?? 'Enter an email like name@example.com');
            return;
          }
          setFieldError(undefined);
          void request(result.data);
        }}
      >
        <Field label="Email" error={fieldError}>
          <Input
            type="email"
            autoComplete="username"
            inputMode="email"
            spellCheck={false}
            autoCapitalize="off"
            placeholder="name@example.com"
            value={email}
            onChange={(e) => {
              setEmail(e.target.value);
              if (fieldError) setFieldError(undefined);
            }}
            autoFocus
            className="h-10 text-base"
          />
        </Field>
        <Button type="submit" variant="primary" busy={busy} className="mt-1 h-10 w-full text-base">
          Send reset link
        </Button>
      </form>
      <MoreOptions>
        <TextButton className="self-start" onClick={() => go({ kind: 'start' })}>
          Back to sign in
        </TextButton>
      </MoreOptions>
    </>
  );
}

/** The password lengths the gateway asks for (`login_options.password`), else its defaults. */
function usePasswordPolicy(): { min: number; max: number } {
  const min = useLogin((s) => s.options?.password?.min_length ?? 12);
  const max = useLogin((s) => s.options?.password?.max_length ?? 128);
  return { min, max };
}

function resetSchema({ min, max }: { min: number; max: number }) {
  return z
    .object({
      password: z
        .string()
        .min(min, `Use at least ${min} characters`)
        .max(max, `Use ${max} characters or fewer`),
      confirm: z.string(),
    })
    .refine((v) => v.password === v.confirm, { message: "Passwords don't match", path: ['confirm'] });
}

/** The rule a browser can check as you type; the gateway checks the rest when you submit. */
function PasswordRules({ password, min }: { password: string; min: number }) {
  const long = password.length >= min;
  return (
    <ul className="flex flex-col gap-1 text-caption" aria-label="Password rules">
      <li className={cn('flex items-center gap-1.5', long ? 'text-success' : 'text-muted')}>
        <span aria-hidden="true" className="flex h-3.5 w-3.5 items-center justify-center">
          {long ? <Icon name="check" size={12} /> : <span className="h-1 w-1 rounded-full bg-current" />}
        </span>
        {min} or more characters
        <span className="sr-only">{long ? '(done)' : '(not yet)'}</span>
      </li>
      <li className="flex items-center gap-1.5 text-muted">
        <span aria-hidden="true" className="flex h-3.5 w-3.5 items-center justify-center">
          <span className="h-1 w-1 rounded-full bg-current" />
        </span>
        Not a common password, your email, or one you&apos;ve used recently
      </li>
    </ul>
  );
}

type DeadState = Exclude<LinkStatus, 'checking' | 'valid' | 'unknown'>;

const DEAD: Record<'reset' | 'invitation', Record<DeadState, { title: string; body: string }>> = {
  invitation: {
    missing: {
      title: 'This invitation link is incomplete',
      body: 'Part of the link is missing. Open it again from the email, or copy the whole address into your browser.',
    },
    expired: {
      title: 'This invitation has expired',
      body: 'Invitation links work for a few days. Ask the person who invited you to send a new one.',
    },
    used: {
      title: 'This invitation was already accepted',
      body: 'Sign in with your email and the password you chose when you accepted it.',
    },
    invalid: {
      title: 'This invitation link no longer works',
      body: 'A newer invitation replaces older ones, and a cancelled one stops working. Use the newest email, or ask the person who invited you to send a new one.',
    },
  },
  reset: {
    missing: {
      title: 'This reset link is incomplete',
      body: 'Part of the link is missing. Open it again from the email, or copy the whole address into your browser.',
    },
    expired: {
      title: 'This reset link has expired',
      body: 'Reset links work for a short time after they are sent. Ask for a new one.',
    },
    used: {
      title: 'This reset link was already used',
      body: "Each reset link works once. If you didn't finish choosing a password, ask for a new link.",
    },
    invalid: {
      title: 'This reset link no longer works',
      body: 'A newer reset link replaces older ones. Use the newest email, or ask for a new link.',
    },
  },
};

/** An emailed link that no longer works, or never did: what happened and what to do instead. */
function DeadLink({ purpose, state }: { purpose: 'reset' | 'invitation'; state: DeadState }) {
  const go = useLogin((s) => s.go);
  const passwordReset = useLogin((s) => s.options?.password_reset ?? false);
  const invitation = purpose === 'invitation';
  const { title, body: deadBody } = DEAD[purpose][state];
  const offerReset = !invitation && passwordReset;
  // A gateway without password resets can't send a newer link: say whom to ask instead.
  const body =
    !invitation && !passwordReset && state !== 'missing'
      ? "This gateway doesn't send reset links. Ask a gateway admin to set a new password for you."
      : deadBody;
  return (
    <>
      <StepHeader title={title} />
      <div className="flex flex-col gap-4">
        <p className="text-body leading-relaxed text-fg">{body}</p>
        {invitation && state !== 'used' ? (
          <Note>Already accepted it? Sign in with your email and the password you chose.</Note>
        ) : null}
        {offerReset ? (
          <Button variant="primary" className="h-10 w-full text-base" onClick={() => go({ kind: 'forgot' })}>
            Send a new reset link
          </Button>
        ) : (
          <Button variant="primary" className="h-10 w-full text-base" onClick={() => go({ kind: 'start' })}>
            Go to sign in
          </Button>
        )}
      </div>
      {offerReset ? (
        <MoreOptions>
          <TextButton className="self-start" onClick={() => go({ kind: 'start' })}>
            Back to sign in
          </TextButton>
        </MoreOptions>
      ) : null}
    </>
  );
}

/** Setting a password from an emailed link: accepting an invitation, or a reset. */
export function ResetStep() {
  const step = useLogin((s) => s.step);
  const confirm = useLogin((s) => s.confirmReset);
  const checkLink = useLogin((s) => s.checkLink);
  const busy = useLogin((s) => s.busy);
  const error = useLogin((s) => s.error);
  const passwordError = useLogin((s) => s.passwordError);
  const organization = useLogin((s) => s.organization);
  const policy = usePasswordPolicy();
  const [values, setValues] = useState({ password: '', confirm: '' });
  const [errors, setErrors] = useState<Record<string, string | undefined>>({});
  const reset = step.kind === 'reset' ? step : null;

  useEffect(() => {
    if (reset?.link === 'checking') void checkLink();
  }, [reset?.link, checkLink]);

  if (!reset) return null;
  const invitation = reset.purpose === 'invitation';
  if (reset.link === 'checking') {
    return (
      <>
        <StepHeader title={invitation ? 'Accept your invitation' : 'Choose a new password'} />
        <p className="flex items-center gap-2 text-ui text-muted" role="status">
          <Spinner /> Checking your link…
        </p>
      </>
    );
  }
  if (
    reset.link === 'invalid' ||
    reset.link === 'missing' ||
    reset.link === 'expired' ||
    reset.link === 'used'
  )
    return <DeadLink purpose={reset.purpose} state={reset.link} />;
  const invitedAs = invitation ? (reset.facts?.email ?? null) : null;
  const joining = invitation ? (reset.facts?.organization ?? null) : null;
  const left = timeLeft(reset.facts?.expiresAt);

  const passwordMessage = errors.password ?? passwordError ?? undefined;
  return (
    <>
      <StepHeader
        title={
          invitation ? (joining ? `Join ${joining}` : 'Accept your invitation') : 'Choose a new password'
        }
        step={invitation ? { current: 1, total: 3 } : null}
        subtitle={
          invitation ? (
            <>
              {organization && !joining ? (
                <>
                  <OrganizationLine />
                  <br />
                </>
              ) : null}
              {invitedAs ? (
                <>
                  You were invited as <span className="font-medium text-fg">{invitedAs}</span>.{' '}
                </>
              ) : null}
              Choose a password for your account. Next, you&apos;ll set up two-step verification.
            </>
          ) : (
            "After this, you'll confirm with your verification code as usual."
          )
        }
      />
      <form
        className="flex flex-col gap-4"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          const result = validate(resetSchema(policy), values);
          if (!result.ok) {
            setErrors(result.errors);
            return;
          }
          setErrors({});
          void confirm(result.data.password);
        }}
      >
        <ErrorStrip message={error} />
        <Field label="New password" error={passwordMessage}>
          <SecretInput
            autoComplete="new-password"
            value={values.password}
            onChange={(e) => {
              setValues({ ...values, password: e.target.value });
              if (errors.password) setErrors({ ...errors, password: undefined });
              if (passwordError) useLogin.setState({ passwordError: null });
            }}
            autoFocus
          />
        </Field>
        <PasswordRules password={values.password} min={policy.min} />
        <Field label="Confirm password" error={errors.confirm}>
          <SecretInput
            autoComplete="new-password"
            value={values.confirm}
            onChange={(e) => {
              setValues({ ...values, confirm: e.target.value });
              if (errors.confirm) setErrors({ ...errors, confirm: undefined });
            }}
          />
        </Field>
        <Button type="submit" variant="primary" busy={busy} className="mt-1 h-10 w-full text-base">
          {invitation ? 'Set password and continue' : 'Reset password'}
        </Button>
        {left ? (
          <p className="text-center text-caption text-dim">This link works once, for another {left}.</p>
        ) : null}
      </form>
    </>
  );
}
