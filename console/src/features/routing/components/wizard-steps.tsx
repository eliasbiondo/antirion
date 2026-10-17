'use client';

import type { ReactNode } from 'react';

import { Button, IconButton } from '@/components/ui/button';
import { Checkbox, RadioGroup } from '@/components/ui/choice';
import { Field, Input, NumberInput, Segmented } from '@/components/ui/form';
import { ModelMark } from '@/components/ui/marks';
import { Combobox, Select } from '@/components/ui/select';
import type { DeploymentResource, Tier } from '@/lib/api/types';

import { canStick, emptyCondition, movesOn, type RuleDraft, type StickyKind } from '../draft';
import { type Condition, FIELDS, GUARD_FIELDS, OPERATORS, STRATEGIES, type Target, TRIGGERS } from '../types';
import { tierLabel } from '@/features/models/types';

import { AdaptiveFields } from './adaptive-fields';
import { CostQualityFields, PoolFields } from './pool-fields';
import { HedgingFields, ResponseChecks } from './resilience-fields';

type Set = <K extends keyof RuleDraft>(key: K, value: RuleDraft[K]) => void;
type Errors = Record<string, string>;

function FieldError({ error }: { error: string | undefined }) {
  return error ? <p className="text-caption text-error">{error}</p> : null;
}

/** Step 1: the conditions, the share of requests, and the rule's name, note and switch. */
export function ConditionsStep({
  draft,
  set,
  errors,
  guardrails,
  locked,
}: {
  draft: RuleDraft;
  set: Set;
  errors: Errors;
  guardrails: string[];
  /** The rule's condition has parts the wizard can't show; it is kept as written. */
  locked: string | null;
}) {
  const update = (i: number, patch: Partial<Condition>) =>
    set(
      'conditions',
      draft.conditions.map((x, j) => (j === i ? { ...x, ...patch } : x)),
    );
  return (
    <div className="flex flex-col gap-4">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Name" error={errors.name} required>
          <Input value={draft.name} onChange={(e) => set('name', e.target.value)} data-autofocus />
        </Field>
        {locked === null ? (
          <Field label="Match">
            <Segmented<'all' | 'any'>
              label="Match"
              value={draft.join}
              onChange={(join) => set('join', join)}
              options={[
                { value: 'all', label: 'All conditions' },
                { value: 'any', label: 'Any condition' },
              ]}
            />
          </Field>
        ) : null}
      </div>
      {locked !== null ? (
        <div className="rounded-control border border-line bg-sunken px-3 py-2.5 text-ui">
          <p className="text-fg">When {locked}</p>
          <p className="mt-1 text-caption text-muted">
            This condition uses fields the wizard doesn’t edit, so it is kept as written. Change it in the
            configuration.
          </p>
        </div>
      ) : (
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-2 text-ui font-medium text-fg">Conditions</legend>
          {draft.conditions.map((c, i) => (
            <div key={i} className="flex flex-col gap-1">
              <div className="grid grid-cols-[1fr_110px_1fr_auto] items-start gap-2 max-sm:grid-cols-2">
                <Select
                  aria-label={`Field of condition ${i + 1}`}
                  value={c.kind}
                  onChange={(v) => update(i, { kind: v as Condition['kind'] })}
                  options={FIELDS.filter((f) => f.value !== 'sample').map((f) => ({
                    value: f.value,
                    label: f.label,
                  }))}
                />
                <Select
                  aria-label={`Operator of condition ${i + 1}`}
                  value={c.op}
                  onChange={(v) => update(i, { op: v as Condition['op'] })}
                  options={OPERATORS}
                />
                <div>
                  <Input
                    mono
                    aria-label={`Value of condition ${i + 1}`}
                    aria-invalid={errors[`conditions.${i}.value`] ? true : undefined}
                    placeholder={
                      c.kind === 'guardrail' && c.guardField === 'passed'
                        ? 'true or false'
                        : c.op === 'in' || c.op === 'nin'
                          ? 'a, b, c'
                          : undefined
                    }
                    value={c.value}
                    onChange={(e) => update(i, { value: e.target.value })}
                  />
                  <FieldError error={errors[`conditions.${i}.value`]} />
                </div>
                <IconButton
                  icon="x"
                  label={`Remove condition ${i + 1}`}
                  onClick={() =>
                    set(
                      'conditions',
                      draft.conditions.filter((_, j) => j !== i),
                    )
                  }
                />
              </div>
              {c.kind === 'metadata' ? (
                <div className="grid grid-cols-[1fr_110px_1fr_auto] gap-2 max-sm:grid-cols-2">
                  <div>
                    <Input
                      mono
                      aria-label={`Metadata key of condition ${i + 1}`}
                      placeholder="metadata key, e.g. tier"
                      value={c.key}
                      onChange={(e) => update(i, { key: e.target.value })}
                    />
                    <FieldError error={errors[`conditions.${i}.key`]} />
                  </div>
                  <p className="col-span-2 self-center text-caption text-muted">
                    From the request’s x-antirion-metadata header.
                  </p>
                </div>
              ) : null}
              {c.kind === 'guardrail' ? (
                <div className="grid grid-cols-[1fr_110px_1fr_auto] gap-2 max-sm:grid-cols-2">
                  <div>
                    <Combobox
                      aria-label={`Guardrail of condition ${i + 1}`}
                      value={c.key}
                      placeholder="Choose a guardrail"
                      onChange={(v) => update(i, { key: v })}
                      options={guardrails.map((g) => ({ value: g, label: g }))}
                    />
                    <FieldError error={errors[`conditions.${i}.key`]} />
                  </div>
                  <Select
                    aria-label={`Guardrail result of condition ${i + 1}`}
                    value={c.guardField}
                    onChange={(v) => update(i, { guardField: v })}
                    options={GUARD_FIELDS}
                  />
                  <p className="self-center text-caption text-muted">Runs only when this rule is checked.</p>
                </div>
              ) : null}
            </div>
          ))}
          <Button
            size="sm"
            icon="plus"
            className="self-start"
            onClick={() => set('conditions', [...draft.conditions, emptyCondition()])}
          >
            Add condition
          </Button>
        </fieldset>
      )}
      <div className="max-w-md">
        <Field
          label="Share of matching requests, in percent"
          optional
          error={errors.sample}
          hint="Take only part of the requests that match, e.g. 10 for one in ten. The same request always gets the same answer."
        >
          <NumberInput min={0} max={100} step={1} value={draft.sample} onChange={(v) => set('sample', v)} />
        </Field>
      </div>
      <p className="-mt-2 text-caption text-muted">
        With no conditions, the rule matches every request. Rules are checked from top to bottom; the first
        match wins.
      </p>
      <Field label="Note" optional>
        <Input value={draft.description} onChange={(e) => set('description', e.target.value)} />
      </Field>
      <Checkbox
        label="Turn the rule on when saved"
        checked={draft.enabled}
        onChange={(v) => set('enabled', v)}
      />
    </div>
  );
}

