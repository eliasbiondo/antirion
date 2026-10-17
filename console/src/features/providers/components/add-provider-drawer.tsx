'use client';

import { useEffect, useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Field, Input, Segmented, Select, Textarea } from '@/components/ui/form';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { ProviderMark } from '@/components/ui/marks';
import { providerTypeLabel } from '@/lib/providers';
import { useEdited } from '@/lib/hooks';
import { serverFieldErrors, validate, type FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import {
  AUTH_LABELS,
  type AuthMethod,
  EMPTY_PROVIDER,
  orderTypes,
  providerSchema,
  type ProviderValues,
  suggestName,
  toProviderSpec,
} from '../form';
import { useProviders } from '../store';

const SECRET_HINT = 'Stored encrypted and never shown again. A reference such as ${env:NAME} also works.';

/**
 * "Add provider": the provider's type, a name, where its API is and how the gateway signs in to
 * it. The credential is write-only; the gateway keeps it encrypted.
 */
export function AddProviderDrawer({
  open,
  onClose,
  onAdded,
  taken: takenExtra,
}: {
  open: boolean;
  onClose: () => void;
  onAdded?: (name: string) => void;
  /** Names already used, when the caller knows more than the providers store. */
  taken?: string[];
}) {
  const types = useProviders((s) => s.types);
  const loadTypes = useProviders((s) => s.loadTypes);
  const providers = useProviders((s) => s.providers.data);
  const create = useProviders((s) => s.create);
  const [values, setValues] = useState<ProviderValues>(EMPTY_PROVIDER);
  const [nameTouched, setNameTouched] = useState(false);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);

  useEffect(() => {
    if (open) void loadTypes();
  }, [open, loadTypes]);

  const taken = useMemo(
    () => [...new Set([...(providers ?? []).map((p) => p.name), ...(takenExtra ?? [])])],
    [providers, takenExtra],
  );
  const ordered = useMemo(() => orderTypes(types.data ?? []), [types.data]);
  const type = ordered.find((t) => t.type === values.type);
  const methods = (type?.auth_methods ?? ['api_key']).filter((m): m is AuthMethod => m in AUTH_LABELS);

  const set = <K extends keyof ProviderValues>(key: K, value: ProviderValues[K]) => {
    setValues((v) => ({ ...v, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _gone, ...rest }) => rest);
  };

  function pickType(next: string) {
    const t = ordered.find((x) => x.type === next);
    const first = (t?.auth_methods ?? ['api_key']).find((m): m is AuthMethod => m in AUTH_LABELS);
    setValues((v) => ({
      ...v,
      type: next,
      name: nameTouched ? v.name : next ? suggestName(next, taken) : '',
      method: first ?? 'api_key',
      baseUrl: '',
    }));
    setErrors({});
  }

  function close() {
    setValues(EMPTY_PROVIDER);
    setNameTouched(false);
    setErrors({});
    setError(null);
    onClose();
  }

  async function submit() {
    const result = validate(providerSchema(type, taken), values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    const name = result.data.name.trim();
    try {
      await create(name, toProviderSpec(values));
      notify('success', 'Provider added', `${name} is ready. Add the models you want to offer from it.`);
      onAdded?.(name);
      close();
    } catch (e) {
      setError(e);
      const fields = serverFieldErrors(e, `providers.${name}`);
      setErrors({
        ...(fields.base_url ? { baseUrl: fields.base_url } : {}),
        ...(fields.auth ? { apiKey: fields.auth } : {}),
      });
    } finally {
      setBusy(false);
    }
  }

  const defaultUrl = type?.default_base_url ?? null;

  return (
    <Drawer
      dirty={edited}
      open={open}
      onClose={close}
      title="Add provider"
      subtitle="Connect an AI provider so you can offer its models to your apps."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            Add provider
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
        <Field label="Provider" error={errors.type} required>
          <Select
            value={values.type}
            onChange={(v) => pickType(v)}
            placeholder={types.status === 'loading' ? 'Loading providers…' : 'Select a provider'}
            options={ordered.map((t) => ({
              value: t.type,
              label: providerTypeLabel(t.type),
              icon: <ProviderMark type={t.type} size="md" />,
            }))}
            data-autofocus
          />
        </Field>
        {values.type ? (
          <>
            <Field
              label="Name"
              error={errors.name}
              required
              hint="Deployments and logs refer to the provider by this name. It can't be changed later."
            >
              <Input
                mono
                value={values.name}
                autoComplete="off"
                onChange={(e) => {
                  setNameTouched(true);
                  set('name', e.target.value);
                }}
              />
            </Field>
            {values.type === 'bedrock' ? (
              <Field label="AWS region" error={errors.region} required hint="e.g. us-east-1">
                <Input mono value={values.region} onChange={(e) => set('region', e.target.value)} />
              </Field>
            ) : null}
            {values.type === 'vertex' ? (
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Google Cloud project" error={errors.gcpProject} required>
                  <Input mono value={values.gcpProject} onChange={(e) => set('gcpProject', e.target.value)} />
                </Field>
                <Field label="Location" optional hint="Leave blank for global.">
                  <Input
                    mono
                    value={values.gcpLocation}
                    onChange={(e) => set('gcpLocation', e.target.value)}
                  />
                </Field>
              </div>
            ) : null}
            {methods.length > 1 ? (
              <Field label="Sign in with">
                <Segmented<AuthMethod>
                  label="Sign in with"
                  value={values.method}
                  onChange={(m) => set('method', m)}
                  options={methods.map((m) => ({ value: m, label: AUTH_LABELS[m] }))}
                />
              </Field>
            ) : null}
            {values.method === 'api_key' ? (
              <Field label="API key" error={errors.apiKey} required hint={SECRET_HINT}>
                <Input
                  mono
                  type="password"
                  autoComplete="off"
                  value={values.apiKey}
                  onChange={(e) => set('apiKey', e.target.value)}
                />
              </Field>
            ) : null}
            {values.method === 'aws_sigv4' ? (
              <div className="grid gap-4 sm:grid-cols-2">
                <Field
                  label="Access key ID"
                  optional
                  hint="Leave both blank to use the gateway’s own AWS credentials."
                >
                  <Input
                    mono
                    autoComplete="off"
                    value={values.accessKeyId}
                    onChange={(e) => set('accessKeyId', e.target.value)}
                  />
                </Field>
                <Field label="Secret access key" optional error={errors.secretAccessKey}>
                  <Input
                    mono
                    type="password"
                    autoComplete="off"
                    value={values.secretAccessKey}
                    onChange={(e) => set('secretAccessKey', e.target.value)}
                  />
                </Field>
              </div>
            ) : null}
            {values.method === 'gcp' ? (
              <Field
                label="Service account key (JSON)"
                optional
                hint="Leave blank to use the gateway’s own Google credentials. Stored encrypted."
              >
                <Textarea
                  mono
                  rows={4}
                  value={values.serviceAccountJson}
                  onChange={(e) => set('serviceAccountJson', e.target.value)}
                />
              </Field>
            ) : null}
            {values.method === 'azure_ad' ? (
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Tenant ID" optional>
                  <Input mono value={values.tenantId} onChange={(e) => set('tenantId', e.target.value)} />
                </Field>
                <Field label="Client ID" optional>
                  <Input mono value={values.clientId} onChange={(e) => set('clientId', e.target.value)} />
                </Field>
                <Field label="Client secret" optional hint={SECRET_HINT} className="sm:col-span-2">
                  <Input
                    mono
                    type="password"
                    autoComplete="off"
                    value={values.clientSecret}
                    onChange={(e) => set('clientSecret', e.target.value)}
                  />
                </Field>
              </div>
            ) : null}
            {values.method === 'none' ? (
              <p className="text-caption text-muted">
                The gateway calls this provider without credentials, as for a model server on your own
                network.
              </p>
            ) : null}
            <Field
              label="Base URL"
              optional={defaultUrl !== null}
              required={defaultUrl === null}
              error={errors.baseUrl}
              hint={
                defaultUrl !== null
                  ? `Leave blank to use ${defaultUrl}.`
                  : 'The address of the provider’s API, including any path such as /v1.'
              }
            >
              <Input
                mono
                inputMode="url"
                value={values.baseUrl}
                onChange={(e) => set('baseUrl', e.target.value)}
              />
            </Field>
          </>
        ) : (
          <p className="text-ui text-muted">
            Choose the company or server that runs the models. For a server with an OpenAI-style API (a local
            model server, or a provider not listed), choose OpenAI-compatible.
          </p>
        )}
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}
