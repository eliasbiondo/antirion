'use client';

import { useEffect, useMemo, useState } from 'react';

import { Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import {
  Combobox,
  Field,
  Input,
  MultiSelect,
  NumberInput,
  Segmented,
  Select,
  Textarea,
} from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { api, listAll } from '@/lib/api/client';
import type { FieldErrors } from '@/lib/validation';
import { useEdited } from '@/lib/hooks';
import { notify } from '@/stores/ui';

import type { RuleSpec } from '../api';
import {
  categoryWord,
  CHANNEL_TYPES,
  describeCondition,
  durationWords,
  METRICS,
  SCOPE_DIMENSIONS,
  severityWord,
  UNIT_WORD,
  type ScopeDimension,
} from '../describe';
import { useAlertEditors } from '../editors';
import { categoryForEvents, EVENT_SOURCES, eventWords } from '../events';
import {
  mapServerErrors,
  RULE_FIELD,
  ruleErrors,
  ruleForm,
  ruleSpec,
  suggestName,
  type RuleForm,
} from '../form';
import { severityTone, useAlerts } from '../store';
import { RULE_TEMPLATES, type RuleTemplate } from '../templates';
import { RulePreview } from './rule-preview';

const CATEGORIES: RuleSpec['category'][] = [
  'latency',
  'errors',
  'availability',
  'capacity',
  'budget',
  'cost',
  'safety',
  'pii',
  'cache',
  'security',
  'anomaly',
];

const OPERATORS = [
  { value: 'gt', label: 'is more than' },
  { value: 'gte', label: 'is at least' },
  { value: 'lt', label: 'is less than' },
  { value: 'lte', label: 'is at most' },
];

const withCurrent = (values: string[], current: string) =>
  (values.includes(current) || !current ? values : [...values, current]).map((v) => ({
    value: v,
    label: v === '0s' ? 'No wait' : (durationWords(v) ?? v),
  }));

const WINDOWS = ['1m', '5m', '10m', '15m', '30m', '1h', '6h', '24h'];
const HOLD = ['0s', '1m', '2m', '5m', '10m', '15m', '30m'];
const COOLDOWNS = ['0s', '5m', '10m', '15m', '30m', '1h', '4h', '6h', '24h'];
const RESOLVE = ['0s', '1m', '5m', '10m', '15m', '30m', '1h'];

const SCOPE_LABEL: Record<ScopeDimension, string> = {
  organization: 'Organization',
  team: 'Team',
  project: 'Project',
  key: 'API key',
  provider: 'Provider',
  deployment: 'Deployment',
  model: 'Model',
  route: 'Route',
};

const SCOPE_READ: Partial<Record<ScopeDimension, string>> = {
  organization: '/organizations',
  team: '/teams',
  project: '/projects',
  provider: '/providers',
  deployment: '/deployments',
  model: '/models',
  route: '/routes',
};

/** The names the scope fields offer, read once per editor; a list the reader may not read stays free text. */
function useScopeOptions(): Partial<Record<ScopeDimension, string[]>> {
  const [options, setOptions] = useState<Partial<Record<ScopeDimension, string[]>>>({});
  useEffect(() => {
    let cancelled = false;
    for (const [dim, path] of Object.entries(SCOPE_READ) as [ScopeDimension, string][]) {
      listAll(
        (cursor) =>
          (
            api.get as (
              p: string,
              o: unknown,
            ) => Promise<{ items: { name: string }[]; next_cursor?: string | null }>
          )(path, { query: { limit: 200, ...(cursor ? { cursor } : {}) } }),
        5,
      )
        .then((items) => {
          if (!cancelled) setOptions((o) => ({ ...o, [dim]: items.map((i) => i.name).sort() }));
        })
        .catch(() => undefined);
    }
    return () => {
      cancelled = true;
    };
  }, []);
  return options;
}

function Section({ title, aside, children }: { title: string; aside?: string; children: React.ReactNode }) {
  return (
    <fieldset className="flex flex-col gap-4 border-t border-line pt-4 first:border-t-0 first:pt-0">
      <legend className="float-left mb-1 flex w-full items-baseline justify-between gap-2">
        <span className="text-ui font-semibold text-fg">{title}</span>
        {aside ? <span className="text-caption text-muted">{aside}</span> : null}
      </legend>
      {children}
    </fieldset>
  );
}

/** The template gallery a new rule starts from. */
function TemplatePicker({ onPick }: { onPick: (t: RuleTemplate | 'metric' | 'event') => void }) {
  return (
    <div className="flex flex-col gap-4">
      <p className="text-ui text-muted">
        Start from a built-in template or a blank rule. Nothing fires until you save it, and every field can
        be changed.
      </p>
      <ul className="grid grid-cols-2 gap-2 max-sm:grid-cols-1" aria-label="Rule templates">
        {RULE_TEMPLATES.map((t) => (
          <li key={t.id}>
            <button
              type="button"
              onClick={() => onPick(t)}
              className="flex h-full w-full items-start gap-3 rounded-control border border-line bg-canvas px-3 py-2.5 text-left hover:border-line-strong hover:bg-hover"
            >
              <span className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-control bg-sunken text-muted">
                <Icon name={t.icon} size={16} />
              </span>
              <span className="min-w-0">
                <span className="flex items-center gap-2 text-ui font-medium text-fg">
                  {t.title}
                  <Dot tone={severityTone(t.spec.severity ?? 'warning')} />
                </span>
                <span className="mt-0.5 block text-caption text-muted">{t.summary}</span>
              </span>
            </button>
          </li>
        ))}
      </ul>
      <div className="flex flex-wrap gap-2 border-t border-line pt-4">
        <Button icon="plus" onClick={() => onPick('metric')}>
          Blank metric rule
        </Button>
        <Button icon="webhook" onClick={() => onPick('event')}>
          Subscribe to an event
        </Button>
      </div>
    </div>
  );
}

/**
 * Creates or edits an alert rule: a template gallery for a new one, then what to watch (a metric
 * with its condition, or events), the scope, how it alerts, who it notifies, and a preview of how
 * often it would have fired.
 */
export function RuleDrawer({
  name,
  draft,
}: {
  name: string | null;
  draft?: { name?: string; spec: Partial<RuleSpec> } | undefined;
}) {
  const close = useAlertEditors((s) => s.close);
  const rules = useAlerts((s) => s.rules.data);
  const channels = useAlerts((s) => s.channels.data) ?? [];
  const saveRule = useAlerts((s) => s.saveRule);
  const deleteRule = useAlerts((s) => s.deleteRule);
  const show = useAlertEditors((s) => s.show);
  const canWrite = useCan(name ? 'replaceAlertRule' : 'createAlertRule');
  const canDelete = useCan('deleteAlertRule');
  const existing = name ? (rules?.find((r) => r.name === name) ?? null) : null;
  const taken = useMemo(() => (rules ?? []).map((r) => r.name), [rules]);
  const [picking, setPicking] = useState(!name && !draft);
  const [template, setTemplate] = useState<RuleTemplate | null>(null);
  const [form, setForm] = useState<RuleForm>(() =>
    existing
      ? ruleForm(existing.name, existing.spec)
      : ruleForm(draft?.name ?? suggestName(draft?.spec.events?.[0] ?? 'rule', taken), draft?.spec ?? {}),
  );
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(form, picking ? 'templates' : (template?.title ?? 'rule'));
  const [confirm, setConfirm] = useState(false);
  /** After a conflict, the fields someone else changed: the form keeps its edits over their version. */
  const [theirs, setTheirs] = useState<string[] | null>(null);
  const scopeOptions = useScopeOptions();
  const [moreScope, setMoreScope] = useState(
    () => !!(existing?.spec.scope && Object.keys(existing.spec.scope).length) || !!draft?.spec.scope,
  );
  const set = <K extends keyof RuleForm>(key: K, value: RuleForm[K]) => {
    setForm((f) => ({ ...f, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _, ...rest }) => rest);
  };
  const spec = ruleSpec(form);

  function pick(t: RuleTemplate | 'metric' | 'event') {
    if (t === 'metric') {
      setTemplate(null);
      setForm(
        ruleForm(suggestName('rule', taken), { kind: 'metric', metric: 'error_rate', category: 'errors' }),
      );
    } else if (t === 'event') {
      setTemplate(null);
      setForm(ruleForm(suggestName('events', taken), { kind: 'event', category: 'anomaly', cooldown: '0s' }));
    } else {
      setTemplate(t);
      setForm(ruleForm(suggestName(t.name, taken), t.spec));
      if (t.asks) setMoreScope(true);
    }
    setPicking(false);
  }

  /** Reads the rule as someone else saved it; the next save replaces it with this form. */
  async function reload() {
    const before = existing?.spec ?? {};
    await useAlerts.getState().load({ quiet: true });
    const after = useAlerts.getState().rules.data?.find((r) => r.name === name)?.spec ?? {};
    const keys = new Set([...Object.keys(before), ...Object.keys(after)]);
    const b = before as Record<string, unknown>;
    const a = after as Record<string, unknown>;
    setTheirs([...keys].filter((k) => JSON.stringify(b[k] ?? null) !== JSON.stringify(a[k] ?? null)));
    setError(null);
  }

  async function submit() {
    const found = ruleErrors(form, existing, taken);
    setErrors(found);
    if (Object.keys(found).length) return;
    setBusy(true);
    setError(null);
    try {
      await saveRule(form.name, spec, existing);
      notify('success', existing ? 'Rule saved' : 'Rule created', form.name);
      close();
    } catch (e) {
      setError(e);
      setErrors(mapServerErrors<RuleForm>(e, `observability.alerts.rules.${form.name}`, RULE_FIELD));
    } finally {
      setBusy(false);
    }
  }

  const more: MenuItem[] = [];
  if (existing) {
    more.push({
      label: 'Duplicate',
      icon: 'copy',
      onSelect: () =>
        show({
          kind: 'rule',
          name: null,
          draft: { name: suggestName(existing.name, taken), spec: existing.spec },
        }),
    });
    more.push({
      label: 'Delete rule',
      icon: 'trash',
      danger: true,
      disabled: !canDelete,
      disabledReason: canDelete ? undefined : "You don't have permission to delete rules.",
      onSelect: () => setConfirm(true),
    });
  }
  const denied = canWrite ? undefined : "You don't have permission to change alert rules.";
  const metric = METRICS[form.metric];
  const scopeCount = SCOPE_DIMENSIONS.filter((d) => form.scope[d]).length;
  const channelOptions = channels.map((c) => ({
    value: c.name,
    label: c.name,
    description: CHANNEL_TYPES[c.spec.type],
  }));
  const ruleEvents = EVENT_SOURCES.flatMap((s) =>
    s.events
      .filter((e) => e.rule)
      .map((e) => ({ value: e.type, label: e.label, description: `${s.label} · ${e.type}` })),
  );
  const prefixes = EVENT_SOURCES.filter((s) => s.events.some((e) => e.rule) && s.events.length > 1).map(
    (s) => ({
      value: `${s.prefix}.*`,
      label: eventWords(`${s.prefix}.*`),
      description: `${s.prefix}.*`,
    }),
  );

  return (
    <Drawer
      dirty={edited}
      open
      onClose={close}
      width="lg"
      title={
        existing
          ? existing.name
          : picking
            ? 'New alert rule'
            : template
              ? `New rule: ${template.title}`
              : 'New alert rule'
      }
      subtitle={
        existing
          ? `${categoryWord(existing.spec.category)} rule. Changes apply at the next evaluation and are recorded in the audit log.`
          : picking
            ? undefined
            : 'Saved rules are evaluated every minute across the gateway.'
      }
      footer={
        picking ? (
          <CancelButton />
        ) : (
          <>
            {more.length ? (
              <span className="mr-auto">
                <MenuButton items={more} label="More" align="left" />
              </span>
            ) : !existing ? (
              <span className="mr-auto">
                <Button variant="ghost" icon="template" onClick={() => setPicking(true)}>
                  Templates
                </Button>
              </span>
            ) : null}
            <CancelButton />
            <Button
              variant="primary"
              busy={busy}
              disabled={!canWrite}
              disabledReason={denied}
              onClick={() => void submit()}
            >
              {existing ? 'Save changes' : 'Create rule'}
            </Button>
          </>
        )
      }
    >
      {picking ? (
        <TemplatePicker onPick={pick} />
      ) : (
        <form
          noValidate
          className="flex flex-col gap-5"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <InlineError error={error} onReload={existing ? () => void reload() : undefined} />
          {theirs ? (
            <p role="status" className="text-caption text-muted">
              {theirs.length
                ? `Reloaded. They changed ${theirs.map((k) => k.replace(/_/g, ' ')).join(', ')}; your edits are kept, and saving replaces their version.`
                : 'Reloaded. Their save changed nothing you edit here; your edits are kept.'}
            </p>
          ) : null}
          <Section title="What to watch">
            <Segmented<'metric' | 'event'>
              label="Rule kind"
              value={form.kind}
              onChange={(kind) => {
                set('kind', kind);
                if (kind === 'event' && form.cooldown === '15m') set('cooldown', '0s');
              }}
              options={[
                { value: 'metric', label: 'A metric crosses a threshold', icon: 'activity' },
                { value: 'event', label: 'An event happens', icon: 'webhook' },
              ]}
              className="w-fit max-sm:w-full"
            />
            {form.kind === 'metric' ? (
              <>
                <div className="grid grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)_minmax(0,0.9fr)] gap-3 max-sm:grid-cols-1">
                  <Field label="Metric">
                    <Select
                      value={form.metric}
                      onChange={(v) => set('metric', v as RuleForm['metric'])}
                      options={Object.entries(METRICS).map(([value, m]) => ({
                        value,
                        label: m.label + (m.baseline ? ' against its baseline' : ''),
                        description: value,
                      }))}
                    />
                  </Field>
                  <Field label="Condition">
                    <Select
                      value={form.operator}
                      onChange={(v) => set('operator', v as RuleForm['operator'])}
                      options={OPERATORS}
                    />
                  </Field>
                  <Field
                    label={UNIT_WORD[metric.unit] ? `Threshold (${UNIT_WORD[metric.unit]})` : 'Threshold'}
                    error={errors.threshold}
                    required
                  >
                    <NumberInput
                      value={form.threshold}
                      onChange={(v) => set('threshold', v)}
                      step={
                        metric.unit === 'ms'
                          ? 100
                          : metric.unit === 'usd'
                            ? 0.01
                            : metric.unit === 'ratio'
                              ? 0.5
                              : 1
                      }
                      min={0}
                      data-autofocus
                    />
                  </Field>
                </div>
                <div className="grid grid-cols-2 gap-3 max-sm:grid-cols-1">
                  <Field label="Measured over" hint="The trailing window the metric is computed on.">
                    <Select
                      value={form.window}
                      onChange={(v) => set('window', v)}
                      options={withCurrent(WINDOWS, form.window)}
                    />
                  </Field>
                  <Field label="Fires after it holds for" hint="How long the condition must stay true.">
                    <Select
                      value={form.holdFor}
                      onChange={(v) => set('holdFor', v)}
                      options={withCurrent(HOLD, form.holdFor)}
                    />
                  </Field>
                </div>
              </>
            ) : (
              <Field
                label="Events"
                error={errors.events}
                required
                hint="The rule raises an alert for each of these events; a whole source covers its future events too."
              >
                <MultiSelect
                  mono
                  creatable
                  noun="event"
                  placeholder="Choose events"
                  options={[...prefixes, ...ruleEvents]}
                  value={form.events}
                  onChange={(v) => {
                    set('events', v);
                    if (!existing && v.length && form.events.length === 0)
                      set('category', categoryForEvents(v) as RuleForm['category']);
                  }}
                />
              </Field>
            )}
            <p className="border-l-2 border-accent pl-3 text-ui text-fg" aria-live="polite">
              {describeCondition(spec)}
              {scopeCount ? (
                <span className="text-muted">
                  {' '}
                  · only for{' '}
                  {SCOPE_DIMENSIONS.filter((d) => form.scope[d])
                    .map((d) => `${SCOPE_LABEL[d].toLowerCase()} ${form.scope[d]}`)
                    .join(', ')}
                </span>
              ) : null}
            </p>
            <RulePreview spec={spec} existing={existing?.name ?? null} />
          </Section>

          <Section title="Scope" aside={scopeCount ? `${scopeCount} set` : 'Whole gateway'}>
            {moreScope ? (
              <div className="grid grid-cols-2 gap-3 max-sm:grid-cols-1">
                {SCOPE_DIMENSIONS.map((d) => {
                  const opts = scopeOptions[d];
                  return (
                    <Field key={d} label={SCOPE_LABEL[d]} optional>
                      {opts?.length ? (
                        <Combobox
                          mono
                          value={form.scope[d]}
                          emptyOption="Any"
                          onChange={(v) => set('scope', { ...form.scope, [d]: v })}
                          options={withValue(opts, form.scope[d])}
                        />
                      ) : (
                        <Input
                          mono
                          value={form.scope[d]}
                          placeholder="Any"
                          onChange={(e) => set('scope', { ...form.scope, [d]: e.target.value })}
                        />
                      )}
                    </Field>
                  );
                })}
              </div>
            ) : (
              <div className="flex flex-wrap items-center gap-3">
                <p className="text-ui text-muted">The rule watches all traffic.</p>
                <Button size="sm" icon="filter" onClick={() => setMoreScope(true)}>
                  {template?.asks ? `Choose a ${template.asks}` : 'Limit to part of the traffic'}
                </Button>
              </div>
            )}
          </Section>

          <Section title="How it alerts">
            <div className="flex flex-col gap-1.5">
              <span className="text-ui font-medium text-fg">Severity</span>
              <Segmented<RuleForm['severity']>
                label="Severity"
                value={form.severity}
                onChange={(v) => set('severity', v)}
                options={(['critical', 'warning', 'info'] as const).map((s) => ({
                  value: s,
                  label: severityWord(s),
                }))}
                className="w-fit"
              />
            </div>
            <div className="grid grid-cols-3 gap-3 max-sm:grid-cols-1">
              <Field label="Category">
                <Select
                  value={form.category}
                  onChange={(v) => set('category', v as RuleForm['category'])}
                  options={CATEGORIES.map((c) => ({ value: c, label: categoryWord(c) }))}
                />
              </Field>
              <Field label="Quiet after firing" hint="No new alert from this rule for this long.">
                <Select
                  value={form.cooldown}
                  onChange={(v) => set('cooldown', v)}
                  options={withCurrent(COOLDOWNS, form.cooldown)}
                />
              </Field>
              <Field label="Resolves after" hint="Clear for this long.">
                <Select
                  value={form.resolveAfter}
                  onChange={(v) => set('resolveAfter', v)}
                  options={withCurrent(RESOLVE, form.resolveAfter)}
                />
              </Field>
            </div>
            <div className="flex flex-col gap-2">
              <Switch
                checked={form.silence}
                onChange={(v) => set('silence', v)}
                label="Silence during maintenance windows"
              />
              <p className="text-caption text-muted">
                An alert that fires inside a maintenance window is snoozed until the window ends and notifies
                no one.
              </p>
              <Switch checked={form.enabled} onChange={(v) => set('enabled', v)} label="Rule is on" />
            </div>
          </Section>

          <Section title="Who hears about it">
            <Field
              label="Notify"
              optional
              error={errors.channels}
              hint={
                channels.length
                  ? 'Each channel gets the alerts at or above its own minimum severity.'
                  : undefined
              }
              aside={
                <button
                  type="button"
                  className="text-caption text-accent hover:underline"
                  onClick={() => show({ kind: 'channel', name: null }, { over: true })}
                >
                  New channel
                </button>
              }
            >
              <MultiSelect
                mono
                noun="channel"
                placeholder={channels.length ? 'No channels' : 'No channels yet'}
                options={channelOptions}
                value={form.channels}
                onChange={(v) => set('channels', v)}
              />
            </Field>
            <Field
              label="Escalate to"
              optional
              error={errors.escalation}
              hint="Paged when someone escalates the alert."
            >
              <MultiSelect
                mono
                noun="channel"
                placeholder="No escalation"
                options={channelOptions}
                value={form.escalation}
                onChange={(v) => set('escalation', v)}
              />
            </Field>
            <div className="grid grid-cols-2 gap-3 max-sm:grid-cols-1">
              <Field label="Owner team" optional error={errors.ownerTeam}>
                {scopeOptions.team?.length ? (
                  <Combobox
                    mono
                    value={form.ownerTeam}
                    emptyOption="No owner"
                    onChange={(v) => set('ownerTeam', v)}
                    options={withValue(scopeOptions.team, form.ownerTeam)}
                  />
                ) : (
                  <Input mono value={form.ownerTeam} onChange={(e) => set('ownerTeam', e.target.value)} />
                )}
              </Field>
              <Field label="Runbook link" optional error={errors.runbookUrl}>
                <Input
                  value={form.runbookUrl}
                  placeholder="https://"
                  onChange={(e) => set('runbookUrl', e.target.value)}
                />
              </Field>
            </div>
            <Field
              label="Description"
              optional
              error={errors.description}
              hint="Shown under the alert's title."
            >
              <Textarea
                rows={2}
                maxLength={1000}
                value={form.description}
                onChange={(e) => set('description', e.target.value)}
              />
            </Field>
          </Section>

          <Section title="Name">
            <Field
              label="Rule name"
              error={errors.name}
              hint={
                existing
                  ? 'A rule keeps its name; duplicate it to rename.'
                  : 'Alerts and the audit log refer to the rule by this name.'
              }
            >
              <Input
                mono
                value={form.name}
                disabled={!!existing}
                onChange={(e) => set('name', e.target.value)}
              />
            </Field>
          </Section>
          <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
        </form>
      )}
      {existing ? (
        <ConfirmDialog
          open={confirm}
          onClose={() => setConfirm(false)}
          title={`Delete rule ${existing.name}?`}
          confirmLabel="Delete rule"
          objectName={existing.name}
          typeToConfirm={existing.name}
          effect="The rule stops evaluating. Its past alerts stay in the inbox until they age out."
          onConfirm={async () => {
            await deleteRule(existing);
            notify('success', 'Rule deleted', existing.name);
            close();
          }}
        />
      ) : null}
    </Drawer>
  );
}

function withValue(values: string[], current: string) {
  const all = current && !values.includes(current) ? [current, ...values] : values;
  return all.map((v) => ({ value: v, label: v }));
}
