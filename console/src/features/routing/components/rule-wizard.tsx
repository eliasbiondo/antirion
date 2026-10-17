'use client';

import { useState } from 'react';
import { z } from 'zod';

import { Button } from '@/components/ui/button';
import { BarList } from '@/components/ui/charts';
import { Field } from '@/components/ui/form';
import { Drawer } from '@/components/ui/overlay';
import { ModelMark } from '@/components/ui/marks';
import { Combobox, Select } from '@/components/ui/select';
import { ErrorState, InlineError } from '@/components/ui/states';
import { isChangeControl } from '@/lib/api/client';
import { cn } from '@/lib/cn';
import { formatCount, plural } from '@/lib/format';
import { notify } from '@/stores/ui';

import {
  conditionEditable,
  describeDraft,
  draftFrom,
  movesOn,
  type RuleDraft,
  targetsLocked,
  toRule,
} from '../draft';
import { durationMs, validateExtras } from '../resilience';
import { useRouting } from '../store';
import { describePredicate, type Json, nodeTypeLabel, type Rule, strategyOf, withRules } from '../types';
import { ConditionsStep, LimitsFields, StrategyStep, TargetsStep } from './wizard-steps';

const STEPS = ['Conditions', 'Strategy', 'Targets', 'Review'] as const;

const NEW_ROUTE = 'global';

const DURATION = /^[0-9]+(\.[0-9]+)?(ms|s|m|h|d)$/;
const ROLE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const durationOrEmpty = z
  .string()
  .trim()
  .refine((v) => v === '' || DURATION.test(v), 'Use a number and a unit, e.g. 30s, 500ms or 2m');
const percentOrEmpty = z
  .string()
  .trim()
  .refine((v) => v === '' || (Number(v) >= 0 && Number(v) <= 100), 'Enter a percent from 0 to 100');