/** Step 2: the strategy. */
export function StrategyStep({ draft, set, locked }: { draft: RuleDraft; set: Set; locked: string | null }) {
  if (locked)
    return (
      <div className="rounded-control border border-line bg-sunken px-3 py-2.5 text-ui">
        <p className="font-semibold text-fg">{locked}</p>
        <p className="mt-1 text-caption text-muted">
          The wizard doesn’t build this kind of target, so it is kept as written. Its timeouts and attempts
          can still be changed in the review step.
        </p>
      </div>
    );
  return (
    <RadioGroup
      label="Strategy"
      variant="cards"
      value={draft.strategy}
      onChange={(v) => {
        set('strategy', v);
        if (v === 'adaptive' && draft.targets.length < 2)
          set('targets', [
            draft.targets[0] ?? { deployment: '', weight: 1, role: '' },
            { deployment: '', weight: 1, role: '' },
          ]);
        if (v === 'canary' && draft.targets.length < 2)
          set('targets', [
            {
              ...(draft.targets[0] ?? { deployment: '', weight: 1 }),
              role: draft.targets[0]?.role || 'stable',
            },
            { deployment: '', weight: 1, role: 'canary' },
          ]);
      }}
      options={STRATEGIES.map((s) => ({ value: s.value, label: s.label, description: s.description }))}
    />
  );
}

