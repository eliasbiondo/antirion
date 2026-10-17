'use client';

import { useRef, useState } from 'react';
import { z } from 'zod';

import { Button } from '@/components/ui/button';
import { Checkbox, Field, Input } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { plural } from '@/lib/format';
import { validate, type FieldErrors } from '@/lib/validation';

import { authApi } from '../api';
import { useLogin, useTrustDays } from '../store';
import { ErrorStrip, MoreOptions, Note, SecretInput, StepHeader, TextButton } from './parts';

export { ErrorStrip } from './parts';

/** The protocol behind each SSO button, as a quiet tag (OAuth sign-in is GitHub's). */
const PROTOCOL: Record<string, string> = { oidc: 'OIDC', saml: 'SAML', oauth: 'GitHub' };

function without(errors: FieldErrors, key: string): FieldErrors {
  return Object.fromEntries(Object.entries(errors).filter(([k]) => k !== key));
}

const localSchema = z.object({
  email: z
    .string()
    .trim()
    .min(1, 'Enter your email')
    .regex(/^[^@\s]+@[^@\s]+$/, 'Enter an email like name@example.com'),
  password: z.string().min(1, 'Enter your password'),
});

/** "Organization acme · Change" when one is chosen; it only picks the methods and where the console opens. */
export function OrganizationLine() {
  const organization = useLogin((s) => s.organization);
  const go = useLogin((s) => s.go);
  if (!organization) return null;
  return (
    <span className="inline-flex flex-wrap items-baseline gap-x-1.5">
      <span>
        Organization <span className="font-medium text-fg">{organization}</span>
      </span>
      <span aria-hidden="true">·</span>
      <TextButton tone="accent" onClick={() => go({ kind: 'organization' })}>
        Change
      </TextButton>
    </span>
  );
}

function LocalForm({ autoFocus }: { autoFocus: boolean }) {
  const signInLocal = useLogin((s) => s.signInLocal);
  const busy = useLogin((s) => s.busy);
  const email = useLogin((s) => s.email);
  const setEmail = useLogin((s) => s.setEmail);
  const passwordReset = useLogin((s) => s.options?.password_reset ?? false);
  const go = useLogin((s) => s.go);
  const [password, setPassword] = useState('');
  const [errors, setErrors] = useState<FieldErrors>({});
  const passwordRef = useRef<HTMLInputElement>(null);

  return (
    <form
      className="flex flex-col gap-4"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        const result = validate(localSchema, { email, password });
        if (!result.ok) {
          setErrors(result.errors);
          return;
        }
        setErrors({});
        void signInLocal(result.data.email, result.data.password).then((ok) => {
          if (ok) return;
          // A refused sign-in keeps the email and asks for the password again.
          setPassword('');
          passwordRef.current?.focus();
        });
      }}
    >
      <Field label="Email" error={errors.email}>
        <Input
          type="email"
          name="email"
          autoComplete="username"
          inputMode="email"
          spellCheck={false}
          autoCapitalize="off"
          value={email}
          onChange={(e) => {
            setEmail(e.target.value);
            if (errors.email) setErrors(without(errors, 'email'));
          }}
          placeholder="name@example.com"
          autoFocus={autoFocus}
          className="h-10 text-base"
        />
      </Field>
      <Field
        label="Password"
        error={errors.password}
        aside={
          passwordReset ? (
            <TextButton tone="accent" className="text-caption" onClick={() => go({ kind: 'forgot' })}>
              Forgot password?
            </TextButton>
          ) : null
        }
      >
        <SecretInput
          ref={passwordRef}
          name="password"
          autoComplete="current-password"
          value={password}
          onChange={(e) => {
            setPassword(e.target.value);
            if (errors.password) setErrors(without(errors, 'password'));
          }}
        />
      </Field>
      <Button type="submit" variant="primary" busy={busy} className="mt-1 h-10 w-full text-base">
        Sign in
      </Button>
      <p className="flex items-start gap-1.5 text-caption leading-relaxed text-dim">
        <span className="mt-px shrink-0">
          <Icon name="shield" size={12} />
        </span>
        Password sign-in always asks for a second step: a code from your authenticator app, or a security key.
      </p>
    </form>
  );
}