/** Errors of a draft, keyed by field path; the step a key belongs to is found from its prefix. */
function validate(d: RuleDraft, locked: { when: boolean; targets: boolean }): Record<string, string> {
  const out: Record<string, string> = {};
  const name = d.name.trim();
  if (!name) out.name = 'Name the rule, e.g. support-to-fast-models';
  else if (name.length > 100) out.name = 'Use at most 100 characters';
  if (!locked.when) {
    d.conditions.forEach((c, i) => {
      if (!c.value.trim()) out[`conditions.${i}.value`] = 'Enter a value, or remove the condition';
      if (c.kind === 'metadata' && !/^[^\s.][^\s]*$/.test(c.key.trim()))
        out[`conditions.${i}.key`] = 'Enter the metadata key';
      if (c.kind === 'guardrail' && !c.key) out[`conditions.${i}.key`] = 'Choose a guardrail';
    });
    const sample = percentOrEmpty.safeParse(d.sample);
    if (!sample.success) out.sample = sample.error.issues[0]?.message ?? 'Invalid';
  }
  if (!locked.targets) {
    const picked = d.targets.filter((t) => t.deployment);
    const needs: Record<string, number> = {
      single: 1,
      weighted: 1,
      fallback: 1,
      least_busy: 1,
      least_latency: 1,
      shadow: 1,
      pool: 1,
    };
    if ((needs[d.strategy] ?? 0) > picked.length) out.targets = 'Choose at least one deployment';
    if (d.strategy === 'canary') {
      if (!d.targets[0]?.deployment || !d.targets[1]?.deployment)
        out.targets = 'Choose the stable deployment and the candidate';
      else if (d.targets[0].deployment === d.targets[1].deployment)
        out.targets = 'The candidate must differ from the stable deployment';
      const steps = d.canary.steps.filter((s) => s.weight.trim() !== '');
      if (steps.length === 0) out['canary.steps'] = 'Add at least one step';
      else if (steps.some((s) => !/^\d+$/.test(s.weight.trim()) || Number(s.weight) > 100))
        out['canary.steps'] = 'Each share is a whole percent from 0 to 100';
      else if (steps.some((s) => !DURATION.test(s.after.trim() || '0s')))
        out['canary.steps'] = 'Write each time as a number and a unit, e.g. 1h';
      if (d.canary.budget) {
        if (!(Number(d.canary.maxErrorRate) > 0 && Number(d.canary.maxErrorRate) <= 100))
          out['canary.maxErrorRate'] = 'Enter a percent above 0';
        if (!DURATION.test(d.canary.window.trim())) out['canary.window'] = 'e.g. 5m';
      }
    }
    if (d.strategy === 'shadow') {
      if (!d.shadow.target) out['shadow.target'] = 'Choose the deployment that receives the copies';
      else if (d.shadow.target === d.targets[0]?.deployment)
        out['shadow.target'] = 'Copy to a different deployment';
      if (!(Number(d.shadow.percent) > 0 && Number(d.shadow.percent) <= 100))
        out['shadow.percent'] = 'Enter a percent above 0';
    }
    if (d.strategy === 'tier' && !d.tier.tier) out.tier = 'Choose a tier';
    if (d.strategy === 'pool' && d.pool.picker === 'external') {
      if (!/^https?:\/\/\S+$/.test(d.pool.url.trim()))
        out['pool.url'] = 'The picker’s address, e.g. http://picker.internal:9002';
      const t = d.pool.timeout.trim();
      const ms = t ? durationMs(t) : 300;
      if (ms === null || ms < 10 || ms > 30_000) out['pool.timeout'] = 'From 10ms to 30s, e.g. 300ms';
    }
    if (d.strategy === 'cost_quality') {
      const c = d.costQuality;
      if (!c.strong) out['costQuality.strong'] = 'Choose the strong deployment';
      if (!c.weak) out['costQuality.weak'] = 'Choose the weak deployment';
      else if (c.weak === c.strong) out['costQuality.weak'] = 'Choose a different deployment';
      if (!(c.percent.trim() !== '' && Number(c.percent) >= 0 && Number(c.percent) <= 100))
        out['costQuality.percent'] = 'A percent from 0 to 100';
      if (c.scorer === 'matrix_factorization') {
        if (!c.modelFile.trim()) out['costQuality.modelFile'] = 'The path of the trained model file';
        if (!c.embeddingModel.trim()) out['costQuality.embeddingModel'] = 'The embeddings Model to use';
        if (!c.strongName.trim()) out['costQuality.strongName'] = 'As the file names it';
        if (!c.weakName.trim()) out['costQuality.weakName'] = 'As the file names it';
      }
    }
    if (d.strategy === 'adaptive') {
      if (picked.length < 2) out.targets = 'Choose at least two deployments to learn between';
      if (!/^\S{1,128}$/.test(d.adaptive.experiment.trim()))
        out['adaptive.experiment'] = 'Name the experiment, e.g. support-model-trial';
      if (!d.adaptive.metric.trim()) out['adaptive.metric'] = 'Choose the feedback metric it learns from';
      const samples = d.adaptive.minSamples.trim();
      if (samples && !(/^\d+$/.test(samples) && Number(samples) >= 1))
        out['adaptive.minSamples'] = 'A whole number of at least 1';
      if (d.adaptive.stop && !(Number(d.adaptive.delta) > 0 && Number(d.adaptive.delta) <= 50))
        out['adaptive.delta'] = 'A percent above 0 and at most 50';
    }
    const roles = d.targets.map((t) => t.role.trim()).filter(Boolean);
    if (roles.some((r) => !ROLE.test(r)))
      out.roles = 'Roles use lowercase letters, digits, - and _, e.g. primary';
    else if (new Set(roles).size !== roles.length) out.roles = 'Give each deployment a different role';
    if (
      (d.strategy === 'weighted' || d.strategy === 'canary') &&
      (d.sticky.kind === 'header' || d.sticky.kind === 'metadata') &&
      !d.sticky.name.trim()
    )
      out['sticky.name'] = d.sticky.kind === 'header' ? 'Enter the header name' : 'Enter the metadata key';
  }
  for (const key of ['attempt', 'deadline'] as const) {
    const r = durationOrEmpty.safeParse(d.limits[key]);
    if (!r.success) out[`limits.${key}`] = r.error.issues[0]?.message ?? 'Invalid';
  }
  if (movesOn(d.strategy)) Object.assign(out, validateExtras(d.hedging, d.predicates, ''));
  const attempts = d.limits.maxAttempts.trim();
  if (attempts && !(/^\d+$/.test(attempts) && Number(attempts) >= 1 && Number(attempts) <= 10))
    out['limits.maxAttempts'] = 'From 1 to 10';
  return out;
}

/**
 * The steps before `to` that still have errors, as the errors to show and the first such step;
 * null when the wizard may move on.
 */
