'use client';

import { useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button, IconButton } from '@/components/ui/button';
import { Combobox, Field, Segmented } from '@/components/ui/form';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { Banner, EmptyState, InlineError } from '@/components/ui/states';
import { cn } from '@/lib/cn';
import { useEdited } from '@/lib/hooks';
import { notify } from '@/stores/ui';

import { useDeploymentMap, useRouteMap } from '../hooks';
import { saveModelSpec, useCatalog } from '../store';
import { describeNode, moveStep, newStep, type Step, type StepKind, stepsOf, withChain } from '../tree';
import { healthName, type ModelView, tierLabel } from '../types';

const KIND_LABEL: Record<StepKind, string> = {
  deployment: 'Deployment',
  route: 'Shared route',
  tier: 'Tier',
  subtree: 'Sub-tree',
};

/**
 * Builds a Model's fallback chain: the positions of its route tree tried in order, each a deployment,
 * a shared route (a sub-tree) or a tier; a step that is a whole sub-tree is kept as it is and can be
 * moved or removed. One step is the target itself; several are a fallback node.
 */
export function FallbackEditor({ view, onClose }: { view: ModelView; onClose: () => void }) {
  const deployments = useDeploymentMap();
  const routes = useRouteMap();
  const tiers = useCatalog((s) => s.tiers.data ?? []);
  const health = useCatalog((s) => s.health.data);
  const spec = view.model.spec;
  const [steps, setSteps] = useState<Step[]>(() => stepsOf(spec));
  const [kind, setKind] = useState<Exclude<StepKind, 'subtree'>>('deployment');
  const [value, setValue] = useState('');
  const [error, setError] = useState<unknown>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(steps);

  const options =
    kind === 'deployment'
      ? [...deployments.values()].map((d) => ({
          value: d.name,
          label: d.name,
          description: `${d.spec.provider} · ${d.spec.model}`,
        }))
      : kind === 'route'
        ? [...routes.keys()].map((r) => ({ value: r, label: r }))
        : tiers.map((t) => ({ value: t.name, label: tierLabel(t.name), description: t.description }));
  const used = new Set(steps.filter((s) => s.kind === kind).map((s) => s.value));

  function add() {
    if (!value) {
      setProblem(`Select a ${KIND_LABEL[kind].toLowerCase()} to add`);
      return;
    }
    if (used.has(value)) {
      setProblem('This step is already in the chain');
      return;
    }
    setProblem(null);
    setSteps((s) => [...s, newStep(kind, value)]);
    setValue('');
  }

  async function save() {
    if (steps.length === 0) {
      setProblem('Keep at least one step: a model with nothing to try can’t serve requests');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await saveModelSpec(view.name, withChain(spec, steps));
      notify('success', 'Fallback chain saved', view.name);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const stateOf = (deployment: string) => {
    const h = health?.find((x) => healthName(x) === deployment);
    if (!h) return null;
    return h.state === 'closed' ? 'success' : h.state === 'half_open' ? 'warn' : 'error';
  };

  return (
    <Drawer
      dirty={edited}
      open
      onClose={onClose}
      width="lg"
      title={`Fallback chain of ${view.name}`}
      subtitle="Requests try the first step; when it fails, the next, in order."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void save()}>
            Save chain
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <InlineError error={error} />
        <p className="text-ui text-muted">
          Fallbacks are positions in this model’s route tree, not other models’ names: a deployment, a shared
          route with its own rules, or a tier.
        </p>
        {spec.route ? (
          <Banner tone="info" title="Follows a shared route">
            It now serves from the shared route {spec.route}. Saving gives it a chain of its own, starting
            with that route.
          </Banner>
        ) : null}
        <ol aria-label="Steps" className="flex flex-col gap-1.5">
          {steps.map((s, i) => {
            const tone = s.kind === 'deployment' ? stateOf(s.value) : null;
            const missing =
              (s.kind === 'deployment' && !deployments.has(s.value)) ||
              (s.kind === 'route' && !routes.has(s.value));
            return (
              <li
                key={s.key}
                className={cn(
                  'flex items-center gap-3 rounded-control border bg-sunken px-3 py-2',
                  i === 0 ? 'border-accent' : 'border-line',
                )}
              >
                <span className="w-20 shrink-0 text-caption text-muted">
                  {i === 0 ? 'Primary' : `Fallback ${i}`}
                </span>
                <Badge>{KIND_LABEL[s.kind]}</Badge>
                <span className="mono min-w-0 flex-1 truncate text-ui text-fg">
                  {s.kind === 'subtree'
                    ? describeNode(s.node, routes)
                    : s.kind === 'tier'
                      ? tierLabel(s.value)
                      : s.value}
                </span>
                {tone ? <Dot tone={tone} /> : null}
                {missing ? <Badge tone="error">Missing</Badge> : null}
                <span className="flex shrink-0 items-center">
                  <IconButton
                    icon="arrowUp"
                    label={`Move ${s.value} up`}
                    disabled={i === 0}
                    onClick={() => setSteps((list) => moveStep(list, i, -1))}
                  />
                  <IconButton
                    icon="arrowDown"
                    label={`Move ${s.value} down`}
                    disabled={i === steps.length - 1}
                    onClick={() => setSteps((list) => moveStep(list, i, 1))}
                  />
                  <IconButton
                    icon="x"
                    label={`Remove ${s.value}`}
                    onClick={() => setSteps((list) => list.filter((x) => x.key !== s.key))}
                  />
                </span>
              </li>
            );
          })}
          {steps.length === 0 ? (
            <li>
              <EmptyState size="sm" title="No steps" description="Add the deployment to try first." />
            </li>
          ) : null}
        </ol>
        <fieldset className="flex flex-col gap-3 rounded-control border border-line p-3">
          <legend className="px-1 text-ui font-semibold text-fg">Add a step</legend>
          <Segmented<Exclude<StepKind, 'subtree'>>
            label="Kind of step"
            value={kind}
            onChange={(k) => {
              setKind(k);
              setValue('');
            }}
            options={[
              { value: 'deployment', label: 'Deployment' },
              { value: 'route', label: 'Shared route', count: routes.size },
              { value: 'tier', label: 'Tier' },
            ]}
          />
          <div className="flex items-end gap-2">
            <Field label={KIND_LABEL[kind]} error={problem ?? undefined} className="min-w-0 flex-1">
              <Combobox
                value={value}
                onChange={setValue}
                emptyOption={`Select a ${KIND_LABEL[kind].toLowerCase()}`}
                options={options.filter((o) => !used.has(o.value))}
              />
            </Field>
            <Button icon="plus" onClick={add}>
              Add step
            </Button>
          </div>
        </fieldset>
      </div>
    </Drawer>
  );
}