/** The first step: SSO buttons, then email and password, then the other ways in. */
export function MethodsStep({ next }: { next: string }) {
  const options = useLogin((s) => s.options);
  const error = useLogin((s) => s.error);
  const organization = useLogin((s) => s.organization);
  const go = useLogin((s) => s.go);
  const trust = useLogin((s) => s.trustDevice);
  const setTrust = useLogin((s) => s.setTrustDevice);
  const trustDays = useTrustDays();
  const methods = options?.methods ?? [];
  const sso = methods.filter((m) => m.type === 'oidc' || m.type === 'saml' || m.type === 'oauth');
  // Only an OpenID provider can vouch for a second step, which trusting a browser needs.
  const trustable = trustDays !== null && sso.some((m) => m.type === 'oidc');
  const local = methods.some((m) => m.type === 'local');
  const returnTo = `${process.env.NEXT_PUBLIC_UI_PATH ?? ''}${next}`;

  return (
    <>
      <StepHeader title="Sign in" subtitle={organization ? <OrganizationLine /> : null} />
      <div className="flex flex-col gap-4">
        <ErrorStrip message={error} />
        {sso.length > 0 ? (
          <ul className="flex flex-col gap-2" aria-label="Single sign-on">
            {sso.map((m) => {
              const name = m.display_name ?? m.id;
              return (
                <li key={`${m.type}-${m.id}`}>
                  <a
                    href={authApi.ssoUrl(
                      m.type as 'oidc' | 'oauth' | 'saml',
                      m.id,
                      returnTo,
                      trustable && trust,
                    )}
                    className="flex h-10 items-center gap-3 rounded-sm border border-line bg-raised px-3.5 text-base font-medium text-fg hover:border-line-strong hover:bg-hover"
                  >
                    <span
                      aria-hidden="true"
                      className="flex h-5 w-5 shrink-0 items-center justify-center rounded-xs border border-line-strong bg-sunken text-caption font-semibold text-muted"
                    >
                      {name.charAt(0).toUpperCase()}
                    </span>
                    <span className="flex-1 truncate">Continue with {name}</span>
                    <span className="text-caption font-normal text-dim">
                      {PROTOCOL[m.type] ?? m.type.toUpperCase()}
                    </span>
                  </a>
                </li>
              );
            })}
          </ul>
        ) : null}
        {trustable && trustDays ? (
          <Checkbox
            label={`Trust this browser for ${plural(trustDays, 'day')}`}
            description="When your identity provider confirms a second step, you won't sign in again on this browser until then."
            checked={trust}
            onChange={(v) => setTrust(v)}
          />
        ) : null}
        {sso.length > 0 && local ? (
          <div className="flex items-center gap-3 text-caption text-dim">
            <span className="h-px flex-1 bg-line" />
            or
            <span className="h-px flex-1 bg-line" />
          </div>
        ) : null}
        {local ? <LocalForm autoFocus={sso.length === 0} /> : null}
      </div>
      <MoreOptions>
        <div className="flex flex-col gap-1">
          <TextButton icon="key" className="self-start" onClick={() => go({ kind: 'token' })}>
            Use an admin token
          </TextButton>
          <p className="pl-5 text-caption leading-relaxed text-dim">
            Setting up a new gateway? Sign in with the bootstrap token from its configuration.
          </p>
        </div>
        {options?.device_authorization ? (
          <TextButton icon="terminal" className="self-start" onClick={() => go({ kind: 'cli' })}>
            Sign in from a terminal
          </TextButton>
        ) : null}
        {organization ? null : (
          <TextButton icon="building" className="self-start" onClick={() => go({ kind: 'organization' })}>
            Choose an organization
          </TextButton>
        )}
      </MoreOptions>
    </>
  );
}

const tokenSchema = z.object({ token: z.string().trim().min(8, 'Paste your admin token') });