function DeploymentPicker({
  label,
  value,
  onChange,
  deployments,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  deployments: DeploymentResource[];
}) {
  return (
    <Combobox
      aria-label={label}
      value={value}
      placeholder="Select a deployment"
      onChange={onChange}
      options={deployments.map((d) => ({
        value: d.name,
        label: `${d.name} · ${d.spec.provider}/${d.spec.model}`,
        icon: <ModelMark model={null} upstream={[d.spec.base_model, d.spec.model]} size="md" />,
      }))}
    />
  );
}

function Legend({ children }: { children: ReactNode }) {
  return <legend className="mb-2 text-ui font-medium text-fg">{children}</legend>;
}

/** Step 3: targets with weights and role labels, and each strategy's own settings. */
export function TargetsStep({
  draft,
  set,
  errors,
  deployments,
  tiers,
  locked,
}: {
  draft: RuleDraft;
  set: Set;
  errors: Errors;
  deployments: DeploymentResource[];
  tiers: Tier[];
  locked: string | null;
}) {
  const s = draft.strategy;
  const setTarget = (i: number, patch: Partial<Target>) =>
    set(
      'targets',
      draft.targets.map((x, j) => (j === i ? { ...x, ...patch } : x)),
    );

  if (locked)
    return (
      <div className="rounded-control border border-line bg-sunken px-3 py-2.5 text-ui">
        <p className="text-fg">{locked}</p>
        <p className="mt-1 text-caption text-muted">
          These targets include nested rules or settings the wizard doesn’t edit; they are kept as written.
        </p>
      </div>
    );

  if (s === 'block') return <BlockFields draft={draft} set={set} />;

  if (s === 'cost_quality')
    return <CostQualityFields draft={draft} set={set} errors={errors} deployments={deployments} />;

  if (s === 'tier')
    return (
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Tier" error={errors.tier} hint="Models join and leave the tier on their own.">
          <Select
            value={draft.tier.tier}
            onChange={(v) => set('tier', { ...draft.tier, tier: v })}
            options={tiers.map((t) => ({
              value: t.name,
              label: `${tierLabel(t.name)} · ${t.models} models`,
            }))}
          />
        </Field>
        <Field label="Pick">
          <Select
            value={draft.tier.strategy}
            onChange={(v) => set('tier', { ...draft.tier, strategy: v as RuleDraft['tier']['strategy'] })}
            options={[
              { value: 'least_cost', label: 'The cheapest model' },
              { value: 'least_latency', label: 'The fastest model' },
              { value: 'weighted', label: 'Each model in turn' },
              { value: 'fallback', label: 'The first model, then the next' },
            ]}
          />
        </Field>
        <Checkbox
          className="sm:col-span-2"
          label="Include preview models"
          checked={draft.tier.includePreview}
          onChange={(v) => set('tier', { ...draft.tier, includePreview: v })}
        />
        <Triggers draft={draft} set={set} />
      </div>
    );

  if (s === 'shadow')
    return (
      <div className="flex flex-col gap-4">
        <Field label="Answer from" error={errors.targets}>
          <DeploymentPicker
            label="Primary deployment"
            value={draft.targets[0]?.deployment ?? ''}
            onChange={(v) =>
              set('targets', [{ deployment: v, weight: 1, role: draft.targets[0]?.role ?? '' }])
            }
            deployments={deployments}
          />
        </Field>
        <Field
          label="Copy to"
          error={errors['shadow.target']}
          hint="Its answers are compared and thrown away."
        >
          <DeploymentPicker
            label="Shadow deployment"
            value={draft.shadow.target}
            onChange={(v) => set('shadow', { ...draft.shadow, target: v })}
            deployments={deployments}
          />
        </Field>
        <div className="grid gap-4 sm:grid-cols-2">
          <Field
            label="Share of requests copied"
            error={errors['shadow.percent']}
            hint="Percent of matching requests."
          >
            <NumberInput
              min={0}
              max={100}
              className="w-28"
              value={draft.shadow.percent}
              onChange={(v) => set('shadow', { ...draft.shadow, percent: v })}
            />
          </Field>
          <Field label="Choose copies by">
            <Segmented<'request' | 'session'>
              label="Choose copies by"
              value={draft.shadow.sampleBy}
              onChange={(v) => set('shadow', { ...draft.shadow, sampleBy: v })}
              options={[
                { value: 'request', label: 'Request' },
                { value: 'session', label: 'Session' },
              ]}
            />
          </Field>
        </div>
        <p className="text-caption text-muted">
          Copies are never retried and never change the answer; their spend counts toward gateway and project
          budgets.
        </p>
      </div>
    );

  if (s === 'canary')
    return (
      <div className="flex flex-col gap-4">
        <fieldset className="flex flex-col gap-2">
          <Legend>Deployments</Legend>
          {[0, 1].map((i) => (
            <div key={i} className="grid grid-cols-[110px_1fr_120px] items-center gap-2 max-sm:grid-cols-1">
              <span className="text-ui text-muted">{i === 0 ? 'Stable' : 'Candidate'}</span>
              <DeploymentPicker
                label={i === 0 ? 'Stable deployment' : 'Candidate deployment'}
                value={draft.targets[i]?.deployment ?? ''}
                onChange={(v) => setTarget(i, { deployment: v })}
                deployments={deployments}
              />
              <Input
                mono
                aria-label={i === 0 ? 'Role of the stable deployment' : 'Role of the candidate'}
                value={draft.targets[i]?.role ?? ''}
                placeholder={i === 0 ? 'stable' : 'canary'}
                onChange={(e) => setTarget(i, { role: e.target.value })}
              />
            </div>
          ))}
          <FieldError error={errors.targets ?? errors.roles} />
        </fieldset>
        <fieldset className="flex flex-col gap-2">
          <Legend>Ramp</Legend>
          <p className="-mt-1 text-caption text-muted">
            The candidate’s share of traffic, from when the rule is saved. Saving the rule again restarts the
            ramp.
          </p>
          {draft.canary.steps.map((step, i) => (
            <div
              key={i}
              className="grid grid-cols-[90px_max-content_110px_auto] items-center justify-start gap-2"
            >
              <NumberInput
                aria-label={`Share at step ${i + 1}, percent`}
                min={0}
                max={100}
                value={step.weight}
                onChange={(v) =>
                  set('canary', {
                    ...draft.canary,
                    steps: draft.canary.steps.map((x, j) => (j === i ? { ...x, weight: v } : x)),
                  })
                }
              />
              <span className="text-ui text-muted">% of traffic, after</span>
              <Input
                mono
                aria-label={`Time before step ${i + 1}`}
                value={step.after}
                onChange={(e) =>
                  set('canary', {
                    ...draft.canary,
                    steps: draft.canary.steps.map((x, j) => (j === i ? { ...x, after: e.target.value } : x)),
                  })
                }
              />
              <IconButton
                icon="x"
                label={`Remove step ${i + 1}`}
                disabled={draft.canary.steps.length <= 1}
                onClick={() =>
                  set('canary', { ...draft.canary, steps: draft.canary.steps.filter((_, j) => j !== i) })
                }
              />
            </div>
          ))}
          <FieldError error={errors['canary.steps']} />
          <Button
            size="sm"
            icon="plus"
            className="self-start"
            disabled={draft.canary.steps.length >= 20}
            onClick={() =>
              set('canary', {
                ...draft.canary,
                steps: [...draft.canary.steps, { weight: '100', after: '24h' }],
              })
            }
          >
            Add step
          </Button>
        </fieldset>
        <fieldset className="flex flex-col gap-3">
          <Legend>Error budget</Legend>
          <Checkbox
            label="Watch the candidate’s errors"
            description="Needs at least 20 requests in the window."
            checked={draft.canary.budget}
            onChange={(v) => set('canary', { ...draft.canary, budget: v })}
          />
          {draft.canary.budget ? (
            <div className="grid gap-4 sm:grid-cols-3">
              <Field
                label="Errors above"
                error={errors['canary.maxErrorRate']}
                hint="Percent of its requests."
              >
                <NumberInput
                  min={0}
                  max={100}
                  step={0.5}
                  value={draft.canary.maxErrorRate}
                  onChange={(v) => set('canary', { ...draft.canary, maxErrorRate: v })}
                />
              </Field>
              <Field label="Over" error={errors['canary.window']} hint="30s to 1h.">
                <Input
                  mono
                  value={draft.canary.window}
                  onChange={(e) => set('canary', { ...draft.canary, window: e.target.value })}
                />
              </Field>
              <Field label="Then">
                <Select
                  value={draft.canary.action}
                  onChange={(v) => set('canary', { ...draft.canary, action: v as 'rollback' | 'hold' })}
                  options={[
                    { value: 'rollback', label: 'Roll back' },
                    { value: 'hold', label: 'Hold the ramp' },
                  ]}
                />
              </Field>
            </div>
          ) : null}
        </fieldset>
        <Sticky draft={draft} set={set} errors={errors} />
        <Triggers draft={draft} set={set} />
      </div>
    );

  return (
    <div className="flex flex-col gap-4">
      <fieldset className="flex flex-col gap-2">
        <Legend>
          {s === 'fallback'
            ? 'Deployments, in the order to try them'
            : s === 'weighted'
              ? 'Deployments and their share of traffic'
              : s === 'adaptive'
                ? 'Deployments to learn between'
                : s === 'pool'
                  ? 'Replicas of the model server'
                  : 'Deployments'}
        </Legend>
        {draft.targets.map((t, i) => (
          <div
            key={i}
            className="grid grid-cols-[1fr_90px_120px_auto] items-center gap-2 max-sm:grid-cols-[1fr_auto]"
          >
            <DeploymentPicker
              label={`Deployment ${i + 1}`}
              value={t.deployment}
              onChange={(v) => setTarget(i, { deployment: v })}
              deployments={deployments}
            />
            {s === 'weighted' ? (
              <NumberInput
                aria-label={`Weight of target ${i + 1}`}
                min={0}
                max={1000}
                value={String(t.weight)}
                onChange={(v) => setTarget(i, { weight: Number(v.replace(/\D/g, '')) || 0 })}
              />
            ) : (
              <span className="max-sm:hidden" />
            )}
            <Input
              mono
              aria-label={`Role of target ${i + 1}`}
              placeholder={i === 0 ? 'primary' : 'backup'}
              value={t.role}
              onChange={(e) => setTarget(i, { role: e.target.value })}
              className="max-sm:hidden"
            />
            <IconButton
              icon="x"
              label={`Remove deployment ${i + 1}`}
              onClick={() =>
                set(
                  'targets',
                  draft.targets.filter((_, j) => j !== i),
                )
              }
            />
          </div>
        ))}
        <FieldError error={errors.targets ?? errors.roles} />
        {s !== 'single' || draft.targets.length === 0 ? (
          <Button
            size="sm"
            icon="plus"
            className="self-start"
            onClick={() => set('targets', [...draft.targets, { deployment: '', weight: 1, role: '' }])}
          >
            Add deployment
          </Button>
        ) : null}
        <p className="text-caption text-muted">
          A role is an optional label such as primary or backup; it names the deployment in the request log’s
          route path.
        </p>
      </fieldset>
      {s === 'adaptive' ? <AdaptiveFields draft={draft} set={set} errors={errors} /> : null}
      {s === 'pool' ? <PoolFields draft={draft} set={set} errors={errors} /> : null}
      {canStick(s) ? <Sticky draft={draft} set={set} errors={errors} /> : null}
      {movesOn(s) ? <Triggers draft={draft} set={set} /> : null}
    </div>
  );
}

