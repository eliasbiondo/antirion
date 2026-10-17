'use client';

import { useEffect, useState } from 'react';
import { z } from 'zod';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Segmented, Select } from '@/components/ui/form';
import { RevealOnceDialog } from '@/components/ui/reveal-once';
import { EmptyState, ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { formatDate, formatRelative } from '@/lib/time';
import { validate, type FieldErrors } from '@/lib/validation';
import { usePreferences } from '@/stores/preferences';

import { useAccount } from '../store';

type Access = 'read_only' | 'full';

const tokenSchema = z.object({
  description: z
    .string()
    .trim()
    .min(1, 'Name the token after what will use it')
    .max(500, 'Use 500 characters or fewer'),
  days: z.coerce.number().int().min(1).max(365),
  access: z.enum(['full', 'read_only']),
});

const ACCESS_LABEL: Record<Access, string> = { read_only: 'Read only', full: 'Read and write' };

export function TokensSection() {
  const tokens = useAccount((s) => s.tokens);
  const load = useAccount((s) => s.loadTokens);
  const create = useAccount((s) => s.createToken);
  const remove = useAccount((s) => s.deleteToken);
  const timeZone = usePreferences((s) => s.timeZone);
  const [values, setValues] = useState<{ description: string; days: string; access: Access }>({
    description: '',
    days: '30',
    access: 'read_only',
  });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [secret, setSecret] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<{ kid: string; name: string } | null>(null);

  useEffect(() => {
    void load();
  }, [load]);

  async function submit() {
    const result = validate(tokenSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      setSecret(await create(result.data));
      setValues({ description: '', days: '30', access: 'read_only' });
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-6">
      <p className="text-ui text-muted">
        Use a personal token in scripts and the CLI. It acts as you, with your permissions; choose Read only
        to limit it.
      </p>
      <form
        className="flex flex-col gap-4 rounded-sm border border-line p-4"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-[minmax(0,1fr)_140px]">
          <Field label="Name" error={errors.description}>
            <Input
              value={values.description}
              onChange={(e) => {
                setValues({ ...values, description: e.target.value });
                setErrors({});
              }}
              placeholder="e.g. Deploy script"
            />
          </Field>
          <Field label="Expires" error={errors.days}>
            <Select
              value={values.days}
              onChange={(v) => setValues({ ...values, days: v })}
              options={[
                { value: '7', label: 'In 7 days' },
                { value: '30', label: 'In 30 days' },
                { value: '90', label: 'In 90 days' },
                { value: '365', label: 'In 1 year' },
              ]}
            />
          </Field>
        </div>
        <Field label="Access">
          <Segmented<Access>
            label="Access"
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
              Create token
            </Button>
          </div>
        </div>
      </form>
      {tokens.status === 'loading' && !tokens.data ? <LoadingRows rows={3} /> : null}
      {tokens.error ? (
        <ErrorState error={tokens.error} subject="personal tokens" onRetry={() => void load()} />
      ) : null}
      {tokens.data?.length === 0 ? <EmptyState title="No personal tokens yet" /> : null}
      {tokens.data && tokens.data.length > 0 ? (
        <Table label="Personal tokens">
          <thead>
            <tr>
              <Th>Name</Th>
              <Th>Access</Th>
              <Th>Created</Th>
              <Th>Expires</Th>
              <Th>Last used</Th>
              <Th>
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {tokens.data.map((t) => (
              <Tr key={t.kid}>
                <Td>{t.description || 'Untitled token'}</Td>
                <Td>
                  <Badge tone={t.access === 'full' ? 'warn' : 'neutral'}>{ACCESS_LABEL[t.access]}</Badge>
                </Td>
                <Td muted>{formatDate(t.created_at, timeZone)}</Td>
                <Td muted>{formatDate(t.expires_at, timeZone)}</Td>
                <Td muted>{t.last_used_at ? formatRelative(t.last_used_at) : 'Never'}</Td>
                <Td className="text-right">
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => setRevoking({ kid: t.kid, name: t.description || t.kid })}
                  >
                    Revoke
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
        title="Your new personal token"
        label="Token"
        description="Use it as a bearer token in scripts and the CLI."
      />
      <ConfirmDialog
        open={revoking !== null}
        onClose={() => setRevoking(null)}
        title="Revoke this token?"
        objectName={revoking?.name}
        typeToConfirm={revoking?.name}
        effect="Scripts using it will stop working right away. This can't be undone."
        confirmLabel="Revoke token"
        withReason={false}
        onConfirm={async () => {
          if (revoking) await remove(revoking.kid);
        }}
      />
    </div>
  );
}
