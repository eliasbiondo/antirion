'use client';

import { useEffect, useState, type ReactNode } from 'react';

import { usePermission } from '@/components/ui/gate';
import { Button } from '@/components/ui/button';
import { DatePicker } from '@/components/ui/date-picker';
import { Tooltip } from '@/components/ui/tooltip';
import { joinValues, splitValues } from '@/components/ui/select';
import {
  Checkbox,
  Combobox,
  Field,
  Input,
  MultiSelect,
  NumberInput,
  Segmented,
  Select,
  Textarea,
} from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import type { KeyResource } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { useEdited } from '@/lib/hooks';
import { plural } from '@/lib/format';
import { serverFieldErrors, validate, type FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import {
  type CertBinding,
  EMPTY_KEY_FORM,
  type ExpiryChoice,
  fromKeySpec,
  keyFormSchema,
  type KeyFormValues,
  toKeySpec,
} from '../form';
import { createKey, useKeys, useKeysContext } from '../store';
import { ENDPOINT_GROUPS, KEY_PERMISSIONS } from '../types';

export interface KeyFormDrawerProps {
  open: boolean;
  onClose: () => void;
  /** The key to edit; absent to create one. */
  editing?: KeyResource | undefined;
  /** Receives a new key's secret, to show once. */
  onCreated: (name: string, secret: string) => void;
}

const ADVANCED_FIELDS = ['rpm', 'tpm', 'allowedIps', 'permissions', 'priorityClass', 'certValue'] as const;

/** How many advanced settings the values set, for the collapsed section's summary. */
function advancedCount(v: KeyFormValues): number {
  return (
    (v.rpm.trim() ? 1 : 0) +
    (v.tpm.trim() ? 1 : 0) +
    (v.allowedIps.trim() ? 1 : 0) +
    v.ipSets.length +
    (v.regions.length > 0 ? 1 : 0) +
    (v.denyRegions.length > 0 ? 1 : 0) +
    (v.priorityClass.trim() ? 1 : 0) +
    (v.certBinding !== 'none' ? 1 : 0) +
    v.permissions.length
  );
}

function Section({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <section className="flex flex-col gap-4 border-t border-line pt-5 first:border-t-0 first:pt-0">
      <div>
        <h3 className="text-body font-semibold text-fg">{title}</h3>
        {description ? <p className="mt-0.5 text-caption text-muted">{description}</p> : null}
      </div>
      {children}
    </section>
  );
}

/**
 * Create or edit a key. The basics come first; which endpoints it may call is one choice that opens
 * a grouped list only when needed; limits, IP ranges and key permissions sit under Advanced.
 */
export function KeyFormDrawer({ open, onClose, editing, onCreated }: KeyFormDrawerProps) {
  const [values, setValues] = useState<KeyFormValues>(() =>
    editing ? fromKeySpec(editing.name, editing.spec) : EMPTY_KEY_FORM,
  );
  const [scoped, setScoped] = useState(() => values.endpoints.length > 0);
  const [kept, setKept] = useState<string[]>(values.endpoints);
  const [advanced, setAdvanced] = useState(() => advancedCount(values) > 0);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  const projects = useKeysContext((s) => s.projects.data) ?? [];
  const users = useKeysContext((s) => s.users.data) ?? [];
  const ipSets = useKeysContext((s) => s.auth.data?.spec.ip_sets);
  const knownRegions = useKeysContext((s) => s.regions.data) ?? [];
  const loadRegions = useKeysContext((s) => s.loadRegions);
  useEffect(() => {
    if (open) void loadRegions();
  }, [open, loadRegions]);
  const update = useKeys((s) => s.update);
  const canGrant = usePermission('keys:grant_permissions');
  const team = projects.find((p) => p.name === values.project)?.spec.team;
  const ownerKind = users.find((u) => u.name === values.owner)?.spec.kind;

  function set<K extends keyof KeyFormValues>(key: K, value: KeyFormValues[K]) {
    setValues((v) => ({ ...v, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _, ...rest }) => rest);
  }

  function toggle(key: 'endpoints' | 'permissions', item: string) {
    setValues((v) => ({
      ...v,
      [key]: v[key].includes(item) ? v[key].filter((x) => x !== item) : [...v[key], item],
    }));
  }

  function chooseScope(only: boolean) {
    setScoped(only);
    if (only) set('endpoints', kept);
    else {
      setKept(values.endpoints);
      set('endpoints', []);
    }
  }

  async function submit() {
    const result = validate(keyFormSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      if (ADVANCED_FIELDS.some((f) => result.errors[f])) setAdvanced(true);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      if (editing) {
        const keepExpiry =
          editing.spec.expires_at !== undefined &&
          values.expiry === 'custom' &&
          values.expiresOn === editing.spec.expires_at.slice(0, 10);
        await update(editing.name, toKeySpec(values, editing.spec, Date.now(), keepExpiry));
        notify('success', 'Key saved', editing.name);
        onClose();
      } else {
        const secret = await createKey({ name: values.name.trim(), spec: toKeySpec(values) });
        onCreated(values.name.trim(), secret);
        setValues(EMPTY_KEY_FORM);
        onClose();
      }
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `keys.${values.name.trim()}`));
    } finally {
      setBusy(false);
    }
  }

  const setCount = advancedCount(values);

  return (
    <Drawer
      dirty={edited}
      open={open}
      onClose={onClose}
      title={editing ? `Edit ${editing.name}` : 'Create API key'}
      subtitle={
        editing
          ? 'Changes apply right away and are recorded in the audit log.'
          : "You'll see the key once, right after you create it."
      }
      width="lg"
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            {editing ? 'Save changes' : 'Create key'}
          </Button>
        </>
      }
    >
      <form
        noValidate
        className="flex flex-col gap-6"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <InlineError error={error} />
        <Section title="Basics">
          <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
            <Field
              label="Name"
              error={errors.name}
              required
              hint={editing ? "A key's name can't be changed." : 'Lowercase letters, numbers, - and _'}
            >
              <Input
                mono
                value={values.name}
                onChange={(e) => set('name', e.target.value)}
                readOnly={Boolean(editing)}
                data-autofocus
                placeholder="e.g. support-bot-prod"
                autoComplete="off"
              />
            </Field>
            <Field
              label="Project"
              error={errors.project}
              required
              hint={team ? `In team ${team}` : undefined}
            >
              <Combobox
                value={values.project}
                onChange={(v) => set('project', v)}
                options={(projects.length > 0
                  ? projects
                  : [{ name: 'default', spec: { team: 'default' } }]
                ).map((p) => ({ value: p.name, label: p.name }))}
              />
            </Field>
          </div>
          <Field label="Description" optional error={errors.description}>
            <Textarea
              rows={2}
              value={values.description}
              onChange={(e) => set('description', e.target.value)}
              placeholder="e.g. Support chatbot in production"
            />
          </Field>
          <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
            <Field
              label="Owner"
              hint={
                !values.owner || ownerKind === 'service'
                  ? 'A service key keeps working when people leave.'
                  : 'Keys owned by a person stop working when they leave.'
              }
            >
              <Combobox
                value={values.owner}
                onChange={(v) => set('owner', v)}
                emptyOption="None (service key)"
                options={users.map((u) => ({
                  value: u.name,
                  label: `${u.spec.display_name ?? u.name}${u.spec.kind === 'service' ? ' (service account)' : ''}`,
                }))}
              />
            </Field>
            <Field label="Expires" error={errors.expiresOn}>
              <div className="flex gap-2">
                <Select
                  aria-label="Expires"
                  value={values.expiry}
                  onChange={(v) => set('expiry', v as ExpiryChoice)}
                  options={[
                    { value: '7d', label: 'In 7 days' },
                    { value: '30d', label: 'In 30 days' },
                    { value: '90d', label: 'In 90 days' },
                    { value: '365d', label: 'In 1 year' },
                    { value: 'custom', label: 'On a date' },
                    { value: 'never', label: 'Never' },
                  ]}
                />
                {values.expiry === 'custom' ? (
                  <DatePicker
                    aria-label="Expiry date"
                    value={values.expiresOn}
                    onChange={(v) => set('expiresOn', v)}
                  />
                ) : null}
              </div>
            </Field>
          </div>
        </Section>

        <Section title="Access" description="API keys can't be used for admin access.">
          <div className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-3">
              <span className="text-ui font-medium text-fg" id="key-endpoints-label">
                Endpoints
              </span>
              <Segmented<'all' | 'only'>
                label="Endpoints"
                value={scoped ? 'only' : 'all'}
                onChange={(v) => chooseScope(v === 'only')}
                options={[
                  { value: 'all', label: 'All endpoints' },
                  { value: 'only', label: 'Only these…' },
                ]}
              />
              {scoped ? (
                <span className="text-caption text-muted">
                  {values.endpoints.length === 0
                    ? 'None chosen yet: the key can call every endpoint.'
                    : `${plural(values.endpoints.length, 'endpoint')} chosen`}
                </span>
              ) : null}
            </div>
            {scoped ? (
              <fieldset
                aria-labelledby="key-endpoints-label"
                className="grid grid-cols-3 gap-4 rounded-control border border-line bg-sunken p-4 max-sm:grid-cols-1"
              >
                {ENDPOINT_GROUPS.map((group) => (
                  <div key={group.label} className="flex flex-col gap-2">
                    <span className="text-caption font-medium text-muted">{group.label}</span>
                    {group.items.map((item) => (
                      <Tooltip key={item.id} content={item.id} side="bottom">
                        <span>
                          <Checkbox
                            label={item.label}
                            checked={values.endpoints.includes(item.id)}
                            onChange={() => toggle('endpoints', item.id)}
                          />
                        </span>
                      </Tooltip>
                    ))}
                  </div>
                ))}
              </fieldset>
            ) : null}
          </div>
          <Field
            label="Allowed models"
            optional
            error={errors.models}
            hint="Leave blank to allow all. Use * as a wildcard, e.g. claude-*."
          >
            <MultiSelect
              mono
              creatable
              noun="model"
              placeholder="All models"
              options={[]}
              value={splitValues(values.models)}
              onChange={(v) => set('models', joinValues(v))}
            />
          </Field>
        </Section>

        <section className="flex flex-col gap-4 border-t border-line pt-5">
          <button
            type="button"
            aria-expanded={advanced}
            aria-controls="key-advanced"
            onClick={() => setAdvanced(!advanced)}
            className="-mx-1 flex items-center gap-2 rounded-xs px-1 text-left"
          >
            <Icon
              name="chevronRight"
              size={14}
              className={cn('text-muted transition-transform', advanced && 'rotate-90')}
            />
            <span className="text-body font-semibold text-fg">Advanced</span>
            <span className="text-caption text-muted">
              {setCount > 0
                ? `${plural(setCount, 'setting')} on`
                : 'Limits, IP addresses, regions, priority, client certificate and key permissions'}
            </span>
          </button>
          {advanced ? (
            <div id="key-advanced" className="flex flex-col gap-5">
              <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
                <Field
                  label="Rate limit (requests per minute)"
                  optional
                  error={errors.rpm}
                  hint="Leave blank for no limit."
                >
                  <NumberInput
                    steppers
                    min={0}
                    step={10}
                    value={values.rpm}
                    onChange={(v) => set('rpm', v)}
                  />
                </Field>
                <Field
                  label="Token limit (per minute)"
                  optional
                  error={errors.tpm}
                  hint="Leave blank for no limit."
                >
                  <NumberInput
                    steppers
                    min={0}
                    step={1000}
                    value={values.tpm}
                    onChange={(v) => set('tpm', v)}
                  />
                </Field>
              </div>
              <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
                <Field
                  label="Allowed IP addresses"
                  optional
                  error={errors.allowedIps}
                  hint="One address or range per line, e.g. 10.0.0.0/8. Leave both blank to allow any."
                >
                  <Textarea
                    mono
                    rows={3}
                    value={values.allowedIps}
                    onChange={(e) => set('allowedIps', e.target.value)}
                  />
                </Field>
                <Field
                  label="Named IP sets"
                  optional
                  hint={
                    ipSets && Object.keys(ipSets).length > 0
                      ? 'Sets defined in Settings; a change to a set applies to every key using it.'
                      : 'No IP sets are defined in Settings yet.'
                  }
                >
                  <MultiSelect
                    mono
                    noun="IP set"
                    placeholder="No sets"
                    options={Object.entries(ipSets ?? {}).map(([name, ranges]) => ({
                      value: name,
                      label: name,
                      description: ranges.join(', '),
                    }))}
                    value={values.ipSets}
                    onChange={(v) => set('ipSets', v)}
                  />
                </Field>
              </div>
              <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
                <Field
                  label="Only these regions"
                  optional
                  hint="Requests go only to deployments in these regions. Leave blank for any."
                >
                  <MultiSelect
                    mono
                    creatable
                    noun="region"
                    placeholder="Any region"
                    options={knownRegions.map((r) => ({ value: r, label: r }))}
                    value={values.regions}
                    onChange={(v) => set('regions', v)}
                  />
                </Field>
                <Field label="Never these regions" optional hint="Deployments in these regions are skipped.">
                  <MultiSelect
                    mono
                    creatable
                    noun="region"
                    placeholder="None"
                    options={knownRegions.map((r) => ({ value: r, label: r }))}
                    value={values.denyRegions}
                    onChange={(v) => set('denyRegions', v)}
                  />
                </Field>
              </div>
              <Field
                label="Priority class"
                optional
                error={errors.priorityClass}
                hint="Limits with priority shares keep part of their capacity for each class, such as interactive or batch."
              >
                <Input
                  mono
                  value={values.priorityClass}
                  onChange={(e) => set('priorityClass', e.target.value)}
                  placeholder="e.g. interactive"
                />
              </Field>
              <Field
                label="Client certificate"
                optional
                error={errors.certValue}
                hint={
                  values.certBinding === 'none'
                    ? 'Bind the key to a certificate so it works only over mutual TLS with that certificate.'
                    : 'Requests with this key are refused unless the client certificate verified for the connection matches.'
                }
              >
                <div
                  className={cn(
                    'grid gap-2',
                    values.certBinding !== 'none' && 'grid-cols-[14rem_1fr] max-sm:grid-cols-1',
                  )}
                >
                  <Select
                    aria-label="Client certificate"
                    value={values.certBinding}
                    onChange={(v) => set('certBinding', v as CertBinding)}
                    options={[
                      { value: 'none', label: 'Not required' },
                      { value: 'fingerprint', label: 'SHA-256 fingerprint' },
                      { value: 'san', label: 'Subject alternative name' },
                    ]}
                  />
                  {values.certBinding !== 'none' ? (
                    <Input
                      mono
                      aria-label={
                        values.certBinding === 'fingerprint' ? 'Certificate fingerprint' : 'Certificate name'
                      }
                      value={values.certValue}
                      onChange={(e) => set('certValue', e.target.value)}
                      placeholder={
                        values.certBinding === 'fingerprint'
                          ? 'e.g. 3F:A2:…:9C (64 hexadecimal digits)'
                          : 'e.g. spiffe://acme.test/billing'
                      }
                      autoComplete="off"
                      spellCheck={false}
                    />
                  ) : null}
                </div>
              </Field>
              <fieldset className="flex flex-col gap-3" disabled={!canGrant}>
                <legend className="mb-1 text-ui font-medium text-fg">Key permissions</legend>
                <p className="-mt-1 text-caption text-muted">
                  {canGrant
                    ? 'Extra abilities a request with this key may use. Most keys need none.'
                    : "Your role can't grant key permissions."}
                </p>
                <div className="grid grid-cols-3 gap-4 max-sm:grid-cols-1">
                  {(['Routing', 'Providers', 'Requests'] as const).map((group) => (
                    <div key={group} className="flex flex-col gap-2">
                      <span className="text-caption font-medium text-muted">{group}</span>
                      {KEY_PERMISSIONS.filter((p) => p.group === group).map((p) => (
                        <Tooltip key={p.id} content={p.id} side="bottom">
                          <span>
                            <Checkbox
                              label={p.label}
                              checked={values.permissions.includes(p.id)}
                              onChange={() => toggle('permissions', p.id)}
                            />
                          </span>
                        </Tooltip>
                      ))}
                    </div>
                  ))}
                </div>
              </fieldset>
            </div>
          ) : null}
        </section>
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}
