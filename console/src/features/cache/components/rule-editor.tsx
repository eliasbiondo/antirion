'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Field, Input, Segmented, Select, Switch, Textarea } from '@/components/ui/form';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { modelOption } from '@/components/ui/marks';
import type { InvalidationRuleResource } from '@/lib/api/types';
import { notify } from '@/stores/ui';

import {
  describeRule,
  draftErrors,
  draftOf,
  DROPS,
  emptyDraft,
  PRESETS,
  SCHEDULES,
  specOf,
  TRIGGERS,
  valueFromTrigger,
  type DropKind,
  type RuleDraft,
  type TriggerEvent,
} from '../rules';
import { useCache } from '../store';
import { useEdited } from '@/lib/hooks';

const VALUE_HINT: Record<DropKind, string> = {
  model: 'The model whose responses to clear.',
  route: 'The route whose responses to clear.',
  deployment: 'The deployment whose responses to clear.',
  key: 'The API key whose responses to clear.',
  end_user: 'The end user id apps send.',
  tag: 'A request tag.',
  metadata: 'key=value, such as prompt_version=v2.',
  older_than: '',
};

/**
 * Creates or edits a clearing rule: a trigger (a model turned off, a deployment or route changed, a
 * schedule, a request metadata value changing, or by hand) and what it clears, with the two presets
 * and the rule in words before saving.
 */
