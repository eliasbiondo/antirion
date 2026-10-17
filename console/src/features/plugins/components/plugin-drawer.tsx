'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { FavouriteButton } from '@/components/ui/favourite';
import { Field, Input, Segmented, Select, Switch, Textarea } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { CopyLinkButton } from '@/components/ui/misc';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { formatCount, formatLatency, formatPercent } from '@/lib/format';
import { serverFieldErrors, type FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import {
  Callout,
  Facts,
  FormGroup,
  HeaderRowsEditor,
  ListInput,
  Section,
} from '@/features/mcp/components/parts';

import { attachmentName, type PluginResource } from '../api';
import {
  EMPTY_PLUGIN,
  failureRate,
  type Hook,
  HOOK_LABEL,
  pluginErrors,
  pluginForm,
  type PluginForm,
  pluginSpec,
  type PluginType,
  TYPE_LABEL,
} from '../form';
import { usePlugins } from '../store';
import { describeError } from '@/lib/api/client';
import { useEdited } from '@/lib/hooks';

const NONE: never[] = [];

/** Whether the gateway level attaches `name`, and whether that attachment is locked. */
export function useAttachment(name: string | null): { attached: boolean; locked: boolean } {
  const attach = usePlugins((s) => s.policies.data?.spec.plugins?.attach);
  const entry = (attach ?? []).find((a) => attachmentName(a) === name);
  return { attached: entry !== undefined, locked: typeof entry === 'object' && entry.locked === true };
}

function PluginView({ plugin }: { plugin: PluginResource }) {
  const runtime = usePlugins((s) => s.runtime.data?.find((r) => r.name === plugin.name));
  const setAttached = usePlugins((s) => s.setAttached);
  const canAttach = useCan('patchPoliciesSettings');
  const { attached, locked } = useAttachment(plugin.name);
  const s = plugin.spec;
  const rate = failureRate(runtime);
  return (
    <div className="flex flex-col gap-6">
      {runtime?.last_error ? (
        <Callout tone={rate !== null && rate > 0.05 ? 'error' : 'warn'} title="Its last call failed">
          {runtime.last_error}.{' '}
          {s.on_error === 'fail_open'
            ? 'Requests went on without it, as set.'
            : 'Requests it ran on were refused, since it fails closed.'}
        </Callout>
      ) : null}
      <Section title="Calls" aside="This node, since it started">
        {runtime && runtime.calls > 0 ? (
          <Facts
            items={[
              { label: 'Calls', value: formatCount(runtime.calls) },
              {
                label: 'Failures',
                value: `${formatCount(runtime.failures)}${rate !== null ? ` · ${formatPercent(rate)}` : ''}`,
              },
              { label: 'Timeouts', value: formatCount(runtime.timeouts) },
              { label: 'Typical time (p50)', value: formatLatency(runtime.p50_ms) },
              { label: 'Slowest (p99)', value: formatLatency(runtime.p99_ms) },
            ]}
          />
        ) : (
          <p className="text-ui text-muted">
            No calls yet.{' '}
            {s.enabled === false
              ? 'It is off: turn it on for it to run where it is attached.'
              : attached
                ? 'It runs on the next request.'
                : 'Attach it to a level for it to run on requests.'}
          </p>
        )}
      </Section>
      <Section title="Where it runs">
        <div className="flex flex-col gap-2">
          <Switch
            label="Attach it at the gateway level, to every request"
            checked={attached}
            disabled={!canAttach || locked}
            disabledReason={
              locked
                ? 'The gateway level attaches it locked; change it in the configuration.'
                : "You don't have permission to change policies."
            }
            onChange={(on) =>
              void setAttached(plugin.name, on).then(
                () =>
                  notify('success', on ? `${plugin.name} runs on every request` : `${plugin.name} detached`),
                (e: unknown) => notify('error', `Couldn't change ${plugin.name}`, describeError(e)),
              )
            }
          />
          <p className="text-caption text-muted">
            {s.enabled === false
              ? 'It is off, so it runs nowhere until it is turned on, attached or not. '
              : attached
                ? 'Attached: it runs on every request. '
                : 'Not attached at the gateway level. '}
            An organization, team, project, key, Model or route can attach or detach it in its own policies,
            under plugins.
          </p>
        </div>
      </Section>
      <Section title="Settings">
        <Facts
          items={[
            { label: 'Type', mono: false, value: TYPE_LABEL[s.type] },
            {
              label: 'Runs',
              mono: false,
              value: (s.hooks ?? []).map((h) => HOOK_LABEL[h]).join(', ') || '—',
            },
            { label: 'Order', value: s.order ?? 100 },
            {
              label: 'If it fails',
              mono: false,
              value: s.on_error === 'fail_open' ? 'The request goes on without it' : 'The request is refused',
            },
            { label: 'Timeout', value: s.timeout ?? '500ms' },
            { label: 'Sends the body', mono: false, value: s.include_body === false ? 'No' : 'Yes' },
            ...(s.webhook
              ? [
                  { label: 'URL', value: s.webhook.url },
                  ...(Object.keys(s.webhook.headers ?? {}).length
                    ? [
                        {
                          label: 'Headers',
                          value: `${Object.keys(s.webhook.headers ?? {}).join(', ')} (values hidden)`,
                        },
                      ]
                    : []),
                  {
                    label: 'Signed',
                    mono: false,
                    value: s.webhook.signing_secret ? 'Yes, with a stored secret' : 'No',
                  },
                  ...(s.webhook.forward_headers?.length
                    ? [{ label: 'Forwards headers', value: s.webhook.forward_headers.join(', ') }]
                    : []),
                ]
              : []),
            ...(s.builtin ? [{ label: 'Built-in', value: s.builtin.name }] : []),
            ...(s.wasm
              ? [
                  { label: 'Module', value: s.wasm.module },
                  { label: 'SHA-256', value: s.wasm.sha256 },
                ]
              : []),
            ...(s.config
              ? [
                  {
                    label: 'Config',
                    value: <pre className="whitespace-pre-wrap">{JSON.stringify(s.config, null, 2)}</pre>,
                  },
                ]
              : []),
          ]}
        />
      </Section>
    </div>
  );
}

/** One plugin: its calls, where it runs and its settings, or the form that adds or edits it. */
/** A gateway field message as the form shows it: "must be at most 30s" → "Must be at most 30s". */
const sentence = (m: string) => m.charAt(0).toUpperCase() + m.slice(1);

export function PluginDrawer({
  plugin,
  creating,
  onClose,
  onCreated,
}: {
  plugin: PluginResource | null;
  creating: boolean;
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const plugins = usePlugins((s) => s.plugins.data) ?? NONE;
  const save = usePlugins((s) => s.save);
  const remove = usePlugins((s) => s.remove);
  const canEdit = useCan('replacePlugin');
  const canDelete = useCan('deletePlugin');
  const [editing, setEditing] = useState(false);
  const [values, setValues] = useState<PluginForm>(() =>
    plugin && !creating ? pluginForm(plugin) : EMPTY_PLUGIN,
  );
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const edited = useEdited(values, creating || editing);
  const form = creating || editing;
  const name = plugin?.name ?? null;
  const { attached } = useAttachment(name);

  const set = <K extends keyof PluginForm>(key: K, value: PluginForm[K]) => {
    setValues((v) => ({ ...v, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _gone, ...rest }) => rest);
  };
  const toggleHook = (h: Hook, on: boolean) =>
    set('hooks', on ? [...values.hooks.filter((x) => x !== h), h] : values.hooks.filter((x) => x !== h));

  async function submit() {
    const found = pluginErrors(
      values,
      plugins.map((p) => p.name),
      creating,
    );
    if (Object.keys(found).length > 0) {
      setErrors(found);
      return;
    }
    const target = creating ? values.name.trim() : (plugin?.name ?? '');
    setBusy(true);
    setError(null);
    try {
      await save(target, pluginSpec(values, creating ? null : plugin), creating ? null : plugin);
      notify(
        'success',
        creating ? 'Plugin added' : 'Plugin saved',
        creating
          ? values.enabled
            ? `${target} runs once a level attaches it.`
            : `${target} is off: it runs nowhere until you turn it on.`
          : target,
      );
      setEditing(false);
      if (creating) onCreated(target);
    } catch (e) {
      const fields = serverFieldErrors(e, `plugins.${target}`);
      const mapped: FieldErrors = {
        ...(fields.webhook ? { url: fields.webhook } : {}),
        ...(fields.hooks ? { hooks: fields.hooks } : {}),
        ...(fields.config ? { config: fields.config } : {}),
        ...(fields.wasm ? { module: fields.wasm } : {}),
        ...(fields.builtin ? { builtin: fields.builtin } : {}),
        ...(fields.timeout ? { timeout: sentence(fields.timeout) } : {}),
        ...(fields.order ? { order: sentence(fields.order) } : {}),
      };
      setErrors(mapped);
      // A refusal that lands on its fields is shown there, not again at the top of the drawer.
      setError(Object.keys(mapped).length > 0 ? null : e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Drawer
        dirty={(creating || editing) && edited}
        open={creating || plugin !== null}
        onClose={onClose}
        width="lg"
        title={creating ? 'Add plugin' : (plugin?.name ?? 'Plugin')}
        subtitle={
          creating
            ? 'Code the gateway runs on requests: a webhook you host, a built-in or a WebAssembly module.'
            : plugin
              ? `${TYPE_LABEL[plugin.spec.type]}${plugin.spec.enabled === false ? ' · off' : ''}`
              : undefined
        }
        headerActions={
          plugin && !creating ? (
            <>
              <FavouriteButton kind="plugin" name={plugin.name} label={`plugin ${plugin.name}`} />
              <CopyLinkButton />
            </>
          ) : undefined
        }
        footer={
          form ? (
            <>
              {creating ? (
                <CancelButton />
              ) : (
                <Button
                  variant="ghost"
                  onClick={() => {
                    setEditing(false);
                    if (plugin) setValues(pluginForm(plugin));
                  }}
                >
                  Cancel
                </Button>
              )}
              <Button variant="primary" busy={busy} onClick={() => void submit()}>
                {creating ? 'Add plugin' : 'Save changes'}
              </Button>
            </>
          ) : plugin && (canEdit || canDelete) ? (
            <>
              {canDelete ? (
                <Button
                  variant="danger"
                  icon="trash"
                  disabled={attached}
                  disabledReason={attached ? 'Detach it from the gateway level first' : undefined}
                  onClick={() => setDeleting(true)}
                  className="mr-auto"
                >
                  Delete
                </Button>
              ) : null}
              {canEdit ? (
                <Button variant="primary" icon="edit" onClick={() => setEditing(true)}>
                  Edit
                </Button>
              ) : null}
            </>
          ) : undefined
        }
      >
        {form ? (
          <form
            noValidate
            className="flex flex-col gap-5"
            onSubmit={(e) => {
              e.preventDefault();
              void submit();
            }}
          >
            <InlineError error={error} />
            {creating ? (
              <Field
                label="Name"
                required
                error={errors.name}
                hint="Policies attach it by this name. It can't be changed later."
              >
                <Input
                  mono
                  autoComplete="off"
                  value={values.name}
                  onChange={(e) => set('name', e.target.value)}
                  data-autofocus
                />
              </Field>
            ) : null}
            <Field label="Type">
              <Segmented<PluginType>
                label="Type"
                value={values.type}
                onChange={(v) => set('type', v)}
                options={(Object.keys(TYPE_LABEL) as PluginType[]).map((t) => ({
                  value: t,
                  label: TYPE_LABEL[t],
                }))}
              />
            </Field>
            {values.type === 'webhook' ? (
              <>
                <Field
                  label="URL"
                  required
                  error={errors.url}
                  hint="The gateway posts each hook's data here and reads your answer."
                >
                  <Input
                    mono
                    inputMode="url"
                    value={values.url}
                    onChange={(e) => set('url', e.target.value)}
                  />
                </Field>
                <HeaderRowsEditor
                  label="Headers sent to the webhook"
                  rows={values.headers}
                  onChange={(rows) => set('headers', rows)}
                  error={errors.headers}
                  hint="Values are stored encrypted and never shown again."
                />
                <div className="grid gap-4 sm:grid-cols-2">
                  <Field
                    label="Signing secret"
                    optional
                    hint="Signs each call so your webhook can check it came from the gateway."
                  >
                    <Input
                      mono
                      type="password"
                      autoComplete="off"
                      placeholder={values.signingSecretStored ? 'Stored, hidden. Type to replace' : undefined}
                      value={values.signingSecret}
                      onChange={(e) => set('signingSecret', e.target.value)}
                    />
                  </Field>
                  <Field label="Largest answer" optional error={errors.maxResponse} hint="Default 1MiB">
                    <Input
                      mono
                      value={values.maxResponse}
                      onChange={(e) => set('maxResponse', e.target.value)}
                    />
                  </Field>
                </div>
                <Field
                  label="Request headers to forward"
                  optional
                  error={errors.forwardHeaders}
                  hint="Headers that carry credentials are never forwarded."
                >
                  <ListInput
                    noun="header"
                    value={values.forwardHeaders}
                    onChange={(v) => set('forwardHeaders', v)}
                  />
                </Field>
              </>
            ) : null}
            {values.type === 'builtin' ? (
              <Field
                label="Built-in"
                required
                error={errors.builtin}
                hint="static_response answers every request it runs on with fixed text."
              >
                <Input
                  mono
                  value={values.builtin}
                  onChange={(e) => set('builtin', e.target.value)}
                  placeholder="static_response"
                />
              </Field>
            ) : null}
            {values.type === 'wasm' ? (
              <>
                <Field
                  label="Module"
                  required
                  error={errors.module}
                  hint="A file:// path on the gateway or an oci:// image."
                >
                  <Input mono value={values.module} onChange={(e) => set('module', e.target.value)} />
                </Field>
                <Field
                  label="SHA-256"
                  required
                  error={errors.sha256}
                  hint="The module runs only if its content matches."
                >
                  <Input mono value={values.sha256} onChange={(e) => set('sha256', e.target.value)} />
                </Field>
                <div className="grid gap-4 sm:grid-cols-2">
                  <Field label="Memory limit" optional hint="Default 64MiB">
                    <Input mono value={values.maxMemory} onChange={(e) => set('maxMemory', e.target.value)} />
                  </Field>
                  <Field label="Time limit" optional error={errors.maxDuration} hint="Default 5ms">
                    <Input
                      mono
                      value={values.maxDuration}
                      onChange={(e) => set('maxDuration', e.target.value)}
                    />
                  </Field>
                </div>
              </>
            ) : null}
            <FormGroup title="When it runs">
              <div role="group" aria-label="Hooks" className="flex flex-col gap-2">
                {(Object.keys(HOOK_LABEL) as Hook[]).map((h) => (
                  <Checkbox
                    key={h}
                    label={HOOK_LABEL[h]}
                    checked={values.hooks.includes(h)}
                    onChange={(on) => toggleHook(h, on)}
                  />
                ))}
                {errors.hooks ? (
                  <p role="alert" className="text-caption text-error">
                    {errors.hooks}
                  </p>
                ) : null}
              </div>
              <div className="grid gap-4 sm:grid-cols-3">
                <Field label="Order" optional error={errors.order} hint="Lower runs first; default 100">
                  <Input
                    mono
                    inputMode="numeric"
                    value={values.order}
                    onChange={(e) => set('order', e.target.value)}
                  />
                </Field>
                <Field label="Timeout" optional error={errors.timeout} hint="Default 500ms; at most 30s">
                  <Input mono value={values.timeout} onChange={(e) => set('timeout', e.target.value)} />
                </Field>
                <Field label="If it fails">
                  <Select
                    value={values.onError}
                    onChange={(v) => set('onError', v as PluginForm['onError'])}
                    options={[
                      { value: 'fail_closed', label: 'Refuse the request' },
                      { value: 'fail_open', label: 'Go on without it' },
                    ]}
                  />
                </Field>
              </div>
              <Switch
                label="Send the request and response body"
                checked={values.includeBody}
                onChange={(v) => set('includeBody', v)}
              />
              <Switch label="On" checked={values.enabled} onChange={(v) => set('enabled', v)} />
            </FormGroup>
            <Field
              label="Config"
              optional
              error={errors.config}
              hint="A JSON object passed to the plugin as it is."
            >
              <Textarea mono rows={5} value={values.config} onChange={(e) => set('config', e.target.value)} />
            </Field>
            <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
          </form>
        ) : plugin ? (
          <PluginView plugin={plugin} />
        ) : null}
      </Drawer>
      <ConfirmDialog
        open={deleting}
        onClose={() => setDeleting(false)}
        title={`Delete plugin ${name ?? ''}?`}
        objectName={name ?? undefined}
        typeToConfirm={name ?? undefined}
        confirmLabel="Delete plugin"
        effect="The gateway forgets it. A level that still attaches it makes the next configuration change fail until that attachment is removed."
        onConfirm={async (reason) => {
          if (!name) return;
          await remove(name, reason);
          notify('success', 'Plugin deleted', name);
          onClose();
        }}
      />
    </>
  );
}
