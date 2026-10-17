'use client';

import { useState } from 'react';

import { Button, IconButton } from '@/components/ui/button';
import { Checkbox, Switch } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Select, Textarea } from '@/components/ui/form';
import { permissionOf, useCan } from '@/components/ui/gate';
import { noPermissionReason } from '@/lib/permissions';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import type { FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { detailErrors, str, mergePatch, NAME_RE, URL_RE } from '../form-errors';
import { useOrgSettings } from '../store';
import { useEdited } from '@/lib/hooks';

export const REDACTED = '[REDACTED]';
const SECRET_HINT = 'Stored encrypted and never shown again. A reference such as ${env:NAME} also works.';

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj => (typeof v === 'object' && v !== null && !Array.isArray(v) ? (v as Obj) : {});

/* ---------------------------------------------------------------- Headers */

export interface HeaderRow {
  name: string;
  value: string;
  /** The stored value is hidden; a blank value keeps it. */
  hidden: boolean;
}

export function headerRows(headers: unknown): HeaderRow[] {
  return Object.entries(obj(headers)).map(([name, value]) => {
    const hidden = value === REDACTED;
    return { name, value: hidden ? '' : str(value), hidden };
  });
}

export function headerMap(rows: HeaderRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of rows) {
    if (!r.name.trim()) continue;
    out[r.name.trim()] = r.hidden && !r.value ? REDACTED : r.value;
  }
  return out;
}