export function RuleEditor({
  rule,
  onClose,
}: {
  /** The rule to edit; null creates one. */
  rule: InvalidationRuleResource | null;
  onClose: () => void;
}) {
  const save = useCache((s) => s.saveRule);
  const models = useCache((s) => s.models.data);
  const routes = useCache((s) => s.routes.data);
  const creating = rule === null;
  const [d, setD] = useState<RuleDraft>(rule ? draftOf(rule.name, rule.spec) : emptyDraft());
  const [errors, setErrors] = useState<ReturnType<typeof draftErrors>>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(d);
  const put = (patch: Partial<RuleDraft>) => setD((prev) => ({ ...prev, ...patch }));
  const fromTrigger = valueFromTrigger(d.event, d.drop);

  async function submit() {
    const found = draftErrors(d, creating);
    setErrors(found);
    if (Object.keys(found).length > 0) return;
    setBusy(true);
    setError(null);
    try {
      await save(creating ? d.name : rule.name, specOf(d), rule);
      notify(
        'success',
        creating ? 'Clearing rule created' : 'Clearing rule saved',
        creating ? d.name : rule.name,
      );
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const valueOptions =
    d.drop === 'model'
      ? (models ?? []).map((m) => modelOption(m))
      : d.drop === 'route'
        ? [...(routes?.keys() ?? [])].map((r) => ({ value: r, label: r }))
        : null;

  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      title={creating ? 'Create clearing rule' : `Edit ${rule.name}`}
      subtitle="Clears cached responses when something changes, or on a schedule."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            {creating ? 'Create rule' : 'Save rule'}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-5">
        <InlineError error={error} />
        {creating ? (
          <div className="flex flex-col gap-2">
            <span className="text-ui font-semibold text-fg">Start from</span>
            <div className="grid gap-2 sm:grid-cols-2">
              {PRESETS.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => setD({ ...p.draft })}
                  className="rounded-control border border-line bg-canvas px-3 py-2 text-left hover:border-line-strong focus-visible:outline-2 focus-visible:outline-accent"
                >
                  <span className="block text-ui font-semibold text-fg">{p.label}</span>
                  <span className="block text-caption text-muted">{p.description}</span>
                </button>
              ))}
            </div>
          </div>
        ) : null}
        {creating ? (
          <Field label="Name" error={errors.name} hint="Used in the activity feed and the audit log.">
            <Input mono value={d.name} onChange={(e) => put({ name: e.target.value })} data-autofocus />
          </Field>
        ) : null}
        <Field label="Description" optional error={errors.description}>
          <Textarea rows={2} value={d.description} onChange={(e) => put({ description: e.target.value })} />
        </Field>

        <section aria-labelledby="rule-when" className="flex flex-col gap-4 border-t border-line pt-5">
          <h3 id="rule-when" className="text-ui font-semibold text-fg">
            When
          </h3>
          <Field label="Trigger">
            <Select
              aria-label="Trigger"
              value={d.event}
              onChange={(v) => put({ event: v as TriggerEvent })}
              options={TRIGGERS.map((t) => ({ value: t.value, label: t.label }))}
            />
          </Field>
          {d.event === 'schedule' ? (
            <>
              <Field label="Schedule">
                <Segmented<string>
                  label="Schedule"
                  value={SCHEDULES.some((s) => s.value === d.schedule) ? d.schedule : 'custom'}
                  onChange={(v) => put({ schedule: v === 'custom' ? '' : v })}
                  options={[
                    { value: '0 3 * * *', label: 'Nightly' },
                    { value: '0 * * * *', label: 'Hourly' },
                    { value: '0 0 * * 1', label: 'Weekly' },
                    { value: 'custom', label: 'Custom' },
                  ]}
                />
              </Field>
              <Field
                label="Cron expression"
                error={errors.schedule}
                hint="Five fields in UTC: minute, hour, day of month, month, day of week."
              >
                <Input mono value={d.schedule} onChange={(e) => put({ schedule: e.target.value })} />
              </Field>
            </>
          ) : null}
          {d.event === 'metadata_changed' ? (
            <Field
              label="Metadata key"
              error={errors.metadataKey}
              hint="When a request carries a new value for this key, responses recorded with the old value are cleared."
            >
              <Input mono value={d.metadataKey} onChange={(e) => put({ metadataKey: e.target.value })} />
            </Field>
          ) : null}
        </section>

        <section aria-labelledby="rule-what" className="flex flex-col gap-4 border-t border-line pt-5">
          <h3 id="rule-what" className="text-ui font-semibold text-fg">
            Clear
          </h3>
          <Field label="What to clear">
            <Select
              aria-label="What to clear"
              value={d.drop}
              onChange={(v) => put({ drop: v as DropKind, value: '' })}
              options={DROPS}
            />
          </Field>
          {d.drop === 'older_than' ? (
            <Field
              label="Older than"
              error={errors.olderThan}
              hint="Responses cached longer ago than this, such as 12h or 7d."
            >
              <Input mono value={d.olderThan} onChange={(e) => put({ olderThan: e.target.value })} />
            </Field>
          ) : (
            <Field
              label={d.drop === 'metadata' ? 'Metadata value' : 'Which one'}
              optional={fromTrigger}
              error={errors.value}
              hint={
                fromTrigger
                  ? d.drop === 'metadata'
                    ? 'Leave empty to clear what was recorded with the previous value.'
                    : `Leave empty to clear the ${d.drop} that triggered the rule.`
                  : VALUE_HINT[d.drop]
              }
            >
              {valueOptions ? (
                <Select
                  aria-label={d.drop === 'model' ? 'Model' : 'Route'}
                  value={d.value}
                  onChange={(value) => put({ value })}
                  options={valueOptions}
                  searchable
                  {...(fromTrigger
                    ? { emptyOption: `The ${d.drop} that triggered it` }
                    : { placeholder: `Choose a ${d.drop}` })}
                />
              ) : (
                <Input mono value={d.value} onChange={(e) => put({ value: e.target.value })} />
              )}
            </Field>
          )}
        </section>

        <Switch label="Rule on" checked={d.enabled} onChange={(enabled) => put({ enabled })} />
        <p className="rounded-control border border-line bg-canvas px-3 py-2 text-ui text-fg">
          <span className="text-muted">In words: </span>
          {describeRule(specOf(d))}
          {!d.enabled ? <span className="text-muted"> It’s off, so it only runs by hand.</span> : null}
        </p>
      </div>
    </Drawer>
  );
}
