'use client';

import { useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button, IconButton } from '@/components/ui/button';
import { Switch } from '@/components/ui/choice';
import { Combobox, Field, Input, MultiSelect, RadioGroup, Select, Textarea } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { CopyButton } from '@/components/ui/misc';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { EmptyState, InlineError } from '@/components/ui/states';
import { API_BASE } from '@/lib/api/client';
import type { FieldErrors } from '@/lib/validation';
import { useEdited } from '@/lib/hooks';
import { notify } from '@/stores/ui';

import { detailErrors, mergePatch } from '../form-errors';
import {
  type Binding,
  DEFAULT_SCOPES,
  type Mapping,
  PROTOCOL_LABEL,
  type Protocol,
  providerErrors,
  providerForm,
  type ProviderForm,
  type ProviderRow,
  providerSpec,
  scopeWords,
} from '../sso';
import { useDirectory, useOrgSettings } from '../store';
import { SamlPanel } from './saml-panel';

const MATCH_LABEL: Record<Protocol, { match: string; value: string; placeholder: [string, string] }> = {
  oidc: { match: 'Claim', value: 'Value', placeholder: ['groups', 'platform-admins'] },
  saml: { match: 'Attribute', value: 'Value', placeholder: ['memberOf', 'platform-admins'] },
  oauth: { match: 'GitHub organization', value: 'Team (optional)', placeholder: ['acme', 'platform'] },
};

function useRoleOptions() {
  const roles = useDirectory((s) => s.roles.data);
  const scopes = useDirectory((s) => s.scopes.data);
  const load = useDirectory((s) => s.loadRoles);
  useEffect(() => {
    void load();
  }, [load]);
  return {
    roles: (roles ?? []).map((r) => ({
      value: r.name,
      label: r.name,
      description: r.description ?? undefined,
    })),
    scopes: (scopes ?? ['gateway']).map((s) => ({ value: s, label: scopeWords(s), description: s })),
  };
}

/** Role mappings as rows: when a claim, attribute or GitHub team matches, the person gets a role. */
function MappingsEditor({
  protocol,
  value,
  onChange,
  errors,
  disabled,
}: {
  protocol: Protocol;
  value: Mapping[];
  onChange: (v: Mapping[]) => void;
  errors: FieldErrors;
  disabled: boolean;
}) {
  const { roles, scopes } = useRoleOptions();
  const labels = MATCH_LABEL[protocol];
  const set = (i: number, patch: Partial<Mapping>) =>
    onChange(value.map((m, j) => (j === i ? { ...m, ...patch } : m)));
  return (
    <fieldset className="flex flex-col gap-2">
      <legend className="mb-1 text-ui font-semibold text-fg">Role mappings</legend>
      <p className="text-caption text-muted">
        {protocol === 'oauth'
          ? 'Members of a GitHub organization, or of one of its teams, get the role where you choose.'
          : `Everyone whose ${labels.match.toLowerCase()} has the value gets the role where you choose. Every matching row applies.`}
      </p>
      {value.length === 0 ? (
        <EmptyState size="sm" title="No mappings" description="People get only the default roles below." />
      ) : null}
      {value.map((m, i) => (
        <div
          key={i}
          className="grid grid-cols-[1fr_1fr_1fr_1.2fr_auto] items-start gap-2 rounded-control border border-line p-2 max-md:grid-cols-1"
        >
          <Field label={labels.match} error={errors[`mappings.${i}.match`]}>
            <Input
              mono
              value={m.match}
              disabled={disabled}
              placeholder={labels.placeholder[0]}
              onChange={(e) => set(i, { match: e.target.value })}
            />
          </Field>
          <Field label={labels.value} error={errors[`mappings.${i}.value`]}>
            <Input
              mono
              value={m.value}
              disabled={disabled}
              placeholder={labels.placeholder[1]}
              onChange={(e) => set(i, { value: e.target.value })}
            />
          </Field>
          <Field label="Role" error={errors[`mappings.${i}.role`]}>
            <Select
              value={m.role}
              disabled={disabled}
              options={roles}
              onChange={(v) => set(i, { role: v })}
            />
          </Field>
          <Field label="Where" error={errors[`mappings.${i}.scope`]}>
            <Combobox
              value={m.scope}
              disabled={disabled}
              options={scopes}
              onChange={(v) => set(i, { scope: v })}
            />
          </Field>
          <IconButton
            icon="trash"
            label={`Remove mapping ${i + 1}`}
            className="mt-6 max-md:mt-0"
            disabled={disabled}
            onClick={() => onChange(value.filter((_, j) => j !== i))}
          />
        </div>
      ))}
      <div>
        <Button
          size="sm"
          icon="plus"
          disabled={disabled}
          onClick={() => onChange([...value, { match: '', value: '', role: '', scope: 'gateway' }])}
        >
          Add mapping
        </Button>
      </div>
    </fieldset>
  );
}

