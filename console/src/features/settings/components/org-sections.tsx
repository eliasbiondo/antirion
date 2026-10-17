'use client';

import { useEffect, useState } from 'react';
import { z } from 'zod';

import { useSettingsHref } from '@/components/layout/url-sync';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, FileButton, Input, Select } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { Gate, useCan } from '@/components/ui/gate';
import { CopyButton } from '@/components/ui/misc';
import { RevealOnceDialog } from '@/components/ui/reveal-once';
import { ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { formatBytes, formatCount } from '@/lib/format';
import { formatDate, formatRelative } from '@/lib/time';
import { serverFieldErrors, validate, type FieldErrors } from '@/lib/validation';
import { usePreferences } from '@/stores/preferences';
import { scopeOrganization, useScope } from '@/stores/scope';
import { useShell } from '@/stores/shell';
import { notify } from '@/stores/ui';

import { settingsApi } from '../api';
import { dataUrlBytes, LogoError, logoDataUrl } from '../logo';
import { useOrgSettings } from '../store';
import { Card, Facts, SaveBar, TechnicalDetails } from './parts';

function useSettingsLoad() {
  const loadSettings = useOrgSettings((s) => s.loadSettings);
  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);
}

const generalSchema = z.object({
  displayName: z.string().trim().max(200, 'Use at most 200 characters'),
  timezone: z
    .string()
    .regex(/^[A-Za-z_]+(\/[A-Za-z0-9_+-]+)*$/, 'Enter a time zone such as Europe/Berlin or UTC'),
  logo: z
    .string()
    .trim()
    .refine(
      (v) => v === '' || v.startsWith('https://') || v.startsWith('data:image/'),
      'Enter an address that starts with https://',
    ),
});

interface GeneralValues {
  displayName: string;
  timezone: string;
  logo: string;
}

/** The logo: an uploaded image (kept inline, at most 64 KB) or an https address, with a preview. */
function LogoField({
  value,
  canEdit,
  error,
  onChange,
}: {
  value: string;
  canEdit: boolean;
  error: string | undefined;
  onChange: (v: string) => void;
}) {
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const uploaded = value.startsWith('data:');
  return (
    <div className="flex flex-col gap-1.5 sm:col-span-2">
      <span className="text-ui font-medium text-fg">
        Logo <span className="font-normal text-dim">(optional)</span>
      </span>
      <div className="flex flex-wrap items-center gap-3">
        <span className="flex h-12 w-12 shrink-0 items-center justify-center overflow-hidden rounded-control border border-line bg-sunken">
          {value ? (
            // eslint-disable-next-line @next/next/no-img-element -- a small inline or remote image; nothing to optimise
            <img src={value} alt="Organization logo" className="h-full w-full object-contain" />
          ) : (
            <Icon name="building" size={18} className="text-dim" />
          )}
        </span>
        <FileButton
          size="sm"
          icon="plus"
          accept="image/png,image/jpeg,image/webp"
          disabled={!canEdit}
          busy={busy}
          onFile={(file) => {
            setBusy(true);
            setProblem(null);
            logoDataUrl(file)
              .then(onChange)
              .catch((e: unknown) =>
                setProblem(
                  e instanceof LogoError ? e.message : "That image couldn't be used. Try another one.",
                ),
              )
              .finally(() => setBusy(false));
          }}
        >
          {value ? 'Replace image' : 'Upload image'}
        </FileButton>
        {value ? (
          <Button size="sm" variant="ghost" disabled={!canEdit} onClick={() => onChange('')}>
            Remove logo
          </Button>
        ) : null}
        <span className="text-caption text-muted">
          {uploaded
            ? `Uploaded image, ${formatBytes(dataUrlBytes(value))}`
            : 'PNG, JPEG or WebP. Larger images are shrunk to fit 64 KB.'}
        </span>
      </div>
      {uploaded ? null : (
        <Input
          mono
          aria-label="Logo URL"
          value={value}
          disabled={!canEdit}
          placeholder="Or an image address: https://"
          onChange={(e) => onChange(e.target.value)}
          aria-invalid={error ? true : undefined}
        />
      )}
      {problem || error ? (
        <p role="alert" className="flex items-start gap-1.5 text-caption text-error">
          <Icon name="alertCircle" size={14} className="mt-px shrink-0" />
          {problem ?? error}
        </p>
      ) : null}
    </div>
  );
}

