'use client';

import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/choice';
import { Field, Input, NumberInput, Select } from '@/components/ui/form';
import { permissionOf, useCan } from '@/components/ui/gate';
import { noPermissionReason } from '@/lib/permissions';
import { ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { describeError } from '@/lib/api/client';
import type { FieldErrors } from '@/lib/validation';
import { useSession } from '@/stores/session';
import { notify } from '@/stores/ui';

import { settingsApi } from '../api';
import { detailErrors, str, mergePatch, same } from '../form-errors';
import { useLinks } from '../store';
import { REDACTED } from './connection-drawers';
import { Card, SaveBar } from './parts';

interface SmtpForm {
  enabled: boolean;
  host: string;
  port: string;
  tls: 'starttls' | 'tls' | 'none';
  username: string;
  password: string;
  from: string;
  replyTo: string;
  rate: string;
}

const EMAIL_RE = /^[^@\s]+@[^@\s]+$|^.+<[^@\s]+@[^@\s]+>$/;

function SmtpForm({ spec }: { spec: Record<string, unknown> }) {
  const save = useLinks((s) => s.saveSmtp);
  const canEdit = useCan('patchSmtpSettings');
  const canTest = useCan('testSmtp');
  const me = useSession((s) => s.me);
  const hasPassword = spec.password === REDACTED || typeof spec.password === 'string';
  const initial: SmtpForm = {
    enabled: spec.enabled === true,
    host: str(spec.host, ''),
    port: str(spec.port, '587'),
    tls: (spec.tls as SmtpForm['tls'] | undefined) ?? 'starttls',
    username: str(spec.username, ''),
    password: '',
    from: str(spec.from, ''),
    replyTo: str(spec.reply_to, ''),
    rate: str(spec.max_messages_per_minute, '60'),
  };
  const [values, setValues] = useState(initial);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [to, setTo] = useState(me?.user?.email ?? '');
  const [testing, setTesting] = useState(false);
  const dirty = !same(values, initial);
  const set = <K extends keyof SmtpForm>(k: K, v: SmtpForm[K]) => {
    setValues((x) => ({ ...x, [k]: v }));
    if (errors[k]) setErrors(({ [k]: _, ...rest }) => rest);
  };
  async function submit() {
    const e: FieldErrors = {};
    if (values.enabled || values.host) {
      if (!values.host.trim()) e.host = 'Enter the mail server';
      if (!/^\d+$/.test(values.port) || Number(values.port) < 1 || Number(values.port) > 65535)
        e.port = 'Enter a port from 1 to 65535';
      if (!EMAIL_RE.test(values.from.trim())) e.from = 'Enter the sender, such as gateway@example.com';
    }
    if (values.replyTo && !EMAIL_RE.test(values.replyTo.trim()))
      e.replyTo = 'Enter an address such as ops@example.com';
    if (!/^\d+$/.test(values.rate) || Number(values.rate) < 1) e.rate = 'Enter 1 or more';
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    const next: Record<string, unknown> = {
      ...spec,
      enabled: values.enabled,
      host: values.host.trim() || undefined,
      port: Number(values.port),
      tls: values.tls,
      username: values.username.trim() || undefined,
      ...(values.password ? { password: values.password } : {}),
      from: values.from.trim() || undefined,
      reply_to: values.replyTo.trim() || undefined,
      max_messages_per_minute: Number(values.rate),
    };
    try {
      await save(mergePatch(spec, JSON.parse(JSON.stringify(next)) as Record<string, unknown>, ['password']));
      notify('success', 'Email server saved');
    } catch (err) {
      setError(err);
      setErrors(
        detailErrors(err, {
          'smtp.host': 'host',
          'smtp.port': 'port',
          'smtp.tls': 'tls',
          'smtp.username': 'username',
          'smtp.password': 'password',
          'smtp.from': 'from',
          'smtp.reply_to': 'replyTo',
          'smtp.max_messages_per_minute': 'rate',
        }),
      );
    } finally {
      setBusy(false);
    }
  }
  const disabled = !canEdit;
  return (
    <div className="flex flex-col gap-5">
      <form
        noValidate
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <InlineError error={error} />
        <div className="flex flex-col gap-1">
          <Switch
            checked={values.enabled}
            disabled={disabled}
            onChange={(v) => set('enabled', v)}
            label="Send email"
          />
          <p className="text-caption text-muted">
            Invitations, password resets, email alert channels and personal digests go out through this
            server.
          </p>
        </div>
        <div className="grid grid-cols-[1fr_8rem_12rem] gap-4 max-md:grid-cols-1">
          <Field label="Mail server" required={values.enabled} error={errors.host}>
            <Input
              mono
              value={values.host}
              disabled={disabled}
              placeholder="smtp.example.com"
              onChange={(e) => set('host', e.target.value)}
            />
          </Field>
          <Field label="Port" error={errors.port}>
            <NumberInput
              value={values.port}
              min={1}
              max={65535}
              disabled={disabled}
              onChange={(v) => set('port', v)}
            />
          </Field>
          <Field label="Encryption" error={errors.tls}>
            <Select
              value={values.tls}
              disabled={disabled}
              onChange={(v) => set('tls', v as SmtpForm['tls'])}
              options={[
                { value: 'starttls', label: 'STARTTLS' },
                { value: 'tls', label: 'TLS from the start' },
                { value: 'none', label: 'None (this machine only)' },
              ]}
            />
          </Field>
          <Field label="User name" optional error={errors.username}>
            <Input
              mono
              value={values.username}
              disabled={disabled}
              autoComplete="off"
              onChange={(e) => set('username', e.target.value)}
            />
          </Field>
          <Field
            label="Password"
            optional
            error={errors.password}
            hint={
              hasPassword
                ? 'Leave blank to keep it.'
                : 'Stored encrypted. A reference such as ${env:NAME} also works.'
            }
            className="md:col-span-2"
          >
            <Input
              mono
              type="password"
              autoComplete="new-password"
              value={values.password}
              disabled={disabled}
              placeholder={hasPassword ? '••••••••' : ''}
              onChange={(e) => set('password', e.target.value)}
            />
          </Field>
          <Field label="From" required={values.enabled} error={errors.from} hint="The sender people see.">
            <Input
              mono
              value={values.from}
              disabled={disabled}
              placeholder="Antirion <gateway@example.com>"
              onChange={(e) => set('from', e.target.value)}
            />
          </Field>
          <Field label="Reply to" optional error={errors.replyTo}>
            <Input
              mono
              value={values.replyTo}
              disabled={disabled}
              placeholder="ops@example.com"
              onChange={(e) => set('replyTo', e.target.value)}
            />
          </Field>
          <Field label="At most, per minute" error={errors.rate} hint="Messages; more wait.">
            <NumberInput
              value={values.rate}
              min={1}
              max={10000}
              disabled={disabled}
              onChange={(v) => set('rate', v)}
            />
          </Field>
        </div>
        <SaveBar dirty={dirty} busy={busy} canEdit={canEdit} onDiscard={() => setValues(initial)} />
      </form>
      <div className="flex flex-wrap items-end gap-3 border-t border-line pt-4">
        <Field
          label="Send a test message to"
          className="min-w-[16rem] flex-1"
          hint={dirty ? 'Save your changes first; the test uses the saved settings.' : undefined}
        >
          <Input mono value={to} placeholder="you@example.com" onChange={(e) => setTo(e.target.value)} />
        </Field>
        <Button
          icon="send"
          busy={testing}
          disabled={!canTest || !initial.enabled || !EMAIL_RE.test(to.trim())}
          disabledReason={
            !canTest
              ? noPermissionReason(permissionOf('testSmtp'))
              : !initial.enabled
                ? 'Turn on and save the email server first'
                : 'Enter an address'
          }
          onClick={() => {
            setTesting(true);
            settingsApi
              .testSmtp(to.trim())
              .then(
                (r) => {
                  const res = r as { ok?: boolean; error?: string | null };
                  if (res.ok) notify('success', 'Test message sent', `Check ${to.trim()}.`);
                  else
                    notify(
                      'error',
                      "The test message wasn't sent",
                      res.error ?? 'The mail server refused it.',
                    );
                },
                (e: unknown) => notify('error', "The test message wasn't sent", describeError(e)),
              )
              .finally(() => setTesting(false));
          }}
        >
          Send test
        </Button>
      </div>
    </div>
  );
}

/** The outgoing mail server: host, encryption, sign-in, sender and a test message. */
export function SmtpCard() {
  const smtp = useLinks((s) => s.smtp);
  const load = useLinks((s) => s.loadSmtp);
  useEffect(() => {
    void load();
  }, [load]);
  return (
    <Card title="Email server" description="How the gateway sends email.">
      {smtp.error ? <ErrorState error={smtp.error} subject="Email server" size="sm" /> : null}
      {!smtp.data && !smtp.error ? <LoadingRows rows={3} /> : null}
      {smtp.data ? <SmtpForm key={smtp.data.metadata.etag} spec={smtp.data.spec} /> : null}
    </Card>
  );
}