/** The roles every new person gets, whatever their claims say. */
function DefaultsEditor({
  value,
  onChange,
  errors,
  disabled,
}: {
  value: Binding[];
  onChange: (v: Binding[]) => void;
  errors: FieldErrors;
  disabled: boolean;
}) {
  const { roles, scopes } = useRoleOptions();
  const set = (i: number, patch: Partial<Binding>) =>
    onChange(value.map((b, j) => (j === i ? { ...b, ...patch } : b)));
  return (
    <fieldset className="flex flex-col gap-2">
      <legend className="mb-1 text-ui font-semibold text-fg">Default roles for new people</legend>
      <p className="text-caption text-muted">
        Given to everyone this provider signs in, before the mappings.
      </p>
      {value.map((b, i) => (
        <div key={i} className="grid grid-cols-[1fr_1.4fr_auto] items-start gap-2 max-sm:grid-cols-1">
          <Field label="Role" error={errors[`defaults.${i}.role`]}>
            <Select
              value={b.role}
              disabled={disabled}
              options={roles}
              onChange={(v) => set(i, { role: v })}
            />
          </Field>
          <Field label="Where" error={errors[`defaults.${i}.scope`]}>
            <Combobox
              value={b.scope}
              disabled={disabled}
              options={scopes}
              onChange={(v) => set(i, { scope: v })}
            />
          </Field>
          <IconButton
            icon="trash"
            label={`Remove default role ${i + 1}`}
            className="mt-6 max-sm:mt-0"
            disabled={disabled}
            onClick={() => onChange(value.filter((_, j) => j !== i))}
          />
        </div>
      ))}
      <div>
        <Button
          size="sm"
          icon="plus"
          disabled={disabled}
          onClick={() => onChange([...value, { role: '', scope: 'gateway' }])}
        >
          Add default role
        </Button>
      </div>
    </fieldset>
  );
}

/** Server paths of a provider's members, onto the form's fields. */
function serverPaths(protocol: Protocol, name: string): Record<string, string> {
  const base = `auth.admin.sso.${protocol}.${name}`;
  const paths: Record<string, string> = {
    [`${base}.display_name`]: 'displayName',
    [`${base}.issuer`]: 'issuer',
    [`${base}.client_id`]: 'clientId',
    [`${base}.client_secret`]: 'clientSecret',
    [`${base}.scopes`]: 'scopes',
    [`${base}.allowed_email_domains`]: 'domains',
    [`${base}.api_base_url`]: 'apiBase',
    [`${base}.allowed_organizations`]: 'orgs',
    [`${base}.idp.metadata_url`]: 'metadataUrl',
    [`${base}.idp.metadata_refresh`]: 'refresh',
    [`${base}.idp.metadata`]: 'metadata',
    [`${base}.idp.entity_id`]: 'entityId',
    [`${base}.idp.sso_url`]: 'ssoUrl',
    [`${base}.idp.certificates`]: 'certificates',
    [`${base}.idp`]: 'metadataUrl',
    [`${base}.sp_entity_id`]: 'spEntityId',
    [`${base}.email_attribute`]: 'emailAttribute',
    [base]: 'name',
  };
  for (let i = 0; i < 50; i++) {
    paths[`${base}.role_mappings[${i}]`] = `mappings.${i}.role`;
    paths[`${base}.default_role_bindings[${i}]`] = `defaults.${i}.role`;
  }
  return paths;
}

/** What to give the identity provider before the SAML provider exists: the addresses it will have. */
function SamlPreview({ name, spEntityId }: { name: string; spEntityId: string }) {
  const origin = useOrgSettings((s) => s.auth.data?.spec.admin?.public_origin) ?? window.location.origin;
  const id = name || 'name';
  const base = `${origin.replace(/\/$/, '')}${API_BASE}/auth/saml/${id}`;
  const items = [
    { label: 'Entity ID', value: spEntityId.trim() || `${base}/metadata` },
    { label: 'Reply (ACS) URL', value: `${base}/acs` },
  ];
  return (
    <div className="flex flex-col gap-2 rounded-control border border-line bg-sunken px-3.5 py-3">
      <p className="text-ui font-medium text-fg">Give your identity provider these</p>
      {items.map((i) => (
        <div key={i.label} className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="w-32 shrink-0 text-caption text-muted">{i.label}</span>
          <span className="mono min-w-0 flex-1 text-caption break-all text-fg">{i.value}</span>
          <CopyButton value={i.value} label={`Copy the ${i.label}`} />
        </div>
      ))}
      <p className="text-caption text-muted">
        After you add the provider, its metadata file is ready to download here.
      </p>
    </div>
  );
}

