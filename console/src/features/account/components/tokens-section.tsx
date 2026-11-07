'use client';

import { useEffect, useState } from 'react';
import { z } from 'zod';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Segmented, Select } from '@/components/ui/form';
import { RevealOnceDialog } from '@/components/ui/reveal-once';
import { EmptyState, InlineError, LoadError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { formatDate, formatRelative } from '@/lib/time';
import { validate, type FieldErrors } from '@/lib/validation';
import { usePreferences } from '@/stores/preferences';
import { useSession } from '@/stores/session';

import { useAccount } from '../store';
import { expiryFor, lifetimeChoices, lifetimeWords, tokenPolicy } from '../token-policy';
import { useTranslate } from '@/lib/i18n';

type Access = 'read_only' | 'full';

const tokenSchema = z.object({
  description: z
    .string()
    .trim()
    .min(1, 'Name the token after what will use it')
    .max(500, 'Use 500 characters or fewer'),
  lifetime: z.coerce.number().int().positive('Choose when the token expires'),
  access: z.enum(['full', 'read_only']),
});

const ACCESS_LABEL: Record<Access, string> = { read_only: 'Read only', full: 'Read and write' };

export function TokensSection() {
  const translate = useTranslate();
  const tokens = useAccount((s) => s.tokens);
  const load = useAccount((s) => s.loadTokens);
  const create = useAccount((s) => s.createToken);
  const remove = useAccount((s) => s.deleteToken);
  const timeZone = usePreferences((s) => s.timeZone);
  const me = useSession((s) => s.me);
  const policy = tokenPolicy(me);
  const lifetimes = lifetimeChoices(policy);
  const blank = { description: '', lifetime: lifetimes.initial, access: 'read_only' as Access };
  const [values, setValues] = useState<{ description: string; lifetime: string; access: Access }>(blank);
  // The rule can arrive after the form: an option it no longer offers falls back to its default.
  const lifetime = lifetimes.options.some((o) => o.value === values.lifetime)
    ? values.lifetime
    : lifetimes.initial;
  /** The gateway answers 404 on its token list while personal tokens are turned off. */
  const turnedOff = tokens.error?.notFound === true;
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [secret, setSecret] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<{ kid: string; name: string } | null>(null);

  useEffect(() => {
    void load();
  }, [load]);

  async function submit() {
    const result = validate(tokenSchema, { ...values, lifetime });
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      const { description, access } = result.data;
      setSecret(await create({ description, access, expiresAt: expiryFor(result.data.lifetime, policy) }));
      setValues(blank);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-6">
      <p className="text-ui text-muted">
        {translate(
          'Use a personal token in scripts and the CLI. It acts as you, with your permissions; choose Read only to limit it.',
        )}
      </p>
      {turnedOff ? (
        <p role="status" className="rounded-sm border border-line bg-sunken px-3 py-2 text-ui text-muted">
          Personal tokens are turned off on this gateway. An administrator can turn them on in Gateway
          settings, under Sign-in &amp; SSO.
        </p>
      ) : null}
      {turnedOff ? null : (
        <form
          className="flex flex-col gap-4 rounded-sm border border-line p-4"
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-[minmax(0,1fr)_140px]">
            <Field label={translate('Name')} error={errors.description}>
              <Input
                value={values.description}
                onChange={(e) => {
                  setValues({ ...values, description: e.target.value });
                  setErrors({});
                }}
                placeholder={translate('e.g. Deploy script')}
              />
            </Field>
            <Field
              label={translate('Expires')}
              error={errors.lifetime}
              hint={translate('At most {0}.', lifetimeWords(Number(lifetimes.options.at(-1)?.value ?? 0)))}
            >
              <Select
                value={lifetime}
                onChange={(v) => setValues({ ...values, lifetime: v })}
                options={lifetimes.options}
              />
            </Field>
          </div>
          <Field label={translate('Access')}>
            <Segmented<Access>
              label={translate('Access')}
              value={values.access}
              onChange={(access) => setValues({ ...values, access })}
              options={[
                { value: 'read_only', label: ACCESS_LABEL.read_only },
                { value: 'full', label: ACCESS_LABEL.full },
              ]}
            />
          </Field>
          <div className="flex flex-col gap-3">
            <InlineError error={error} />
            <div>
              <Button type="submit" variant="primary" icon="plus" busy={busy}>
                {translate('Create token')}
              </Button>
            </div>
          </div>
        </form>
      )}
      {tokens.status === 'loading' && !tokens.data ? <LoadingRows rows={3} /> : null}
      {turnedOff ? null : (
        <LoadError of={tokens} subject={translate('personal tokens')} onRetry={() => void load()} />
      )}
      {tokens.data?.length === 0 ? <EmptyState title={translate('No personal tokens yet')} /> : null}
      {tokens.data && tokens.data.length > 0 ? (
        <Table label={translate('Personal tokens')}>
          <thead>
            <tr>
              <Th>{translate('Name')}</Th>
              <Th>{translate('Access')}</Th>
              <Th>{translate('Created')}</Th>
              <Th>{translate('Expires')}</Th>
              <Th>{translate('Last used')}</Th>
              <Th>
                <span className="sr-only">{translate('Actions')}</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {tokens.data.map((t) => (
              <Tr key={t.kid}>
                <Td>{t.description || translate('Untitled token')}</Td>
                <Td>
                  <Badge tone={t.access === 'full' ? 'warn' : 'neutral'}>{ACCESS_LABEL[t.access]}</Badge>
                </Td>
                <Td muted>{formatDate(t.created_at, timeZone)}</Td>
                <Td muted>{formatDate(t.expires_at, timeZone)}</Td>
                <Td muted>{t.last_used_at ? formatRelative(t.last_used_at) : translate('Never')}</Td>
                <Td className="text-end">
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => setRevoking({ kid: t.kid, name: t.description || t.kid })}
                  >
                    {translate('Revoke')}
                  </Button>
                </Td>
              </Tr>
            ))}
          </tbody>
        </Table>
      ) : null}
      <RevealOnceDialog
        secret={secret}
        onClose={() => setSecret(null)}
        title={translate('Your new personal token')}
        label={translate('Token')}
        description={translate('Use it as a bearer token in scripts and the CLI.')}
      />
      <ConfirmDialog
        open={revoking !== null}
        onClose={() => setRevoking(null)}
        title={translate('Revoke this token?')}
        objectName={revoking?.name}
        typeToConfirm={revoking?.name}
        effect="Scripts using it will stop working right away. This can't be undone."
        confirmLabel={translate('Revoke token')}
        withReason={false}
        onConfirm={async () => {
          if (revoking) await remove(revoking.kid);
        }}
      />
    </div>
  );
}