export function blockingStep(
  errors: Record<string, string>,
  to: number,
): { step: number; errors: Record<string, string> } | null {
  const before = Object.entries(errors).filter(([k]) => stepOf(k) < to);
  if (before.length === 0) return null;
  return { step: Math.min(...before.map(([k]) => stepOf(k))), errors: Object.fromEntries(before) };
}

function stepOf(key: string): number {
  if (key === 'name' || key === 'sample' || key.startsWith('conditions')) return 0;
  if (key.startsWith('limits') || key.startsWith('hedging') || key.startsWith('predicates')) return 3;
  return 2;
}

export interface RuleWizardProps {
  /** The shared route the rule list lives in; null on a gateway without one, which this creates. */
  route: string | null;
  tree: Json;
  rules: Json[];
  editing: Rule | null;
  onClose: () => void;
}

/** The four-step rule drawer: conditions, strategy, targets, then a review with limits and a dry run. */
export function RuleWizard({ route, tree, rules, editing, onClose }: RuleWizardProps) {
  const deployments = useRouting((s) => s.deployments.data) ?? [];
  const tiers = useRouting((s) => s.tiers.data) ?? [];
  const guardrails = useRouting((s) => s.guardrails.data) ?? [];
  const saveTree = useRouting((s) => s.saveTree);
  const createRoute = useRouting((s) => s.createRoute);
  const simulate = useRouting((s) => s.simulate);
  const simulation = useRouting((s) => s.simulation);
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState<RuleDraft>(() => draftFrom(editing));
  const [initial] = useState<RuleDraft>(() => draftFrom(editing));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [hours, setHours] = useState('24');
  const [otherwise, setOtherwise] = useState('');

  const whenLocked = !conditionEditable(editing);
  const lockedTargets = targetsLocked(draft);
  const strategyLocked =
    editing && strategyOf(editing.target) === 'other' ? nodeTypeLabel(editing.target) : null;

  const nextRules = () => {
    const rule = toRule(draft, whenLocked ? (editing?.when ?? undefined) : undefined);
    if (editing) return rules.map((r, i) => (i === editing.index ? rule : r));
    return [...rules, rule];
  };

  function check(): boolean {
    const out = validate(draft, { when: whenLocked, targets: lockedTargets });
    setErrors(out);
    const keys = Object.keys(out);
    if (keys.length === 0) return true;
    setStep(Math.min(...keys.map(stepOf)));
    return false;
  }

  /** Moves to step `to`, unless an earlier step still has errors: it then shows that step's. */
  function go(to: number) {
    if (to <= step) {
      setStep(to);
      return;
    }
    const blocked = blockingStep(validate(draft, { when: whenLocked, targets: lockedTargets }), to);
    setErrors(blocked?.errors ?? {});
    setStep(blocked ? blocked.step : to);
  }

  async function save() {
    if (!check()) return;
    setBusy(true);
    setError(null);
    try {
      if (route === null)
        await createRoute(NEW_ROUTE, {
          type: 'conditional',
          rules: nextRules(),
          ...(otherwise ? { default: otherwise } : {}),
        });
      else await saveTree(route, withRules(tree, nextRules()));
      notify(
        'success',
        editing ? 'Rule saved' : 'Rule created',
        route === null ? `${draft.name.trim()}, in the new shared route ${NEW_ROUTE}` : draft.name.trim(),
      );
      onClose();
    } catch (e) {
      // Change control: the shell offers or shows the change request, so the wizard is done.
      if (isChangeControl(e)) onClose();
      else setError(e);
    } finally {
      setBusy(false);
    }
  }

  const set = <K extends keyof RuleDraft>(key: K, value: RuleDraft[K]) =>
    setDraft((d) => ({ ...d, [key]: value }));
  const preview =
    whenLocked && editing
      ? `When ${describePredicate(editing.when)} → ${nodeTypeLabel(editing.target)}`
      : describeDraft(draft);

  return (
    <Drawer
      open
      onClose={onClose}
      dirty={JSON.stringify(draft) !== JSON.stringify(initial)}
      width="lg"
      title={editing ? `Edit rule ${editing.name}` : 'Create rule'}
      subtitle={preview}
      footer={
        <>
          {step > 0 ? (
            <Button variant="ghost" onClick={() => setStep(step - 1)}>
              Back
            </Button>
          ) : null}
          <span className="flex-1" />
          {step < STEPS.length - 1 ? (
            <Button variant="primary" onClick={() => go(step + 1)}>
              Next: {STEPS[step + 1]?.toLowerCase()}
            </Button>
          ) : (
            <Button variant="primary" busy={busy} onClick={() => void save()}>
              {editing ? 'Save rule' : 'Create rule'}
            </Button>
          )}
        </>
      }
    >
      <ol className="mb-5 flex gap-1" aria-label="Steps">
        {STEPS.map((s, i) => (
          <li key={s} className="flex-1">
            <button
              type="button"
              aria-current={i === step ? 'step' : undefined}
              onClick={() => go(i)}
              className={cn(
                'w-full border-b-2 pb-1.5 text-left text-ui',
                i === step
                  ? 'border-accent font-semibold text-fg'
                  : i < step
                    ? 'border-success text-muted'
                    : 'border-line text-dim',
              )}
            >
              <span className="mono mr-1.5 text-caption">{i + 1}</span>
              {s}
            </button>
          </li>
        ))}
      </ol>
      <InlineError error={error} />
      {step === 0 ? (
        <ConditionsStep
          draft={draft}
          set={set}
          errors={errors}
          guardrails={guardrails}
          locked={whenLocked && editing ? describePredicate(editing.when) : null}
        />
      ) : null}
      {step === 1 ? <StrategyStep draft={draft} set={set} locked={strategyLocked} /> : null}
      {step === 2 ? (
        <TargetsStep
          draft={draft}
          set={set}
          errors={errors}
          deployments={deployments}
          tiers={tiers}
          locked={lockedTargets && editing ? `${nodeTypeLabel(editing.target)} target` : null}
        />
      ) : null}
      {step === 3 ? (
        <div className="flex flex-col gap-5">
          <p className="rounded-control border border-line bg-sunken px-3 py-2.5 text-body text-fg">
            {preview}
          </p>
          {route === null ? (
            <p className="text-ui text-muted">
              This first rule also creates the gateway&apos;s shared route, named{' '}
              <span className="mono text-fg">{NEW_ROUTE}</span>: it holds every rule, and the rules you add
              later go into it.
            </p>
          ) : null}
          <LimitsFields draft={draft} set={set} errors={errors} />
          {route === null ? (
            <Field
              label="Everything else"
              hint="Where requests no rule matches go. Without one, they get an error."
            >
              <Combobox
                value={otherwise}
                onChange={(v) => setOtherwise(v)}
                emptyOption="Answer with an error"
                options={deployments.map((d) => ({
                  value: d.name,
                  label: `${d.name} · ${d.spec.provider}/${d.spec.model}`,
                  icon: <ModelMark model={null} upstream={[d.spec.base_model, d.spec.model]} size="md" />,
                }))}
              />
            </Field>
          ) : (
            <>
              <div className="flex flex-wrap items-end gap-2">
                <Field label="Try it on past requests from the last">
                  <Select
                    value={hours}
                    onChange={(v) => setHours(v)}
                    options={[
                      { value: '1', label: '1 hour' },
                      { value: '6', label: '6 hours' },
                      { value: '24', label: '24 hours' },
                      { value: '72', label: '3 days' },
                    ]}
                  />
                </Field>
                <Button
                  icon="play"
                  busy={simulation.status === 'loading'}
                  onClick={() => void simulate(route, withRules(tree, nextRules()), Number(hours))}
                >
                  Run dry run
                </Button>
              </div>
              <p className="-mt-3 text-caption text-muted">
                Nothing changes: the gateway replays recent requests through the rules as they would be.
              </p>
            </>
          )}
          {simulation.error ? <ErrorState error={simulation.error} subject="The dry run" size="sm" /> : null}
          {simulation.data ? (
            <div className="flex flex-col gap-3 text-ui">
              <p className="text-fg">
                {plural(simulation.data.evaluated, 'recent request')} checked
                {simulation.data.blocked > 0 ? `, ${formatCount(simulation.data.blocked)} blocked` : ''}
                {simulation.data.no_route > 0
                  ? `, ${formatCount(simulation.data.no_route)} with no route`
                  : ''}
                .
              </p>
              <BarList
                label="Requests each rule would take"
                items={simulation.data.matched_by_rule.map((m) => ({
                  name: m.name ?? 'Everything else',
                  value: m.count,
                }))}
                format={(v) => formatCount(v)}
              />
              {simulation.data.matched_by_rule.some((m) => (m.unknown ?? 0) > 0) ? (
                <p className="text-caption text-muted">
                  Some requests couldn’t be checked because the log doesn’t keep their content.
                </p>
              ) : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </Drawer>
  );
}