function Sticky({ draft, set, errors }: { draft: RuleDraft; set: Set; errors: Errors }) {
  const kind = draft.sticky.kind;
  return (
    <div className="grid gap-4 sm:grid-cols-2">
      <Field
        label="Keep each session on one deployment"
        hint="The same value always gets the same deployment while it stays healthy."
      >
        <Select
          value={kind}
          onChange={(v) => set('sticky', { ...draft.sticky, kind: v as StickyKind })}
          options={[
            { value: '', label: 'No, split every request' },
            { value: 'session', label: 'By session' },
            { value: 'end_user', label: 'By end user' },
            { value: 'key', label: 'By API key' },
            { value: 'header', label: 'By a header' },
            { value: 'metadata', label: 'By a metadata key' },
          ]}
        />
      </Field>
      {kind === 'header' || kind === 'metadata' ? (
        <Field label={kind === 'header' ? 'Header name' : 'Metadata key'} error={errors['sticky.name']}>
          <Input
            mono
            value={draft.sticky.name}
            onChange={(e) => set('sticky', { ...draft.sticky, name: e.target.value })}
          />
        </Field>
      ) : null}
    </div>
  );
}

function Triggers({ draft, set }: { draft: RuleDraft; set: Set }) {
  return (
    <fieldset className="flex flex-col gap-2 sm:col-span-2">
      <Legend>Move to another deployment after</Legend>
      <div className="flex flex-wrap gap-x-5 gap-y-2">
        {TRIGGERS.map((t) => (
          <Checkbox
            key={t.value}
            label={t.label}
            checked={draft.triggers[t.value]}
            onChange={(v) => set('triggers', { ...draft.triggers, [t.value]: v })}
          />
        ))}
      </div>
      <p className="text-caption text-muted">
        Unchecked, the gateway retries the same deployment instead and then returns the error.
      </p>
    </fieldset>
  );
}