function GeneralForm({ org, initial }: { org: string; initial: GeneralValues }) {
  const save = useOrgSettings((s) => s.saveOrganization);
  const canEdit = useCan('patchOrganization');
  const [values, setValues] = useState(initial);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const dirty = JSON.stringify(values) !== JSON.stringify(initial);
  const set = (key: keyof GeneralValues, value: string) => {
    setValues({ ...values, [key]: value });
    if (errors[key]) setErrors(({ [key]: _, ...rest }) => rest);
  };
  async function submit() {
    const result = validate(generalSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      // A removed logo is sent as null, which a merge patch reads as "remove".
      await save(org, {
        display_name: values.displayName || undefined,
        timezone: values.timezone,
        logo: (values.logo.trim() || null) as string | undefined,
      });
      notify('success', 'Organization saved', org);
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `organizations.${org}`));
    } finally {
      setBusy(false);
    }
  }
  return (
    <form
      noValidate
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <InlineError error={error} />
      <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
        <Field label="Display name" error={errors.displayName ?? errors.display_name}>
          <Input
            value={values.displayName}
            disabled={!canEdit}
            onChange={(e) => set('displayName', e.target.value)}
          />
        </Field>
        <Field
          label="Default time zone"
          error={errors.timezone}
          hint="People can choose their own in Preferences."
        >
          <Input
            mono
            value={values.timezone}
            disabled={!canEdit}
            onChange={(e) => set('timezone', e.target.value)}
          />
        </Field>
        <LogoField
          value={values.logo}
          canEdit={canEdit}
          error={errors.logo}
          onChange={(v) => set('logo', v)}
        />
      </div>
      <SaveBar dirty={dirty} busy={busy} canEdit={canEdit} onDiscard={() => setValues(initial)} />
    </form>
  );
}

/** The scope's organization: display name, ID, default time zone and logo, with its facts. */
export function GeneralSection() {
  const scope = useScope((s) => s.scope);
  const org = scopeOrganization(scope) ?? 'default';
  const organization = useOrgSettings((s) => s.organization);
  const load = useOrgSettings((s) => s.loadOrganization);
  const timeZone = usePreferences((s) => s.timeZone);
  useEffect(() => {
    void load(org);
  }, [org, load]);
  const data = organization.data;
  const meta = data?.metadata;
  return (
    <>
      <Card
        title={data?.spec.display_name ?? org}
        description={scope.kind === 'gateway' ? 'Switch organization at the top to edit another.' : undefined}
      >
        {organization.status === 'loading' && !data ? <LoadingRows rows={3} /> : null}
        {organization.error ? <ErrorState error={organization.error} subject="The organization" /> : null}
        {data ? (
          <GeneralForm
            key={data.metadata.etag}
            org={data.name}
            initial={{
              displayName: data.spec.display_name ?? '',
              timezone: data.spec.timezone ?? 'UTC',
              logo: data.spec.logo ?? '',
            }}
          />
        ) : null}
      </Card>
      {data && meta ? (
        <Card title="About this organization">
          <Facts
            items={[
              {
                label: 'Identifier',
                value: (
                  <span className="flex items-center gap-1">
                    <span className="mono">{data.name}</span>
                    <CopyButton value={data.name} label="Copy the identifier" />
                  </span>
                ),
                hint: "Scripts and the API name the organization by it. It can't be changed.",
              },
              {
                label: 'Created',
                value: meta.created_at ? (
                  formatDate(meta.created_at, timeZone)
                ) : (
                  <span className="text-muted">Not recorded</span>
                ),
                hint: meta.created_at ? undefined : 'It was set up before the gateway kept creation dates.',
              },
              {
                label: 'Last changed',
                value: meta.updated_at ? (
                  `${formatDate(meta.updated_at, timeZone)} · ${formatRelative(meta.updated_at)}`
                ) : (
                  <span className="text-muted">Not recorded</span>
                ),
              },
              { label: 'Configuration version', value: <span className="mono">{meta.generation}</span> },
              {
                label: 'Managed',
                value:
                  meta.managed_by === 'api'
                    ? 'In the console and API'
                    : 'By a configuration file; edits here last until the file is applied again',
              },
            ]}
          />
        </Card>
      ) : null}
    </>
  );
}

