'use client';

import { useEffect, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Select } from '@/components/ui/form';
import { permissionOf, useCan } from '@/components/ui/gate';
import { noPermissionReason } from '@/lib/permissions';
import { MenuButton } from '@/components/ui/menu';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { EmptyState, ErrorState, InlineError } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { describeError } from '@/lib/api/client';
import { formatCount } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import type { FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { settingsApi } from '../api';
import { detailErrors, mergePatch, NAME_RE } from '../form-errors';
import { useLinks, useRuntime } from '../store';
import { HeadersEditor, headerMap, headerRows } from './connection-drawers';
import { Card } from './parts';
import { useEdited } from '@/lib/hooks';

type Sink = {
  type?: 'http';
  enabled?: boolean;
  url: string;
  headers?: Record<string, string>;
  format?: 'ndjson' | 'json_array';
  batch_max_records?: number;
  flush_interval?: string;
  max_retry_duration?: string;
};

const RETRY = [
  { value: '1h', label: '1 hour' },
  { value: '24h', label: '1 day' },
  { value: '72h', label: '3 days' },
  { value: '7d', label: '7 days' },
  { value: '30d', label: '30 days' },
];

function retryWords(v: string | undefined): string {
  return RETRY.find((r) => r.value === (v ?? '72h'))?.label ?? v ?? '3 days';
}

function SinkEditor({
  name,
  sink,
  taken,
  onClose,
}: {
  name: string | null;
  sink: Sink | undefined;
  taken: string[];
  onClose: () => void;
}) {
  const save = useLinks((s) => s.saveGovernance);
  const canWrite = useCan('patchGovernanceSettings');
  const [form, setForm] = useState({
    name: name ?? '',
    url: sink?.url ?? '',
    format: sink?.format ?? 'ndjson',
    retry: sink?.max_retry_duration ?? '72h',
    enabled: sink?.enabled ?? true,
    headers: headerRows(sink?.headers),
  });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(form);
  const set = <K extends keyof typeof form>(k: K, v: (typeof form)[K]) => {
    setForm((f) => ({ ...f, [k]: v }));
    if (errors[k]) setErrors(({ [k]: _, ...rest }) => rest);
  };
  async function submit() {
    const e: FieldErrors = {};
    if (!name) {
      if (!NAME_RE.test(form.name)) e.name = 'Use letters, digits, . _ and -; start with a letter or digit';
      else if (taken.includes(form.name)) e.name = 'A destination already has this name';
    }
    if (!/^https:\/\//i.test(form.url.trim())) e.url = 'Enter an https:// address';
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    const next: Sink = {
      ...sink,
      type: 'http',
      url: form.url.trim(),
      format: form.format,
      max_retry_duration: form.retry,
      enabled: form.enabled,
      headers: headerMap(form.headers),
    };
    try {
      const body = sink ? mergePatch(sink, next) : next;
      await save({ audit_sinks: { [form.name]: body } });
      notify('success', name ? 'Destination saved' : 'Destination added', form.name);
      onClose();
    } catch (err) {
      setError(err);
      const base = `governance.audit_sinks.${form.name}`;
      setErrors(
        detailErrors(err, {
          [`${base}.url`]: 'url',
          [`${base}.max_retry_duration`]: 'retry',
          [base]: 'name',
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
      title={name ?? 'New audit log destination'}
      subtitle="Every audit entry is posted here in batches. While the destination is down, entries wait on the gateway and are sent when it is back."
      footer={
        <>
          <CancelButton />
          <Button
            variant="primary"
            busy={busy}
            disabled={!canWrite}
            disabledReason={
              canWrite ? undefined : noPermissionReason(permissionOf('patchGovernanceSettings'))
            }
            onClick={() => void submit()}
          >
            {name ? 'Save destination' : 'Add destination'}
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
          {name ? null : (
            <Field label="Name" required error={errors.name}>
              <Input
                mono
                data-autofocus
                value={form.name}
                placeholder="siem"
                onChange={(e) => set('name', e.target.value)}
              />
            </Field>
          )}
          <Field label="Address" required error={errors.url} className={name ? 'sm:col-span-2' : undefined}>
            <Input
              mono
              value={form.url}
              disabled={disabled}
              placeholder="https://siem.example.com/intake"
              onChange={(e) => set('url', e.target.value)}
            />
          </Field>
          <Field label="Format">
            <Select
              value={form.format}
              disabled={disabled}
              onChange={(v) => set('format', v as Sink['format'] & string)}
              options={[
                { value: 'ndjson', label: 'One JSON entry per line' },
                { value: 'json_array', label: 'A JSON array per batch' },
              ]}
            />
          </Field>
          <Field
            label="Keep trying for"
            error={errors.retry}
            hint="Entries older than this are dropped and counted."
          >
            <Select
              value={form.retry}
              disabled={disabled}
              onChange={(v) => set('retry', v)}
              options={
                RETRY.some((r) => r.value === form.retry)
                  ? RETRY
                  : [...RETRY, { value: form.retry, label: form.retry }]
              }
            />
          </Field>
        </div>
        <HeadersEditor value={form.headers} disabled={disabled} onChange={(v) => set('headers', v)} />
        <Switch
          checked={form.enabled}
          disabled={disabled}
          onChange={(v) => set('enabled', v)}
          label="Send entries"
        />
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}

/** Audit log destinations: delivery state, add, edit, test and remove. */
export function AuditSinksSection() {
  const sinks = useRuntime((s) => s.auditSinks);
  const loadProviders = useRuntime((s) => s.loadProviders);
  const governance = useLinks((s) => s.governance);
  const loadGovernance = useLinks((s) => s.loadGovernance);
  const saveGovernance = useLinks((s) => s.saveGovernance);
  const canWrite = useCan('patchGovernanceSettings');
  const canTest = useCan('testAuditSink');
  const [editing, setEditing] = useState<{ name: string | null; key: number } | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  useEffect(() => {
    void loadProviders();
    void loadGovernance();
  }, [loadProviders, loadGovernance]);
  const configured = (governance.data?.spec.audit_sinks ?? {}) as Record<string, Sink>;
  const names = [...new Set([...Object.keys(configured), ...(sinks.data ?? []).map((s) => s.name)])].sort();
  const dual = (governance.data?.spec.dual_control?.actions ?? []).includes('audit_sink_delete');
  const reason = canWrite ? undefined : noPermissionReason(permissionOf('patchGovernanceSettings'));
  return (
    <Card
      title="Audit log destinations"
      description="Copy the audit log to external storage, such as your SIEM."
      actions={
        <Button
          size="sm"
          icon="plus"
          disabled={!canWrite || !governance.data}
          disabledReason={reason}
          onClick={() => setEditing({ name: null, key: (editing?.key ?? 0) + 1 })}
        >
          Add destination
        </Button>
      }
    >
      <div className="flex flex-col gap-3">
        {sinks.error ? <ErrorState error={sinks.error} subject="Audit log destinations" size="sm" /> : null}
        {governance.error ? (
          <ErrorState error={governance.error} subject="Audit log settings" size="sm" />
        ) : null}
        {governance.data && names.length === 0 ? (
          <EmptyState
            size="sm"
            title="No destinations yet"
            description="The audit log stays on the gateway until you add one."
          />
        ) : null}
        {names.length > 0 ? (
          <Table label="Audit log destinations">
            <thead>
              <tr>
                <Th>Destination</Th>
                <Th>Status</Th>
                <Th numeric>Sent, last 24 h</Th>
                <Th numeric>Waiting</Th>
                <Th>Last sent</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {names.map((name) => {
                const s = sinks.data?.find((x) => x.name === name);
                const c = configured[name];
                const off = c?.enabled === false;
                return (
                  <Tr key={name}>
                    <Td className="w-full max-w-0 py-2 whitespace-normal">
                      <span className="mono font-medium text-fg">{name}</span>
                      {c ? (
                        <span className="mono block truncate text-caption text-muted">{c.url}</span>
                      ) : null}
                      <span className="text-caption text-dim">
                        {c?.format === 'json_array' ? 'JSON arrays' : 'JSON lines'} · kept trying for{' '}
                        {retryWords(c?.max_retry_duration)}
                      </span>
                    </Td>
                    <Td>
                      {off ? (
                        <Badge>Off</Badge>
                      ) : s ? (
                        <span className="flex items-center gap-1.5">
                          <Dot
                            tone={s.state === 'ok' ? 'success' : s.state === 'retrying' ? 'warn' : 'error'}
                          />
                          {s.state === 'ok'
                            ? 'Delivering'
                            : s.state === 'retrying'
                              ? 'Retrying'
                              : 'Dropping entries'}
                        </span>
                      ) : (
                        <span className="text-muted">Starting</span>
                      )}
                    </Td>
                    <Td numeric>
                      {s ? formatCount(s.entries_24h) : '—'}
                      {s?.failures_24h ? (
                        <span className="block text-caption text-warn">
                          {formatCount(s.failures_24h)} failed
                        </span>
                      ) : null}
                    </Td>
                    <Td numeric>{s ? formatCount(s.queued) : '—'}</Td>
                    <Td muted>{s?.last_write_at ? formatRelative(s.last_write_at) : 'Never'}</Td>
                    <Td>
                      <span className="flex items-center justify-end gap-1">
                        <Button
                          size="sm"
                          icon="send"
                          busy={testing === name}
                          disabled={!canTest || !s}
                          disabledReason={
                            canTest ? undefined : noPermissionReason(permissionOf('testAuditSink'))
                          }
                          onClick={() => {
                            setTesting(name);
                            settingsApi
                              .testAuditSink(name)
                              .then(
                                (r) =>
                                  r.ok
                                    ? notify('success', 'Test entry delivered', name)
                                    : notify(
                                        'error',
                                        'Test entry not delivered',
                                        r.error ?? `The destination answered ${r.status_code ?? 'nothing'}.`,
                                      ),
                                (e: unknown) =>
                                  notify('error', "Couldn't send a test entry", describeError(e)),
                              )
                              .finally(() => {
                                setTesting(null);
                                void loadProviders();
                              });
                          }}
                        >
                          Test
                        </Button>
                        <MenuButton
                          label={`More for ${name}`}
                          iconOnly
                          variant="ghost"
                          size="sm"
                          items={[
                            {
                              label: 'Edit',
                              icon: 'edit',
                              disabled: !canWrite || !c,
                              disabledReason: c ? reason : 'Set in the startup file; edit it there.',
                              onSelect: () => setEditing({ name, key: (editing?.key ?? 0) + 1 }),
                            },
                            {
                              label: 'Remove',
                              icon: 'trash',
                              danger: true,
                              disabled: !canWrite || !c,
                              disabledReason: c ? reason : 'Set in the startup file; edit it there.',
                              onSelect: () => setRemoving(name),
                            },
                          ]}
                        />
                      </span>
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : null}
        <p className="text-caption text-muted">
          Each entry is delivered at least once. While a destination is down its entries wait on the gateway
          and are retried for the time it keeps trying, then dropped and counted.
        </p>
      </div>
      {editing ? (
        <SinkEditor
          key={editing.key}
          name={editing.name}
          sink={editing.name ? configured[editing.name] : undefined}
          taken={names}
          onClose={() => {
            setEditing(null);
            void loadProviders();
          }}
        />
      ) : null}
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title={`Remove destination ${removing ?? ''}?`}
        objectName={removing ?? undefined}
        confirmLabel="Remove destination"
        effect={
          dual
            ? 'It stops receiving audit entries, and entries waiting for it are dropped. A second person must approve this first.'
            : 'It stops receiving audit entries, and entries waiting for it are dropped.'
        }
        onConfirm={async (r) => {
          if (!removing) return;
          await saveGovernance({ audit_sinks: { [removing]: null } }, r);
          notify('success', 'Destination removed', removing);
          void loadProviders();
        }}
      />
    </Card>
  );
}