function BlockFields({ draft, set }: { draft: RuleDraft; set: Set }) {
  return (
    <div className="grid gap-4 sm:grid-cols-2">
      <Field label="Answer with" hint="The error apps receive.">
        <Select
          value={String(draft.block.status)}
          onChange={(v) => set('block', { ...draft.block, status: Number(v) as 400 | 403 | 451 })}
          options={[
            { value: '403', label: 'Forbidden (403)' },
            { value: '400', label: 'Bad request (400)' },
            { value: '451', label: 'Unavailable for legal reasons (451)' },
          ]}
        />
      </Field>
      <Field label="Category" hint="How the block is filed on the Safety & PII screen.">
        <Select
          value={draft.block.category}
          onChange={(v) => set('block', { ...draft.block, category: v })}
          options={[
            { value: 'custom', label: 'Custom' },
            { value: 'pii', label: 'Personal data' },
            { value: 'secret', label: 'Secret' },
            { value: 'prompt_injection', label: 'Prompt injection' },
            { value: 'off_topic', label: 'Off topic' },
          ]}
        />
      </Field>
      <Field label="Message" className="sm:col-span-2" hint="Shown to the app in the error.">
        <Input
          value={draft.block.message}
          onChange={(e) => set('block', { ...draft.block, message: e.target.value })}
        />
      </Field>
    </div>
  );
}