/** The admin-token form: the token goes once to `POST /auth/session` and is never stored. */
export function TokenStep() {
  const signIn = useLogin((s) => s.signInWithToken);
  const busy = useLogin((s) => s.busy);
  const error = useLogin((s) => s.error);
  const organization = useLogin((s) => s.organization);
  const go = useLogin((s) => s.go);
  const options = useLogin((s) => s.options);
  const [token, setToken] = useState('');
  const [fieldError, setFieldError] = useState<string | undefined>();
  const inputRef = useRef<HTMLInputElement>(null);
  const onlyToken = (options?.methods ?? []).every((m) => m.type === 'token');
  const setup = options?.setup_required ?? false;

  return (
    <>
      <StepHeader
        title={setup ? 'Set up your gateway' : 'Sign in with an admin token'}
        subtitle={
          setup ? (
            'This gateway has no accounts yet. Sign in with its bootstrap token, then invite your team so everyone signs in as themselves.'
          ) : organization ? (
            <OrganizationLine />
          ) : onlyToken ? (
            'This gateway accepts admin tokens only.'
          ) : null
        }
      />
      <form
        className="flex flex-col gap-4"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          const result = validate(tokenSchema, { token });
          if (!result.ok) {
            setFieldError(result.errors.token);
            return;
          }
          setFieldError(undefined);
          void signIn(result.data.token).then((ok) => {
            if (ok) setToken('');
            else inputRef.current?.select();
          });
        }}
      >
        <ErrorStrip message={error} />
        {/* Lets a password manager file the token under a recognisable name. */}
        <input
          type="text"
          name="username"
          autoComplete="username"
          value="Admin token"
          readOnly
          tabIndex={-1}
          aria-hidden="true"
          className="sr-only"
        />
        <Field
          label="Admin token"
          error={fieldError}
          hint={
            setup
              ? "Paste the bootstrap token. It's used once to start your session and isn't stored."
              : "Paste an admin token or the bootstrap token. It's used once to start your session and isn't stored."
          }
        >
          <SecretInput
            ref={inputRef}
            what="token"
            mono
            name="password"
            autoComplete="current-password"
            value={token}
            onChange={(e) => {
              setToken(e.target.value);
              if (fieldError) setFieldError(undefined);
            }}
            autoFocus
          />
        </Field>
        <Button type="submit" variant="primary" busy={busy} className="mt-1 h-10 w-full text-base">
          Sign in
        </Button>
        <Note>
          <p>
            {setup
              ? 'The bootstrap token is the value of'
              : 'On a new gateway, use the bootstrap token: the value of'}{' '}
            <code className="mono text-fg">auth.admin.bootstrap_token</code> in its configuration.
            {setup
              ? ' Keep it for emergencies once your team has accounts.'
              : ' Then invite your team, so everyone signs in with their own account.'}
          </p>
        </Note>
      </form>
      <MoreOptions>
        {onlyToken ? null : (
          <TextButton
            icon="lock"
            className="self-start"
            onClick={() => go({ kind: setup ? 'methods' : 'start' })}
          >
            Use another sign-in method
          </TextButton>
        )}
        {organization || setup ? null : (
          <TextButton icon="building" className="self-start" onClick={() => go({ kind: 'organization' })}>
            Choose an organization
          </TextButton>
        )}
      </MoreOptions>
    </>
  );
}

const organizationSchema = z.object({
  organization: z
    .string()
    .trim()
    .min(1, "Enter your organization's ID")
    .regex(/^\S+$/, "An organization ID doesn't contain spaces"),
});

/** Asked before the credentials, because the organization decides which ways in are offered. */
export function OrganizationStep() {
  const current = useLogin((s) => s.organization);
  const choose = useLogin((s) => s.chooseOrganization);
  const busy = useLogin((s) => s.busy);
  const go = useLogin((s) => s.go);
  const [value, setValue] = useState(current);
  const [fieldError, setFieldError] = useState<string | undefined>();

  return (
    <>
      <StepHeader
        title="Choose your organization"
        subtitle="Your organization may have its own way to sign in. It also sets where the console opens; it doesn't change what you can access."
      />
      <form
        className="flex flex-col gap-4"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          const result = validate(organizationSchema, { organization: value });
          if (!result.ok) {
            setFieldError(result.errors.organization);
            return;
          }
          setFieldError(undefined);
          void choose(result.data.organization);
        }}
      >
        <Field
          label="Organization ID"
          error={fieldError}
          hint="Not sure? Ask your gateway admin, or use the sign-in link they shared."
        >
          <Input
            mono
            autoComplete="organization"
            spellCheck={false}
            autoCapitalize="off"
            value={value}
            onChange={(e) => {
              setValue(e.target.value);
              if (fieldError) setFieldError(undefined);
            }}
            placeholder="e.g. acme"
            autoFocus
            className="h-10 text-base"
          />
        </Field>
        <Button type="submit" variant="primary" busy={busy} className="mt-1 h-10 w-full text-base">
          Continue
        </Button>
      </form>
      <MoreOptions>
        {current ? (
          <TextButton className="self-start" onClick={() => void choose('')}>
            Sign in without an organization
          </TextButton>
        ) : (
          <TextButton className="self-start" onClick={() => go({ kind: 'start' })}>
            Back to sign in
          </TextButton>
        )}
      </MoreOptions>
    </>
  );
}
