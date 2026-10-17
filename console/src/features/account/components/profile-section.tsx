'use client';

import Link from 'next/link';
import { useId, useState } from 'react';
import { z } from 'zod';

import { Button } from '@/components/ui/button';
import { FileButton, Field, Input } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { Avatar } from '@/components/ui/misc';
import { InlineError } from '@/components/ui/states';
import { ApiError } from '@/lib/api/client';
import { validate, type FieldErrors } from '@/lib/validation';
import { usePreferences } from '@/stores/preferences';
import { highestRole, useSession } from '@/stores/session';
import { notify } from '@/stores/ui';

import { hasAccount, principalName, roleLabel } from '../describe';
import { PhotoError, shrinkPhoto } from '../photo';
import { useAccount } from '../store';

const profileSchema = z.object({
  display_name: z.string().trim().min(1, 'Enter the name people see').max(200, 'Use 200 characters or fewer'),
  title: z.string().trim().max(100, 'Use 100 characters or fewer'),
});

/** Upload or remove the photo shown instead of the initials; it is kept, shrunk, in preferences. */
function PhotoControls() {
  const photo = usePreferences((s) => s.photo);
  const setPreferences = usePreferences((s) => s.set);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="flex flex-col items-start gap-1 sm:items-end">
      <div className="flex flex-wrap items-center gap-2">
        <FileButton
          size="sm"
          icon="user"
          accept="image/png,image/jpeg,image/webp,image/gif"
          busy={busy}
          onFile={(file) => {
            setBusy(true);
            setError(null);
            shrinkPhoto(file)
              .then((url) => {
                setPreferences({ photo: url });
                notify('success', 'Photo updated');
              })
              .catch((e: unknown) =>
                setError(
                  e instanceof PhotoError ? e.message : "That image couldn't be used. Try another one.",
                ),
              )
              .finally(() => setBusy(false));
          }}
        >
          {photo ? 'Change photo' : 'Upload photo'}
        </FileButton>
        {photo ? (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setPreferences({ photo: null });
              notify('success', 'Photo removed');
            }}
          >
            Remove
          </Button>
        ) : null}
      </div>
      {error ? (
        <p role="alert" className="text-caption text-error">
          {error}
        </p>
      ) : (
        <p className="text-caption text-dim">Shown instead of your initials.</p>
      )}
    </div>
  );
}

/** Who is signed in: avatar, name, handle and email, and the role that decides what they can do. */
function Identity() {
  const me = useSession((s) => s.me);
  const user = me?.user && typeof me.user === 'object' ? me.user : null;
  const name = principalName(me);
  const role = roleLabel(highestRole(me));
  const account = hasAccount(me);
  const photo = usePreferences((s) => (account ? s.photo : null));
  return (
    <div className="flex flex-wrap items-center gap-4">
      <Avatar name={name} size="lg" photo={photo} />
      <div className="min-w-0 flex-1">
        <h3 className="truncate text-title font-semibold text-fg">{name}</h3>
        {account ? (
          <p className="truncate text-ui text-muted">
            <span className="mono">@{user?.name ?? me?.principal.id}</span>
            {user?.email ? ` · ${user.email}` : null}
          </p>
        ) : (
          <p className="text-ui text-muted">
            Signed in with {me?.principal.type === 'bootstrap' ? 'the bootstrap token' : 'an admin token'}
          </p>
        )}
        {role ? <p className="text-caption text-dim">{role}</p> : null}
      </div>
      {account ? <PhotoControls /> : null}
    </div>
  );
}

const emailSchema = z
  .string()
  .trim()
  .min(1, 'Enter your new email')
  .max(254, 'Use 254 characters or fewer')
  .regex(/^[^@\s]+@[^@\s]+$/, 'Enter an email like name@example.com');

/** Plain words for a refused email change. */
function emailMessage(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.status === 0) return "Can't reach the gateway. Check your connection and try again.";
    if (/mail is not enabled/i.test(e.message))
      return "This gateway can't send email, so it can't confirm a new address. Ask an administrator to change it.";
    if (e.code === 'invalid_parameter') return 'Enter an email like name@example.com.';
    return e.message;
  }
  return 'Something went wrong. Try again.';
}

/**
 * The account's email. A password account's new email takes effect only once the link mailed to it
 * is followed; a single sign-on account's at once.
 */