/**
 * Adds or edits a single sign-on provider: OIDC, SAML or GitHub, with its connection, who it lets
 * in, role mappings and the default roles of new people. A SAML provider also shows what the
 * identity provider needs and the metadata it has.
 */
export function ProviderDrawer({
  open,
  row,
  taken,
  onClose,
}: {
  open: boolean;
  /** The provider to edit, or the protocol of a new one. */
  row: ProviderRow | { protocol: Protocol; name: null };
  taken: string[];
  onClose: () => void;
}) {
  const saveAuth = useOrgSettings((s) => s.saveAuth);
  const canWrite = useCan('patchAuthSettings');
  const existing = row.name !== null ? row : null;
  const [form, setForm] = useState<ProviderForm>(() => providerForm(row.protocol, row.name, existing?.spec));
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(form);
  const set = <K extends keyof ProviderForm>(key: K, value: ProviderForm[K]) => {
    setForm((f) => ({ ...f, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _, ...rest }) => rest);
  };
  const p = form.protocol;
  const disabled = !canWrite;

  async function submit() {
    const found = providerErrors(form, !existing, taken);
    setErrors(found);
    if (Object.keys(found).length) return;
    setBusy(true);
    setError(null);
    try {
      const spec = providerSpec(form);
      const body = existing ? mergePatch(existing.spec, spec, ['client_secret']) : spec;
      await saveAuth({ admin: { sso: { [p]: { [form.name]: body } } } });
      notify('success', existing ? 'Provider saved' : 'Provider added', form.displayName || form.name);
      onClose();
    } catch (e) {
      setError(e);
      setErrors(detailErrors(e, serverPaths(p, form.name)));
    } finally {
      setBusy(false);
    }
  }

  const secretHint = existing
    ? 'Leave blank to keep the current secret.'
    : 'Stored encrypted and never shown again. A reference such as ${env:NAME} also works.';

  return (
    <Drawer
      dirty={edited}
      open={open}
      onClose={onClose}
      width="lg"
      title={existing ? existing.displayName : `Add ${PROTOCOL_LABEL[p]} provider`}
      subtitle={
        existing ? (
          <span className="flex items-center gap-2">
            <Badge>{PROTOCOL_LABEL[p]}</Badge>
            <span className="mono">{existing.name}</span>
          </span>
        ) : p === 'oauth' ? (
          'People sign in with their GitHub account; organization and team membership grant roles.'
        ) : p === 'saml' ? (
          'People sign in through your SAML 2.0 identity provider.'
        ) : (
          'People sign in through your OpenID Connect identity provider.'
        )
      }
      footer={
        <>
          <CancelButton />
          <Button
            variant="primary"
            busy={busy}
            disabled={!canWrite}
            disabledReason={canWrite ? undefined : "You don't have permission to change sign-in."}
            onClick={() => void submit()}
          >
            {existing ? 'Save changes' : 'Add provider'}
          </Button>
        </>
      }
    >
      <form
        noValidate
        className="flex flex-col gap-5"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <InlineError error={error} />
        {existing && p === 'saml' ? <SamlPanel name={existing.name} /> : null}
        <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
          {existing ? null : (
            <Field
              label="Name"
              required
              error={errors.name}
              hint="Used in its sign-in address. Can't be changed."
            >
              <Input
                mono
                data-autofocus
                value={form.name}
                placeholder={p === 'oauth' ? 'github' : p === 'saml' ? 'okta-saml' : 'okta'}
                onChange={(e) => set('name', e.target.value)}
              />
            </Field>
          )}
          <Field
            label="Button label"
            optional
            error={errors.displayName}
            hint="Shown on the sign-in page: “Sign in with …”."
          >
            <Input
              value={form.displayName}
              disabled={disabled}
              placeholder={p === 'oauth' ? 'GitHub' : 'Okta'}
              onChange={(e) => set('displayName', e.target.value)}
            />
          </Field>
        </div>

        {p === 'oidc' ? (
          <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
            <Field label="Issuer" required error={errors.issuer} className="sm:col-span-2">
              <Input
                mono
                value={form.issuer}
                disabled={disabled}
                placeholder="https://login.example.com"
                onChange={(e) => set('issuer', e.target.value)}
              />
            </Field>
            <Field label="Client ID" required error={errors.clientId}>
              <Input
                mono
                value={form.clientId}
                disabled={disabled}
                onChange={(e) => set('clientId', e.target.value)}
              />
            </Field>
            <Field label="Client secret" optional error={errors.clientSecret} hint={secretHint}>
              <Input
                mono
                type="password"
                autoComplete="new-password"
                value={form.clientSecret}
                disabled={disabled}
                placeholder={existing ? '••••••••' : ''}
                onChange={(e) => set('clientSecret', e.target.value)}
              />
            </Field>
            <Field
              label="Scopes"
              error={errors.scopes}
              hint="What the console asks the provider for."
              className="sm:col-span-2"
            >
              <MultiSelect
                creatable
                noun="scope"
                value={form.scopes}
                disabled={disabled}
                options={[...new Set([...DEFAULT_SCOPES, ...form.scopes])].map((s) => ({
                  value: s,
                  label: s,
                }))}
                onChange={(v) => set('scopes', v)}
              />
            </Field>
          </div>
        ) : null}

        {p === 'oauth' ? (
          <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
            <Field label="Client ID" required error={errors.clientId}>
              <Input
                mono
                value={form.clientId}
                disabled={disabled}
                onChange={(e) => set('clientId', e.target.value)}
              />
            </Field>
            <Field label="Client secret" required={!existing} error={errors.clientSecret} hint={secretHint}>
              <Input
                mono
                type="password"
                autoComplete="new-password"
                value={form.clientSecret}
                disabled={disabled}
                placeholder={existing ? '••••••••' : ''}
                onChange={(e) => set('clientSecret', e.target.value)}
              />
            </Field>
            <Field
              label="Allowed GitHub organizations"
              error={errors.orgs}
              hint="Only their members can sign in. Leave empty to let anyone with a mapping in."
            >
              <MultiSelect
                creatable
                noun="organization"
                value={form.orgs}
                disabled={disabled}
                options={form.orgs.map((o) => ({ value: o, label: o }))}
                onChange={(v) => set('orgs', v)}
              />
            </Field>
            <Field
              label="GitHub API address"
              optional
              error={errors.apiBase}
              hint="For GitHub Enterprise Server; leave blank for github.com."
            >
              <Input
                mono
                value={form.apiBase}
                disabled={disabled}
                placeholder="https://api.github.com"
                onChange={(e) => set('apiBase', e.target.value)}
              />
            </Field>
          </div>
        ) : null}

        {p === 'saml' ? (
          <div className="flex flex-col gap-4">
            {existing ? null : <SamlPreview name={form.name} spEntityId={form.spEntityId} />}
            <div className="flex flex-col gap-1.5">
              <span className="text-ui font-medium text-fg">Identity provider details</span>
              <RadioGroup
                label="Identity provider details"
                variant="cards"
                value={form.idpSource}
                onChange={(v) => set('idpSource', v)}
                options={[
                  {
                    value: 'metadata_url' as const,
                    label: 'Metadata address',
                    description:
                      'Fetched now and refreshed on a schedule, so new certificates are picked up.',
                    disabled,
                  },
                  {
                    value: 'metadata' as const,
                    label: 'Metadata file',
                    description: 'Paste the XML your provider gives.',
                    disabled,
                  },
                  {
                    value: 'fields' as const,
                    label: 'Enter by hand',
                    description: 'Entity ID, sign-in address and signing certificates.',
                    disabled,
                  },
                ]}
              />
            </div>
            {form.idpSource === 'metadata_url' ? (
              <div className="grid grid-cols-[1fr_10rem] gap-4 max-sm:grid-cols-1">
                <Field label="Metadata address" required error={errors.metadataUrl}>
                  <Input
                    mono
                    value={form.metadataUrl}
                    disabled={disabled}
                    placeholder="https://idp.example.com/app/123/sso/saml/metadata"
                    onChange={(e) => set('metadataUrl', e.target.value)}
                  />
                </Field>
                <Field label="Refresh every" error={errors.refresh}>
                  <Select
                    value={form.refresh}
                    disabled={disabled}
                    onChange={(v) => set('refresh', v)}
                    options={[
                      '1h',
                      '6h',
                      '24h',
                      '7d',
                      ...(['1h', '6h', '24h', '7d'].includes(form.refresh) ? [] : [form.refresh]),
                    ].map((v) => ({ value: v, label: v.replace('h', ' h').replace('d', ' days') }))}
                  />
                </Field>
              </div>
            ) : null}
            {form.idpSource === 'metadata' ? (
              <Field label="Metadata XML" required error={errors.metadata}>
                <Textarea
                  mono
                  rows={6}
                  value={form.metadata}
                  disabled={disabled}
                  placeholder="<EntityDescriptor …>"
                  onChange={(e) => set('metadata', e.target.value)}
                />
              </Field>
            ) : null}
            {form.idpSource === 'fields' ? (
              <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
                <Field label="Entity ID" required error={errors.entityId}>
                  <Input
                    mono
                    value={form.entityId}
                    disabled={disabled}
                    onChange={(e) => set('entityId', e.target.value)}
                  />
                </Field>
                <Field
                  label="Sign-in address"
                  required
                  error={errors.ssoUrl}
                  hint="Its HTTP-Redirect binding."
                >
                  <Input
                    mono
                    value={form.ssoUrl}
                    disabled={disabled}
                    onChange={(e) => set('ssoUrl', e.target.value)}
                  />
                </Field>
                <Field
                  label="Signing certificates"
                  required
                  error={errors.certificates}
                  hint="PEM, one or more."
                  className="sm:col-span-2"
                >
                  <Textarea
                    mono
                    rows={5}
                    value={form.certificates}
                    disabled={disabled}
                    placeholder="-----BEGIN CERTIFICATE-----"
                    onChange={(e) => set('certificates', e.target.value)}
                  />
                </Field>
              </div>
            ) : null}
            <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
              <Field label="Name ID format" hint="What the sign-in request asks the identity provider for.">
                <Select
                  value={form.nameId}
                  disabled={disabled}
                  onChange={(v) => set('nameId', v as ProviderForm['nameId'])}
                  options={[
                    { value: 'email', label: 'Email address' },
                    { value: 'persistent', label: 'Persistent' },
                    { value: 'transient', label: 'Transient' },
                    { value: 'unspecified', label: 'Unspecified' },
                  ]}
                />
              </Field>
              <Field
                label="Email attribute"
                optional
                error={errors.emailAttribute}
                hint="Leave blank when the name ID is the email address."
              >
                <Input
                  mono
                  value={form.emailAttribute}
                  disabled={disabled}
                  onChange={(e) => set('emailAttribute', e.target.value)}
                />
              </Field>
              <Field
                label="Entity ID of the gateway"
                optional
                error={errors.spEntityId}
                hint="Leave blank to use its metadata address."
                className="sm:col-span-2"
              >
                <Input
                  mono
                  value={form.spEntityId}
                  disabled={disabled}
                  onChange={(e) => set('spEntityId', e.target.value)}
                />
              </Field>
            </div>
            <div className="flex flex-col gap-1">
              <Switch
                checked={form.idpInitiated}
                disabled={disabled}
                onChange={(v) => set('idpInitiated', v)}
                label="Allow sign-in started from the identity provider's portal"
              />
              <p className="text-caption text-muted">
                Off is safer: an unrequested answer could sign a browser in as someone it didn&apos;t choose.
              </p>
            </div>
          </div>
        ) : null}

        <fieldset className="flex flex-col gap-3">
          <legend className="mb-1 text-ui font-semibold text-fg">Who can sign in</legend>
          {p === 'oauth' ? null : (
            <Field
              label="Allowed email domains"
              error={errors.domains}
              hint="Leave empty to allow any address the provider vouches for."
            >
              <MultiSelect
                creatable
                noun="domain"
                placeholder="example.com"
                value={form.domains}
                disabled={disabled}
                options={form.domains.map((d) => ({ value: d, label: d }))}
                onChange={(v) =>
                  set(
                    'domains',
                    v.map((d) => d.trim().toLowerCase().replace(/^@/, '')),
                  )
                }
              />
            </Field>
          )}
          <div className="flex flex-col gap-1">
            <Switch
              checked={form.jit}
              disabled={disabled}
              onChange={(v) => set('jit', v)}
              label="Create an account at first sign-in"
            />
            <p className="text-caption text-muted">
              Off: only people already invited or added in Teams & users can sign in.
            </p>
          </div>
        </fieldset>

        <MappingsEditor
          protocol={p}
          value={form.mappings}
          onChange={(v) => set('mappings', v)}
          errors={errors}
          disabled={disabled}
        />
        {p === 'oauth' ? null : (
          <DefaultsEditor
            value={form.defaults}
            onChange={(v) => set('defaults', v)}
            errors={errors}
            disabled={disabled}
          />
        )}
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}