/** SCIM status and its token, generated or replaced with an overlap and shown once. */
export function ProvisioningSection() {
  const [status, setStatus] = useState<Awaited<ReturnType<typeof settingsApi.scim>> | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [token, setToken] = useState<string | null>(null);
  const [confirm, setConfirm] = useState(false);
  const [overlap, setOverlap] = useState('24h');
  useEffect(() => {
    settingsApi.scim().then(setStatus, setError);
  }, []);
  const base = typeof window === 'undefined' ? '' : `${window.location.origin}/scim/v2`;
  const hasToken = Boolean(status?.enabled);
  return (
    <Card
      title="User provisioning (SCIM)"
      description="Sync users and groups from your identity provider automatically."
      actions={
        <Gate operation="rotateScimToken" mode="hide">
          <Button size="sm" onClick={() => setConfirm(true)}>
            {hasToken ? 'Replace token' : 'Generate token'}
          </Button>
        </Gate>
      }
    >
      <div className="flex flex-col gap-4">
        {error ? <ErrorState error={error} subject="User provisioning" size="sm" /> : null}
        <Facts
          items={[
            {
              label: 'SCIM URL',
              value: (
                <span className="flex items-center gap-1">
                  <span className="mono break-all">{base}</span>
                  <CopyButton value={base} label="Copy the SCIM URL" />
                </span>
              ),
            },
            ...(status
              ? [
                  { label: 'Status', value: status.enabled ? 'On' : 'Off' },
                  { label: 'Synced users', value: formatCount(status.users) },
                  { label: 'Synced groups', value: formatCount(status.groups) },
                  {
                    label: 'Last sync',
                    value: status.last_request_at ? formatRelative(status.last_request_at) : 'Never',
                  },
                ]
              : []),
          ]}
        />
      </div>
      <ConfirmDialog
        open={confirm}
        onClose={() => setConfirm(false)}
        title={hasToken ? 'Replace the SCIM token?' : 'Generate a SCIM token?'}
        tone="primary"
        confirmLabel={hasToken ? 'Replace token' : 'Generate token'}
        effect={
          hasToken
            ? "You'll see the new token once. The old one keeps working for the time you choose, then stops."
            : "You'll see the token once. Paste it into your identity provider's SCIM settings."
        }
        onConfirm={async () => {
          const r = await settingsApi.rotateScimToken(overlap);
          setToken(
            (r as { token?: string; secret?: string }).token ?? (r as { secret?: string }).secret ?? null,
          );
        }}
      >
        {hasToken ? (
          <Field label="Old token keeps working for">
            <Select
              value={overlap}
              onChange={(v) => setOverlap(v)}
              options={[
                { value: '0s', label: 'Stop it now' },
                { value: '1h', label: '1 hour' },
                { value: '24h', label: '24 hours' },
                { value: '7d', label: '7 days' },
              ]}
            />
          </Field>
        ) : null}
      </ConfirmDialog>
      <RevealOnceDialog
        secret={token}
        onClose={() => setToken(null)}
        title="Your SCIM token"
        label="Token"
        description="Paste it into your identity provider's SCIM settings now."
      />
    </Card>
  );
}