function EmailBlock() {
  const me = useSession((s) => s.me);
  const changeEmail = useAccount((s) => s.changeEmail);
  const current = me?.user?.email ?? null;
  const emailId = useId();
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState('');
  const [fieldError, setFieldError] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit() {
    const result = validate(emailSchema, value);
    if (!result.ok) {
      setFieldError(result.errors[''] ?? 'Enter an email like name@example.com');
      return;
    }
    if (result.data.toLowerCase() === current?.toLowerCase()) {
      setFieldError('That is already your email');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const applied = await changeEmail(result.data);
      setEditing(false);
      setValue('');
      if (applied) {
        setPending(null);
        notify('success', 'Email changed', result.data);
      } else setPending(result.data);
    } catch (e) {
      setError(emailMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-2">
      <Field
        label="Email"
        controlId={emailId}
        hint={editing ? undefined : 'Where invitations, resets and notifications go.'}
      >
        <div className="flex flex-wrap items-center gap-2">
          <Input
            id={emailId}
            value={current ?? ''}
            readOnly
            className="min-w-0 flex-1"
            placeholder="No email"
          />
          {editing ? null : (
            <Button size="sm" onClick={() => setEditing(true)}>
              Change email
            </Button>
          )}
        </div>
      </Field>
      {pending ? (
        <p role="status" className="flex items-start gap-2 text-caption leading-relaxed text-muted">
          <span className="mt-px text-info">
            <Icon name="mail" size={13} />
          </span>
          <span>
            We sent a link to <span className="font-medium text-fg">{pending}</span>. Your email changes when
            you open it while signed in; until then you keep using {current ?? 'your current address'}.
          </span>
        </p>
      ) : null}
      {editing ? (
        <form
          noValidate
          className="flex flex-col gap-3 rounded-sm border border-line bg-sunken p-3"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <Field label="New email" error={fieldError}>
            <Input
              type="email"
              autoComplete="email"
              inputMode="email"
              spellCheck={false}
              autoCapitalize="off"
              value={value}
              placeholder="name@example.com"
              autoFocus
              onChange={(e) => {
                setValue(e.target.value);
                if (fieldError) setFieldError(undefined);
              }}
            />
          </Field>
          <p className="text-caption text-muted">
            If you sign in with a password, we&apos;ll email the new address a link to confirm it.
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <Button type="submit" variant="primary" size="sm" busy={busy}>
              Change email
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                setEditing(false);
                setValue('');
                setFieldError(undefined);
                setError(null);
              }}
            >
              Cancel
            </Button>
            {error ? (
              <span role="alert" className="text-caption text-error">
                {error}
              </span>
            ) : null}
          </div>
        </form>
      ) : null}
    </div>
  );
}

/**
 * Shown instead of the account-only sections when the principal is an admin token: what is
 * missing, why, and how to get it.
 */
export function TokenPrincipalNote({ what }: { what?: 'profile' | 'tokens' | 'notifications' }) {
  const bootstrap = useSession((s) => s.me?.principal.type === 'bootstrap');
  const lead =
    what === 'tokens'
      ? 'Personal tokens belong to a user account.'
      : what === 'notifications'
        ? 'Email notifications go to a user account.'
        : "You're signed in with an admin token, so there's no profile to edit.";
  return (
    <div className="flex flex-col gap-4 rounded-sm border border-line bg-sunken p-4">
      <div className="flex items-start gap-3">
        <span className="mt-0.5 text-muted">
          <Icon name="key" size={16} />
        </span>
        <div className="flex flex-col gap-2 text-body leading-relaxed text-fg">
          <p>
            {lead} Invite yourself as a user to get a profile, personal tokens, two-step verification and
            email notifications.
          </p>
          {bootstrap ? (
            <p className="text-muted">
              The bootstrap token is meant for setting up the gateway. Once your team has accounts, keep it
              somewhere safe and stop using it day to day.
            </p>
          ) : null}
        </div>
      </div>
      <div className="pl-7">
        <Link
          href="/teams/"
          className="inline-flex h-8 items-center gap-2 rounded-sm border border-line bg-raised px-3 text-ui font-medium text-fg hover:border-line-strong hover:bg-hover"
        >
          <Icon name="users" size={13} />
          Go to Teams &amp; users
        </Link>
      </div>
    </div>
  );
}

export function ProfileSection({ onDirty }: { onDirty: (dirty: boolean) => void }) {
  const me = useSession((s) => s.me);
  const updateProfile = useAccount((s) => s.updateProfile);
  const user = me?.user && typeof me.user === 'object' ? me.user : null;
  const [values, setValues] = useState({
    display_name: user?.display_name ?? '',
    title: user?.title ?? '',
  });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

  function change(patch: Partial<typeof values>) {
    setValues((v) => ({ ...v, ...patch }));
    setErrors({});
    onDirty(true);
  }

  async function save() {
    const result = validate(profileSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      await updateProfile(result.data);
      onDirty(false);
      notify('success', 'Profile saved');
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-label="Profile" className="flex flex-col gap-6">
      <Identity />
      {user && hasAccount(me) ? (
        <form
          className="grid grid-cols-1 gap-4 sm:grid-cols-2"
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <Field label="Display name" error={errors.display_name} required>
            <Input
              autoComplete="name"
              value={values.display_name}
              onChange={(e) => change({ display_name: e.target.value })}
            />
          </Field>
          <Field label="Title" optional hint="Shown next to your name, such as Platform engineer.">
            <Input
              autoComplete="organization-title"
              value={values.title}
              onChange={(e) => change({ title: e.target.value })}
            />
          </Field>
          <Field label="User name" hint="Used in mentions and the audit log. It can't be changed.">
            <Input value={user.name} readOnly mono />
          </Field>
          <div className="flex items-center gap-3 sm:col-span-2">
            <Button type="submit" variant="primary" busy={busy}>
              Save profile
            </Button>
            <InlineError error={error} />
          </div>
        </form>
      ) : null}
      {user && hasAccount(me) ? (
        <div className="border-t border-line pt-5">
          <EmailBlock />
        </div>
      ) : (
        <TokenPrincipalNote what="profile" />
      )}
    </section>
  );
}