export function HeadersEditor({
  value,
  onChange,
  disabled,
  hint,
}: {
  value: HeaderRow[];
  onChange: (v: HeaderRow[]) => void;
  disabled: boolean;
  hint?: string;
}) {
  const set = (i: number, patch: Partial<HeaderRow>) =>
    onChange(value.map((h, j) => (j === i ? { ...h, ...patch } : h)));
  return (
    <fieldset className="flex flex-col gap-2">
      <legend className="mb-1 text-ui font-medium text-fg">
        Headers <span className="font-normal text-dim">(optional)</span>
      </legend>
      <p className="text-caption text-muted">
        {hint ?? 'Sent with every request, such as an API key. Values are stored encrypted.'}
      </p>
      {value.map((h, i) => (
        <div key={i} className="grid grid-cols-[1fr_1.4fr_auto] items-center gap-2 max-sm:grid-cols-1">
          <Input
            mono
            aria-label={`Header ${i + 1} name`}
            value={h.name}
            disabled={disabled}
            placeholder="dd-api-key"
            onChange={(e) => set(i, { name: e.target.value })}
          />
          <Input
            mono
            type="password"
            autoComplete="off"
            aria-label={`Header ${i + 1} value`}
            value={h.value}
            disabled={disabled}
            placeholder={h.hidden ? 'Hidden; leave blank to keep' : 'value or ${env:NAME}'}
            onChange={(e) => set(i, { value: e.target.value })}
          />
          <IconButton
            icon="trash"
            label={`Remove header ${i + 1}`}
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
          onClick={() => onChange([...value, { name: '', value: '', hidden: false }])}
        >
          Add header
        </Button>
      </div>
    </fieldset>
  );
}

const PROTOCOLS = [
  { value: 'http/protobuf', label: 'HTTP (protobuf)' },
  { value: 'http/json', label: 'HTTP (JSON)' },
  { value: 'grpc', label: 'gRPC' },
];

function SaveFooter({
  busy,
  canWrite,
  label,
  onCancel,
  onSave,
  extra,
}: {
  busy: boolean;
  canWrite: boolean;
  label: string;
  onCancel: () => void;
  onSave: () => void;
  extra?: React.ReactNode;
}) {
  return (
    <>
      {extra ? <span className="mr-auto flex items-center gap-2">{extra}</span> : null}
      <CancelButton onClick={onCancel} />
      <Button
        variant="primary"
        busy={busy}
        disabled={!canWrite}
        disabledReason={canWrite ? undefined : noPermissionReason(permissionOf('patchObservabilitySettings'))}
        onClick={onSave}
      >
        {label}
      </Button>
    </>
  );
}

/* ---------------------------------------------------------------- OpenTelemetry */

interface OtlpForm {
  endpoint: string;
  protocol: string;
  headers: HeaderRow[];
  metrics: boolean;
  interval: string;
  traces: boolean;
  ratio: string;
  logs: boolean;
  logSink: string;
  logContent: boolean;
}

/**
 * One collector for metrics, traces and the request records as logs: the endpoint, protocol and
 * headers are written to each signal that is on.
 */
export function OtlpDrawer({ onClose }: { onClose: () => void }) {
  const observability = useOrgSettings((s) => s.observability.data?.spec);
  const save = useOrgSettings((s) => s.saveObservability);
  const canWrite = useCan('patchObservabilitySettings');
  const spec = obj(observability);
  const metrics = obj(obj(spec.metrics).otlp);
  const traces = obj(spec.traces);
  const exporter = obj(traces.exporter);
  const sinks = obj(obj(obj(spec.request_logs).sinks).otlp);
  const [logName, logSpec] = Object.entries(sinks)[0] ?? ['collector', undefined];
  const log = obj(logSpec);
  const start: OtlpForm = {
    endpoint: str(metrics.endpoint ?? exporter.endpoint ?? log.endpoint, ''),
    protocol: str(metrics.protocol ?? exporter.protocol ?? log.protocol, 'http/protobuf'),
    headers: headerRows(metrics.headers ?? exporter.headers ?? log.headers),
    metrics: metrics.enabled === true,
    interval: str(metrics.interval, '15s'),
    traces: traces.enabled === true,
    ratio: String(Math.round(Number(obj(traces.sampler).ratio ?? 1) * 100)),
    logs: logSpec !== undefined,
    logSink: logName,
    logContent: log.include_content === true,
  };
  const [form, setForm] = useState(start);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(form);
  const set = <K extends keyof OtlpForm>(k: K, v: OtlpForm[K]) => {
    setForm((f) => ({ ...f, [k]: v }));
    if (errors[k]) setErrors(({ [k]: _, ...rest }) => rest);
  };
  async function submit() {
    const e: FieldErrors = {};
    const on = form.metrics || form.traces || form.logs;
    if (on && !URL_RE.test(form.endpoint.trim()))
      e.endpoint = 'Enter the collector address, such as https://otel.example.com:4318';
    const ratio = Number(form.ratio);
    if (form.traces && !(ratio >= 0 && ratio <= 100)) e.ratio = 'Enter a share from 0 to 100';
    if (form.logs && !NAME_RE.test(form.logSink)) e.logSink = 'Use letters, digits, . _ and -';
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    const headers = headerMap(form.headers);
    const endpoint = form.endpoint.trim();
    try {
      const patch: Obj = {
        metrics: {
          otlp: form.metrics
            ? mergePatch(metrics, {
                ...metrics,
                enabled: true,
                endpoint,
                protocol: form.protocol,
                interval: form.interval,
                headers,
              })
            : { enabled: false },
        },
        traces: form.traces
          ? mergePatch(traces, {
              ...traces,
              enabled: true,
              exporter: { ...exporter, endpoint, protocol: form.protocol, headers },
              sampler: { ...obj(traces.sampler), ratio: ratio / 100 },
            })
          : { enabled: false },
        request_logs: {
          sinks: {
            otlp: form.logs
              ? {
                  [form.logSink]: mergePatch(log, {
                    ...log,
                    endpoint,
                    protocol: form.protocol,
                    headers,
                    include_content: form.logContent,
                  }),
                }
              : logSpec !== undefined
                ? { [logName]: null }
                : {},
          },
        },
      };
      await save(patch);
      notify('success', 'OpenTelemetry export saved');
      onClose();
    } catch (err) {
      setError(err);
      setErrors(
        detailErrors(err, {
          'observability.metrics.otlp.endpoint': 'endpoint',
          'observability.traces.exporter.endpoint': 'endpoint',
          [`observability.request_logs.sinks.otlp.${form.logSink}.endpoint`]: 'endpoint',
          'observability.traces.sampler': 'ratio',
        }),
      );
    } finally {
      setBusy(false);
    }
  }
  const disabled = !canWrite;
  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      width="lg"
      title="OpenTelemetry collector"
      subtitle="Export metrics, traces and request records to your collector or an observability service that takes OTLP."
      footer={
        <SaveFooter
          busy={busy}
          canWrite={canWrite}
          label="Save export"
          onCancel={onClose}
          onSave={() => void submit()}
        />
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
        <div className="grid grid-cols-[1fr_12rem] gap-4 max-sm:grid-cols-1">
          <Field label="Collector address" error={errors.endpoint}>
            <Input
              mono
              data-autofocus
              value={form.endpoint}
              disabled={disabled}
              placeholder="https://otel.example.com:4318"
              onChange={(e) => set('endpoint', e.target.value)}
            />
          </Field>
          <Field label="Protocol">
            <Select
              value={form.protocol}
              disabled={disabled}
              options={PROTOCOLS}
              onChange={(v) => set('protocol', v)}
            />
          </Field>
        </div>
        <HeadersEditor
          value={form.headers}
          disabled={disabled}
          onChange={(v) => set('headers', v)}
          hint="Sent with every export, such as your observability service's API key."
        />
        <fieldset className="flex flex-col gap-3">
          <legend className="mb-1 text-ui font-semibold text-fg">What to send</legend>
          <div className="flex flex-wrap items-end gap-4">
            <div className="min-w-[16rem] flex-1">
              <Switch
                checked={form.metrics}
                disabled={disabled}
                onChange={(v) => set('metrics', v)}
                label="Metrics"
              />
              <p className="text-caption text-muted">Request rates, latency, tokens, spend and errors.</p>
            </div>
            <Field label="Every" className="w-36">
              <Select
                value={form.interval}
                disabled={disabled || !form.metrics}
                onChange={(v) => set('interval', v)}
                options={[
                  '10s',
                  '15s',
                  '30s',
                  '60s',
                  ...(['10s', '15s', '30s', '60s'].includes(form.interval) ? [] : [form.interval]),
                ].map((v) => ({ value: v, label: v.replace('s', ' s') }))}
              />
            </Field>
          </div>
          <div className="flex flex-wrap items-end gap-4">
            <div className="min-w-[16rem] flex-1">
              <Switch
                checked={form.traces}
                disabled={disabled}
                onChange={(v) => set('traces', v)}
                label="Traces"
              />
              <p className="text-caption text-muted">A span per request with its attempts and stages.</p>
            </div>
            <Field label="Share of requests, %" error={errors.ratio} className="w-36">
              <Input
                mono
                inputMode="numeric"
                value={form.ratio}
                disabled={disabled || !form.traces}
                onChange={(e) => set('ratio', e.target.value)}
              />
            </Field>
          </div>
          <div className="flex flex-col gap-2">
            <Switch
              checked={form.logs}
              disabled={disabled}
              onChange={(v) => set('logs', v)}
              label="Request records as logs"
            />
            <p className="text-caption text-muted">
              Each request-log record as one log record, linked to its trace.
            </p>
            {form.logs ? (
              <Checkbox
                checked={form.logContent}
                disabled={disabled}
                onChange={(v) => set('logContent', v)}
                label="Include prompts and responses"
                description="Only when the request log keeps them; off sends the metadata only."
              />
            ) : null}
          </div>
        </fieldset>
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}

/* ---------------------------------------------------------------- Request-log sinks */

export type SinkKind = 'http' | 'langfuse';

interface SinkForm {
  name: string;
  url: string;
  format: string;
  compression: string;
  headers: HeaderRow[];
  content: boolean;
  publicKey: string;
  secretKey: string;
  environment: string;
}

/**
 * A copy of the request log sent to your own endpoint: a data warehouse's HTTP intake (batches of
 * JSON records) or a trace-ingestion service (one trace per request, a generation per attempt).
 */
export function SinkDrawer({
  kind,
  name,
  taken,
  onClose,
}: {
  kind: SinkKind;
  name: string | null;
  taken: string[];
  onClose: () => void;
}) {
  const observability = useOrgSettings((s) => s.observability.data?.spec);
  const save = useOrgSettings((s) => s.saveObservability);
  const canWrite = useCan('patchObservabilitySettings');
  const existing = name ? obj(obj(obj(obj(obj(observability).request_logs).sinks)[kind])[name]) : undefined;
  const [form, setForm] = useState<SinkForm>({
    name: name ?? '',
    url: str(existing?.url, ''),
    format: str(existing?.format, 'ndjson'),
    compression: str(existing?.compression, 'gzip'),
    headers: headerRows(existing?.headers),
    content: existing?.include_content === true,
    publicKey: str(existing?.public_key, ''),
    secretKey: '',
    environment: str(existing?.environment, ''),
  });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(form);
  const [removing, setRemoving] = useState(false);
  const set = <K extends keyof SinkForm>(k: K, v: SinkForm[K]) => {
    setForm((f) => ({ ...f, [k]: v }));
    if (errors[k]) setErrors(({ [k]: _, ...rest }) => rest);
  };
  async function submit() {
    const e: FieldErrors = {};
    if (!name) {
      if (!NAME_RE.test(form.name) || ['store', 'stdout'].includes(form.name))
        e.name = 'Use letters, digits, . _ and -; not "store" or "stdout"';
      else if (taken.includes(form.name)) e.name = 'A destination already has this name';
    }
    if (!/^https:\/\//i.test(form.url.trim())) e.url = 'Enter an https:// address';
    if (kind === 'langfuse') {
      if (!form.publicKey.trim()) e.publicKey = 'Enter the public key';
      if (!name && !form.secretKey) e.secretKey = 'Enter the secret key';
    }
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    const next: Obj =
      kind === 'http'
        ? {
            ...existing,
            url: form.url.trim(),
            format: form.format,
            compression: form.compression,
            headers: headerMap(form.headers),
            include_content: form.content,
          }
        : {
            ...existing,
            url: form.url.trim(),
            public_key: form.publicKey.trim(),
            ...(form.secretKey ? { secret_key: form.secretKey } : {}),
            environment: form.environment.trim() || null,
            include_content: form.content,
          };
    try {
      const body = existing ? mergePatch(existing, next, ['secret_key']) : next;
      await save({ request_logs: { sinks: { [kind]: { [form.name]: body } } } });
      notify('success', name ? 'Destination saved' : 'Destination added', form.name);
      onClose();
    } catch (err) {
      setError(err);
      const base = `observability.request_logs.sinks.${kind}.${form.name}`;
      setErrors(
        detailErrors(err, {
          [`${base}.url`]: 'url',
          [`${base}.public_key`]: 'publicKey',
          [`${base}.secret_key`]: 'secretKey',
          [`${base}.environment`]: 'environment',
          [base]: 'name',
        }),
      );
    } finally {
      setBusy(false);
    }
  }
  const disabled = !canWrite;
  const removeDialog = (
    <ConfirmDialog
      open={removing}
      onClose={() => setRemoving(false)}
      title={`Remove destination ${name ?? ''}?`}
      objectName={name ?? undefined}
      confirmLabel="Remove destination"
      effect="It stops receiving request records; records still waiting for it are dropped."
      onConfirm={async (reason) => {
        if (!name) return;
        await save({ request_logs: { sinks: { [kind]: { [name]: null } } } }, reason);
        notify('success', 'Destination removed', name);
        onClose();
      }}
    />
  );
  const title =
    kind === 'http'
      ? (name ?? 'Send the request log to a data warehouse')
      : (name ?? 'Send traces to a trace-ingestion service');
  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      width="lg"
      title={title}
      subtitle={
        kind === 'http'
          ? 'Records are posted in batches to an HTTPS intake, such as your warehouse’s streaming endpoint, and retried for 5 minutes.'
          : 'Each request becomes a trace and each attempt a generation, posted with basic authentication to a Langfuse-compatible ingestion API.'
      }
      footer={
        <SaveFooter
          busy={busy}
          canWrite={canWrite}
          label={name ? 'Save destination' : 'Add destination'}
          onCancel={onClose}
          onSave={() => void submit()}
          extra={
            name ? (
              <Button variant="danger" icon="trash" disabled={!canWrite} onClick={() => setRemoving(true)}>
                Remove
              </Button>
            ) : undefined
          }
        />
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
        <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
          {name ? null : (
            <Field label="Name" required error={errors.name}>
              <Input
                mono
                data-autofocus
                value={form.name}
                placeholder={kind === 'http' ? 'warehouse' : 'traces'}
                onChange={(e) => set('name', e.target.value)}
              />
            </Field>
          )}
          <Field
            label={kind === 'http' ? 'Intake address' : 'Service address'}
            required
            error={errors.url}
            className={name ? 'sm:col-span-2' : undefined}
          >
            <Input
              mono
              value={form.url}
              disabled={disabled}
              placeholder={
                kind === 'http' ? 'https://ingest.example.com/antirion' : 'https://cloud.langfuse.com'
              }
              onChange={(e) => set('url', e.target.value)}
            />
          </Field>
        </div>
        {kind === 'http' ? (
          <>
            <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
              <Field label="Format">
                <Select
                  value={form.format}
                  disabled={disabled}
                  onChange={(v) => set('format', v)}
                  options={[
                    { value: 'ndjson', label: 'One JSON record per line' },
                    { value: 'json_array', label: 'A JSON array per batch' },
                  ]}
                />
              </Field>
              <Field label="Compression">
                <Select
                  value={form.compression}
                  disabled={disabled}
                  onChange={(v) => set('compression', v)}
                  options={[
                    { value: 'gzip', label: 'gzip' },
                    { value: 'none', label: 'None' },
                  ]}
                />
              </Field>
            </div>
            <HeadersEditor value={form.headers} disabled={disabled} onChange={(v) => set('headers', v)} />
          </>
        ) : (
          <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
            <Field label="Public key" required error={errors.publicKey}>
              <Input
                mono
                value={form.publicKey}
                disabled={disabled}
                onChange={(e) => set('publicKey', e.target.value)}
              />
            </Field>
            <Field
              label="Secret key"
              required={!name}
              error={errors.secretKey}
              hint={name ? 'Leave blank to keep the current key.' : SECRET_HINT}
            >
              <Input
                mono
                type="password"
                autoComplete="new-password"
                value={form.secretKey}
                disabled={disabled}
                placeholder={name ? '••••••••' : ''}
                onChange={(e) => set('secretKey', e.target.value)}
              />
            </Field>
            <Field
              label="Environment"
              optional
              error={errors.environment}
              hint="Tags every trace, such as production."
            >
              <Input
                mono
                value={form.environment}
                disabled={disabled}
                onChange={(e) => set('environment', e.target.value)}
              />
            </Field>
          </div>
        )}
        <Checkbox
          checked={form.content}
          disabled={disabled}
          onChange={(v) => set('content', v)}
          label="Include prompts and responses"
          description="Only when the request log keeps them. Off sends the metadata only."
        />
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
      {removeDialog}
    </Drawer>
  );
}

/* ---------------------------------------------------------------- Secret stores */

export type StoreKind = 'vault' | 'aws_sm' | 'gcp_sm' | 'azure_kv';

export const STORE_LABEL: Record<StoreKind, string> = {
  vault: 'HashiCorp Vault',
  aws_sm: 'AWS Secrets Manager',
  gcp_sm: 'Google Cloud Secret Manager',
  azure_kv: 'Azure Key Vault',
};

/** How a configuration names a secret in each store. */
export const STORE_REFERENCE: Record<StoreKind, string> = {
  vault: '${vault:secret/data/openai#api_key}',
  aws_sm: '${aws-sm:prod/openai#api_key}',
  gcp_sm: '${gcp-sm:projects/acme/secrets/openai}',
  azure_kv: '${azure-kv:acme-vault/openai-key}',
};

interface StoreField {
  key: string;
  label: string;
  secret?: boolean;
  placeholder?: string;
  hint?: string;
  required?: boolean;
  multiline?: boolean;
}

const STORE_FIELDS: Record<StoreKind, StoreField[]> = {
  vault: [
    { key: 'address', label: 'Vault address', placeholder: 'https://vault.internal:8200', required: true },
    { key: 'namespace', label: 'Namespace', hint: 'For Vault Enterprise namespaces.' },
  ],
  aws_sm: [
    { key: 'region', label: 'Region', placeholder: 'us-east-1', hint: 'For names that are not full ARNs.' },
    {
      key: 'access_key_id',
      label: 'Access key ID',
      secret: true,
      hint: 'Leave both blank to use the instance or task role.',
    },
    { key: 'secret_access_key', label: 'Secret access key', secret: true },
    {
      key: 'assume_role.role_arn',
      label: 'Role to assume',
      placeholder: 'arn:aws:iam::123456789012:role/antirion',
    },
    { key: 'endpoint', label: 'Endpoint', hint: 'Only for a private endpoint or a test double.' },
  ],
  gcp_sm: [
    {
      key: 'service_account_json',
      label: 'Service account key',
      secret: true,
      multiline: true,
      hint: 'The JSON key, or a reference to it. Leave blank to use the workload’s own identity.',
    },
  ],
  azure_kv: [
    { key: 'tenant_id', label: 'Tenant ID' },
    { key: 'client_id', label: 'Client ID' },
    {
      key: 'client_secret',
      label: 'Client secret',
      secret: true,
      hint: 'Leave blank with a managed identity.',
    },
    {
      key: 'vault_suffix',
      label: 'Vault domain',
      placeholder: 'vault.azure.net',
      hint: 'Change it only for sovereign clouds.',
    },
  ],
};

const VAULT_METHODS = [
  { value: 'token', label: 'Token' },
  { value: 'approle', label: 'AppRole' },
  { value: 'kubernetes', label: 'Kubernetes service account' },
];

function getPath(o: Obj, path: string): unknown {
  return path.split('.').reduce<unknown>((v, k) => obj(v)[k], o);
}

function setPath(o: Obj, path: string, value: unknown): Obj {
  const [head, ...rest] = path.split('.');
  if (!head) return o;
  return { ...o, [head]: rest.length ? setPath(obj(o[head]), rest.join('.'), value) : value };
}

/**
 * An external secret store the configuration's `${store:…}` references read from: its address or
 * account, how the gateway signs in to it, and how long a value is reused.
 */
export function SecretStoreDrawer({ kind, onClose }: { kind: StoreKind; onClose: () => void }) {
  const security = useOrgSettings((s) => s.security.data?.spec);
  const save = useOrgSettings((s) => s.saveSecurity);
  const canWrite = useCan('patchSecuritySettings');
  const existing = obj(obj(obj(obj(security).secrets).stores)[kind]);
  const connected = Object.keys(existing).length > 0;
  const fields = STORE_FIELDS[kind];
  const auth = obj(existing.auth);
  const [values, setValues] = useState<Record<string, string>>(() => {
    const v: Record<string, string> = {};
    for (const f of fields) {
      const cur = getPath(existing, f.key);
      v[f.key] = f.secret ? '' : str(cur);
    }
    v['auth.method'] = str(auth.method, 'token');
    v['auth.role_id'] = str(auth.role_id, '');
    v['auth.role'] = str(auth.role, '');
    v['auth.mount'] = str(auth.mount, '');
    v['auth.token'] = '';
    v['auth.secret_id'] = '';
    v.ttl = str(existing.ttl, '5m');
    v.managed = existing.managed_identity === true ? 'true' : '';
    return v;
  });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  const set = (k: string, v: string) => {
    setValues((x) => ({ ...x, [k]: v }));
    if (errors[k]) setErrors(({ [k]: _, ...rest }) => rest);
  };
  async function submit() {
    const e: FieldErrors = {};
    for (const f of fields) {
      if (f.required && !values[f.key]?.trim()) e[f.key] = `Enter the ${f.label.toLowerCase()}`;
    }
    if (kind === 'vault') {
      if (values.address && !URL_RE.test(values.address.trim()))
        e.address = 'Enter an address such as https://vault.internal:8200';
      const m = values['auth.method'];
      if (m === 'token' && !connected && !values['auth.token']) e['auth.token'] = 'Enter a Vault token';
      if (m === 'approle' && !values['auth.role_id']?.trim()) e['auth.role_id'] = 'Enter the role ID';
      if (m === 'kubernetes' && !values['auth.role']?.trim()) e['auth.role'] = 'Enter the Vault role';
    }
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    let next: Obj = { ...existing };
    for (const f of fields) {
      const v = values[f.key]?.trim() ?? '';
      if (f.secret && !v) continue;
      next = setPath(next, f.key, v || undefined);
    }
    if (kind === 'vault') {
      const m = values['auth.method'] ?? 'token';
      const used: Record<string, string[]> = {
        token: ['token'],
        approle: ['role_id', 'secret_id'],
        kubernetes: ['role'],
      };
      const a: Obj = Object.fromEntries(
        Object.entries(obj(existing.auth)).filter(
          ([k]) =>
            !['token', 'role_id', 'secret_id', 'role', 'mount'].includes(k) || (used[m] ?? []).includes(k),
        ),
      );
      a.method = m;
      if (values['auth.mount']?.trim()) a.mount = values['auth.mount'].trim();
      for (const k of used[m] ?? []) {
        const typed = values[`auth.${k}`]?.trim() ?? '';
        if (typed) a[k] = typed;
      }
      next.auth = a;
    }
    if (kind === 'azure_kv') next.managed_identity = values.managed === 'true';
    next.ttl = values.ttl;
    // Drop members set to undefined so they are removed.
    next = JSON.parse(JSON.stringify(next)) as Obj;
    // Secrets left blank keep their stored value; a Vault method that no longer uses one drops it.
    const m = values['auth.method'];
    const keep = [
      ...fields.filter((f) => f.secret).map((f) => f.key),
      ...(m === 'token' ? ['auth.token'] : m === 'approle' ? ['auth.secret_id'] : []),
    ];
    try {
      const body = connected ? mergePatch(existing, next, keep) : next;
      await save({ secrets: { stores: { [kind]: body } } });
      notify('success', connected ? 'Secret store saved' : 'Secret store connected', STORE_LABEL[kind]);
      onClose();
    } catch (err) {
      setError(err);
      const base = `security.secrets.stores.${kind}`;
      setErrors(
        detailErrors(err, {
          ...Object.fromEntries(fields.map((f): [string, string] => [`${base}.${f.key}`, f.key])),
          [`${base}.auth`]: 'auth.token',
          [`${base}.ttl`]: 'ttl',
        }),
      );
    } finally {
      setBusy(false);
    }
  }
  const disabled = !canWrite;
  const [removing, setRemoving] = useState(false);
  const method = values['auth.method'];
  const secretHint = connected ? 'Leave blank to keep the stored value.' : SECRET_HINT;
  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      width="lg"
      title={STORE_LABEL[kind]}
      subtitle={
        <>
          Provider keys and other secrets in the configuration can name a value in it, such as{' '}
          <span className="mono text-fg">{STORE_REFERENCE[kind]}</span>.
        </>
      }
      footer={
        <>
          {connected ? (
            <span className="mr-auto">
              <Button variant="danger" icon="plugOff" disabled={!canWrite} onClick={() => setRemoving(true)}>
                Disconnect
              </Button>
            </span>
          ) : null}
          <CancelButton />
          <Button
            variant="primary"
            busy={busy}
            disabled={!canWrite}
            disabledReason={canWrite ? undefined : noPermissionReason(permissionOf('patchSecuritySettings'))}
            onClick={() => void submit()}
          >
            {connected ? 'Save store' : 'Connect store'}
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
        <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
          {fields.map((f) => (
            <Field
              key={f.key}
              label={f.label}
              required={f.required}
              optional={!f.required}
              error={errors[f.key]}
              hint={f.secret ? (f.hint ?? secretHint) : f.hint}
              className={f.multiline || fields.length === 1 ? 'sm:col-span-2' : undefined}
            >
              {f.multiline ? (
                <Textarea
                  mono
                  rows={4}
                  value={values[f.key] ?? ''}
                  disabled={disabled}
                  placeholder={
                    f.secret && connected ? 'Hidden; leave blank to keep' : '{ "type": "service_account", … }'
                  }
                  onChange={(e) => set(f.key, e.target.value)}
                />
              ) : (
                <Input
                  mono
                  type={f.secret ? 'password' : 'text'}
                  autoComplete={f.secret ? 'new-password' : 'off'}
                  value={values[f.key] ?? ''}
                  disabled={disabled}
                  placeholder={f.secret && connected ? '••••••••' : f.placeholder}
                  onChange={(e) => set(f.key, e.target.value)}
                />
              )}
            </Field>
          ))}
        </div>
        {kind === 'vault' ? (
          <fieldset className="flex flex-col gap-3">
            <legend className="mb-1 text-ui font-semibold text-fg">How the gateway signs in</legend>
            <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
              <Field label="Method">
                <Select
                  value={method ?? 'token'}
                  disabled={disabled}
                  options={VAULT_METHODS}
                  onChange={(v) => set('auth.method', v)}
                />
              </Field>
              <Field label="Mount path" optional hint="Leave blank for the method's usual path.">
                <Input
                  mono
                  value={values['auth.mount'] ?? ''}
                  disabled={disabled}
                  onChange={(e) => set('auth.mount', e.target.value)}
                />
              </Field>
              {method === 'token' ? (
                <Field label="Token" required={!connected} error={errors['auth.token']} hint={secretHint}>
                  <Input
                    mono
                    type="password"
                    autoComplete="new-password"
                    value={values['auth.token'] ?? ''}
                    disabled={disabled}
                    placeholder={connected ? '••••••••' : 'hvs.…'}
                    onChange={(e) => set('auth.token', e.target.value)}
                  />
                </Field>
              ) : null}
              {method === 'approle' ? (
                <>
                  <Field label="Role ID" required error={errors['auth.role_id']}>
                    <Input
                      mono
                      value={values['auth.role_id'] ?? ''}
                      disabled={disabled}
                      onChange={(e) => set('auth.role_id', e.target.value)}
                    />
                  </Field>
                  <Field label="Secret ID" optional hint={secretHint}>
                    <Input
                      mono
                      type="password"
                      autoComplete="new-password"
                      value={values['auth.secret_id'] ?? ''}
                      disabled={disabled}
                      onChange={(e) => set('auth.secret_id', e.target.value)}
                    />
                  </Field>
                </>
              ) : null}
              {method === 'kubernetes' ? (
                <Field
                  label="Vault role"
                  required
                  error={errors['auth.role']}
                  hint="The pod's service account token is read at each sign-in."
                >
                  <Input
                    mono
                    value={values['auth.role'] ?? ''}
                    disabled={disabled}
                    onChange={(e) => set('auth.role', e.target.value)}
                  />
                </Field>
              ) : null}
            </div>
          </fieldset>
        ) : null}
        {kind === 'azure_kv' ? (
          <Checkbox
            checked={values.managed === 'true'}
            disabled={disabled}
            onChange={(v) => set('managed', v ? 'true' : '')}
            label="Use the managed identity of the machine the gateway runs on"
          />
        ) : null}
        <Field
          label="Reuse a value for"
          error={errors.ttl}
          hint="Then it is read again, so a rotated secret is picked up."
          className="w-56"
        >
          <Select
            value={values.ttl ?? '5m'}
            disabled={disabled}
            onChange={(v) => set('ttl', v)}
            options={[
              '1m',
              '5m',
              '15m',
              '1h',
              ...(['1m', '5m', '15m', '1h'].includes(values.ttl ?? '') ? [] : [values.ttl ?? '5m']),
            ].map((v) => ({
              value: v,
              label: v.replace('m', ' min').replace('h', ' h'),
            }))}
          />
        </Field>
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
      <ConfirmDialog
        open={removing}
        onClose={() => setRemoving(false)}
        title={`Disconnect ${STORE_LABEL[kind]}?`}
        confirmLabel="Disconnect store"
        effect="The gateway refuses this while the configuration still names a value in it; replace those references first."
        onConfirm={async (reason) => {
          await save({ secrets: { stores: { [kind]: null } } }, reason);
          notify('success', 'Secret store disconnected', STORE_LABEL[kind]);
          onClose();
        }}
      />
    </Drawer>
  );
}