/** Review step: the rule's own timeouts and attempts. */
export function LimitsFields({ draft, set, errors }: { draft: RuleDraft; set: Set; errors: Errors }) {
  if (draft.strategy === 'block') return null;
  return (
    <fieldset className="flex flex-col gap-2">
      <Legend>Timeouts and retries for this rule</Legend>
      <div className="grid gap-4 sm:grid-cols-3">
        <Field
          label="Attempt timeout"
          optional
          error={errors['limits.attempt']}
          hint="e.g. 30s; one call to a deployment."
        >
          <Input
            mono
            value={draft.limits.attempt}
            placeholder="default"
            onChange={(e) => set('limits', { ...draft.limits, attempt: e.target.value })}
          />
        </Field>
        <Field
          label="Time limit"
          optional
          error={errors['limits.deadline']}
          hint="The whole request, retries included."
        >
          <Input
            mono
            value={draft.limits.deadline}
            placeholder="default"
            onChange={(e) => set('limits', { ...draft.limits, deadline: e.target.value })}
          />
        </Field>
        <Field
          label="Attempts at most"
          optional
          error={errors['limits.maxAttempts']}
          hint="1 to 10, retries and fallbacks included."
        >
          <NumberInput
            min={1}
            max={10}
            value={draft.limits.maxAttempts}
            onChange={(v) => set('limits', { ...draft.limits, maxAttempts: v })}
          />
        </Field>
      </div>
      <p className="text-caption text-muted">
        Empty fields use the gateway’s defaults from the Health and defaults tab.
      </p>
      {movesOn(draft.strategy) ? (
        <div className="mt-3 flex flex-col gap-5">
          <HedgingFields value={draft.hedging} onChange={(v) => set('hedging', v)} errors={errors} />
          <ResponseChecks value={draft.predicates} onChange={(v) => set('predicates', v)} errors={errors} />
        </div>
      ) : null}
    </fieldset>
  );
}