type Security = {
  egress?: { operator?: { deny_private?: boolean }; proxy?: { url: string } | null };
  limits?: { max_body?: string };
  client_ip?: { trusted_proxies?: string[] };
};
type Limits = { enabled?: boolean; algorithm?: string; on_store_unavailable?: string };

/** Where apps connect, rate limiting, and TLS and network rules, in words (was API access and Encryption). */
export function ApiAccessSection() {
  useSettingsLoad();
  const limits = useOrgSettings((s) => s.limits);
  const security = useOrgSettings((s) => s.security);
  const dataPlane = useShell((s) => s.status?.data_plane_url ?? null);
  const configHref = useSettingsHref('configuration');
  const sec = (security.data?.spec ?? {}) as Security;
  const lim = (limits.data?.spec ?? {}) as Limits;
  const notSet = (hint: string) => ({ value: <span className="text-muted">Not set</span>, hint });
  return (
    <>
      <Card title="Gateway URL" description="Where your apps send requests.">
        <Facts
          items={[
            dataPlane
              ? {
                  label: 'Address',
                  value: (
                    <span className="flex items-center gap-1">
                      <span className="mono break-all">{dataPlane}</span>
                      <CopyButton value={dataPlane} label="Copy the gateway URL" />
                    </span>
                  ),
                }
              : {
                  label: 'Address',
                  ...notSet(
                    'Set the public URL in the configuration so code snippets show the right address.',
                  ),
                },
          ]}
        />
        {dataPlane ? null : (
          <a href={configHref} className="mt-3 inline-block text-ui text-accent hover:underline">
            Open the configuration
          </a>
        )}
      </Card>
      <Card title="Rate limiting" description="How the gateway counts and enforces limits.">
        {limits.error ? <ErrorState error={limits.error} subject="Rate limiting" size="sm" /> : null}
        {limits.data ? (
          <div className="flex flex-col gap-3">
            <Facts
              items={[
                { label: 'Limits', value: lim.enabled === false ? 'Off' : 'On' },
                {
                  label: 'Counting method',
                  value:
                    lim.algorithm === 'sliding_window'
                      ? 'Sliding window'
                      : lim.algorithm === 'gcra'
                        ? 'Smooth rate (GCRA)'
                        : 'Gateway default',
                },
                {
                  label: 'If the limit store is down',
                  value:
                    lim.on_store_unavailable === 'fail_closed'
                      ? 'Refuse requests'
                      : lim.on_store_unavailable === 'fail_open'
                        ? 'Let requests through'
                        : lim.on_store_unavailable === 'local_share'
                          ? 'Each node enforces its share'
                          : 'Gateway default',
                },
              ]}
            />
            <TechnicalDetails value={limits.data.spec} />
          </div>
        ) : null}
      </Card>
      <Card title="Network" description="Where the gateway may connect, and the largest request it takes.">
        {security.error ? <ErrorState error={security.error} subject="Security settings" size="sm" /> : null}
        {security.data ? (
          <div className="flex flex-col gap-3">
            <Facts
              items={[
                {
                  label: 'Outbound proxy',
                  value: sec.egress?.proxy?.url ? (
                    <span className="mono">{sec.egress.proxy.url}</span>
                  ) : (
                    'None'
                  ),
                },
                {
                  label: 'Calls to private networks',
                  value:
                    sec.egress?.operator?.deny_private === false
                      ? 'Allowed'
                      : sec.egress?.operator?.deny_private === true
                        ? 'Blocked'
                        : 'Gateway default',
                },
                ...(sec.limits?.max_body
                  ? [{ label: 'Largest request', value: <span className="mono">{sec.limits.max_body}</span> }]
                  : []),
              ]}
            />
            <TechnicalDetails value={security.data.spec} />
          </div>
        ) : null}
      </Card>
    </>
  );
}
