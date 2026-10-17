'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Combobox, Field, Input, MultiSelect, Segmented, Select, Textarea } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { ProviderMark } from '@/components/ui/marks';
import { useEdited, useTokenPrice } from '@/lib/hooks';
import { joinValues, splitValues } from '@/components/ui/select';
import { providerTypeLabel } from '@/lib/providers';
import { useTeamOptions } from '@/lib/team-options';
import { serverFieldErrors, validate, type FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { AddProviderDrawer } from '@/features/providers/components/add-provider-drawer';

import {
  type AddModelValues,
  addModelSchema,
  deploymentNameFor,
  EMPTY_ADD_MODEL,
  toModelSpec,
} from '../form';
import { toPerMillion } from '../catalog';
import { useEffectivePrice } from '../hooks';
import { addModel, useCatalog } from '../store';
import { BUILTIN_TIERS, rate, tierLabel } from '../types';

const TIER_HINTS: Record<string, string> = {
  frontier: 'The most capable models',
  balanced: 'Quality at a moderate price',
  fast: 'Low latency and cost',
  specialized: 'Embeddings, rerank and other tasks',
};

const STATUS_HINTS: Record<AddModelValues['lifecycle'], string> = {
  ga: 'Every app can use it.',
  preview: 'Only the teams you list can use it, with no uptime commitment.',
  restricted: 'Apps must ask for access, and an admin approves each request.',
};

/** A model's name from the provider's id: "gpt-4o-2024-08-06" stays, "anthropic.claude-x:0" → "claude-x". */
function suggestedName(upstream: string): string {
  const last = upstream.trim().split('/').pop() ?? '';
  return last.replace(/^[a-z]+\./, '').replace(/:\d+$/, '');
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <fieldset className="flex flex-col gap-4">
      <legend className="mb-3 text-ui font-semibold text-fg">{title}</legend>
      {children}
    </fieldset>
  );
}

/**
 * "Add model": a provider's model, on a new deployment or an existing one, under the name apps
 * send. Prices come from the list prices unless set; a provider can be added from here.
 */
export function AddModelDrawer({
  open,
  onClose,
  onAdded,
}: {
  open: boolean;
  onClose: () => void;
  onAdded: (name: string) => void;
}) {
  const [values, setValues] = useState<AddModelValues>(EMPTY_ADD_MODEL);
  const [nameTouched, setNameTouched] = useState(false);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const teamOptions = useTeamOptions(values.lifecycle !== 'ga');
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  const [addingProvider, setAddingProvider] = useState(false);
  const providersSlot = useCatalog((s) => s.providers);
  const providers = providersSlot.data ?? [];
  const loadProviders = useCatalog((s) => s.loadProviders);
  const deployments = useCatalog((s) => s.deployments.data) ?? [];
  const tiers = useCatalog((s) => s.tiers.data);
  const canAddProvider = useCan('createProvider');
  const tierOptions = (tiers?.map((t) => t.name) ?? [...BUILTIN_TIERS]).map((t) => {
    const hint = tiers?.find((x) => x.name === t)?.description ?? TIER_HINTS[t];
    return { value: t, label: hint ? `${tierLabel(t)} · ${hint}` : tierLabel(t) };
  });
  const reuse = deployments.find(
    (d) => d.spec.provider === values.provider && d.spec.model === values.upstreamModel.trim(),
  );

  const set = <K extends keyof AddModelValues>(key: K, value: AddModelValues[K]) => {
    setValues((v) => ({ ...v, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _gone, ...rest }) => rest);
  };

  const setUpstream = (upstream: string) => {
    setValues((v) => ({
      ...v,
      upstreamModel: upstream,
      name: nameTouched ? v.name : suggestedName(upstream),
    }));
    setErrors(({ upstreamModel: _u, name: _n, ...rest }) => rest);
  };

  const pickDeployment = (name: string) => {
    const dep = deployments.find((d) => d.name === name);
    setValues((v) => ({
      ...v,
      existingDeployment: name,
      name: nameTouched || !dep ? v.name : suggestedName(dep.spec.model),
    }));
    setErrors(({ existingDeployment: _e, name: _n, ...rest }) => rest);
  };

  const priced = values.deploymentMode === 'existing' ? values.existingDeployment : reuse?.name;
  const catalogPrice = useEffectivePrice(priced || undefined);
  const tokenPrice = useTokenPrice();

  async function submit() {
    const result = validate(addModelSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    const v = result.data;
    try {
      const existing = v.deploymentMode === 'existing' ? v.existingDeployment : (reuse?.name ?? null);
      await addModel({
        name: v.name,
        spec: toModelSpec(v),
        existingDeployment: existing,
        deployment:
          existing === null
            ? {
                name: deploymentNameFor(v.provider, v.upstreamModel),
                provider: v.provider,
                model: v.upstreamModel,
                ...(v.inputPrice
                  ? {
                      price: {
                        input: toPerMillion(v.inputPrice, tokenPrice.unit),
                        output: toPerMillion(v.outputPrice, tokenPrice.unit),
                      },
                    }
                  : {}),
              }
            : null,
      });
      notify('success', 'Model added', `Apps can call it as ${v.name}.`);
      onAdded(v.name);
      setValues(EMPTY_ADD_MODEL);
      onClose();
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `models.${v.name}`));
    } finally {
      setBusy(false);
    }
  }

  const noProviders = providersSlot.data !== null && providers.length === 0;

  return (
    <>
      <Drawer
        dirty={edited}
        open={open && !addingProvider}
        onClose={onClose}
        title="Add model"
        subtitle="Make a provider’s model available to your apps."
        width="lg"
        footer={
          <>
            <CancelButton />
            <Button variant="primary" busy={busy} onClick={() => void submit()}>
              Add model
            </Button>
          </>
        }
      >
        <form
          noValidate
          className="flex flex-col gap-7"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <InlineError error={error} />
          <Section title="Source">
            {deployments.length > 0 ? (
              <Segmented<'new' | 'existing'>
                label="Source"
                value={values.deploymentMode}
                onChange={(m) => set('deploymentMode', m)}
                options={[
                  { value: 'new', label: 'New from a provider' },
                  { value: 'existing', label: 'An existing deployment' },
                ]}
              />
            ) : null}
            {values.deploymentMode === 'new' ? (
              <div className="grid gap-4 sm:grid-cols-2">
                <Field
                  label="Provider"
                  error={errors.provider}
                  required
                  aside={
                    canAddProvider && providers.length > 0 ? (
                      <button
                        type="button"
                        className="text-caption text-accent hover:underline"
                        onClick={() => setAddingProvider(true)}
                      >
                        Add provider
                      </button>
                    ) : undefined
                  }
                  hint={noProviders ? 'No providers yet. Add one to continue.' : undefined}
                >
                  <Combobox
                    value={values.provider}
                    onChange={(v) => set('provider', v)}
                    placeholder={noProviders ? 'No providers yet' : 'Select a provider'}
                    disabled={noProviders}
                    options={providers.map((p) => ({
                      value: p.name,
                      label: `${p.name} · ${providerTypeLabel(p.spec.type)}`,
                      icon: <ProviderMark type={p.spec.type} size="md" />,
                    }))}
                    data-autofocus
                  />
                </Field>
                <Field
                  label="Provider’s model ID"
                  error={errors.upstreamModel}
                  required
                  hint={
                    reuse
                      ? `Uses the existing deployment ${reuse.name}.`
                      : values.provider && values.upstreamModel
                        ? `Creates the deployment ${deploymentNameFor(values.provider, values.upstreamModel)}.`
                        : 'As the provider names it, e.g. gpt-4o-mini'
                  }
                >
                  <Input mono value={values.upstreamModel} onChange={(e) => setUpstream(e.target.value)} />
                </Field>
                {noProviders && canAddProvider ? (
                  <div className="flex items-center gap-3 rounded-control border border-dashed border-line-strong px-3 py-2.5 sm:col-span-2">
                    <span className="flex-1 text-ui text-muted">
                      A model is served by a provider. Connect one first, with its API key.
                    </span>
                    <Button size="sm" icon="plus" onClick={() => setAddingProvider(true)}>
                      Add provider
                    </Button>
                  </div>
                ) : null}
              </div>
            ) : (
              <Field label="Deployment" error={errors.existingDeployment} required>
                <Combobox
                  value={values.existingDeployment}
                  onChange={(v) => pickDeployment(v)}
                  placeholder="Select a deployment"
                  options={deployments.map((d) => ({
                    value: d.name,
                    label: `${d.name} · ${d.spec.provider}/${d.spec.model}`,
                  }))}
                />
              </Field>
            )}
          </Section>
          <Section title="Name">
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Name" error={errors.name} required hint="Apps send this name in the model field.">
                <Input
                  mono
                  value={values.name}
                  onChange={(e) => {
                    setNameTouched(true);
                    set('name', e.target.value);
                  }}
                />
              </Field>
              <Field label="Display name" optional error={errors.displayName}>
                <Input value={values.displayName} onChange={(e) => set('displayName', e.target.value)} />
              </Field>
            </div>
          </Section>
          {values.deploymentMode === 'new' && !reuse ? (
            <Section title="Prices">
              <div className="grid gap-4 sm:grid-cols-2">
                <Field
                  label={`Input price (USD per ${tokenPrice.unit} tokens)`}
                  optional
                  error={errors.inputPrice}
                  hint="Leave both blank to use list prices."
                >
                  <Input
                    mono
                    inputMode="decimal"
                    value={values.inputPrice}
                    onChange={(e) => set('inputPrice', e.target.value)}
                  />
                </Field>
                <Field
                  label={`Output price (USD per ${tokenPrice.unit} tokens)`}
                  optional
                  error={errors.outputPrice}
                >
                  <Input
                    mono
                    inputMode="decimal"
                    value={values.outputPrice}
                    onChange={(e) => set('outputPrice', e.target.value)}
                  />
                </Field>
              </div>
            </Section>
          ) : null}
          {catalogPrice === null ? (
            <p className="-mt-4 text-caption text-warn">
              There’s no list price for this deployment, so its spend isn’t counted and priced budgets refuse
              it. Set prices to fix that.
            </p>
          ) : catalogPrice ? (
            <p className="-mt-4 text-caption text-muted">
              List price: input {tokenPrice.format(rate(catalogPrice.price, 'input'))}, output{' '}
              {tokenPrice.format(rate(catalogPrice.price, 'output'))}.
            </p>
          ) : null}
          <Section title="Access">
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Tier" error={errors.tier} required hint="Groups models by what they’re for.">
                <Select value={values.tier} onChange={(v) => set('tier', v)} options={tierOptions} />
              </Field>
              <Field label="Status" hint={STATUS_HINTS[values.lifecycle]}>
                <Select
                  value={values.lifecycle}
                  onChange={(v) => set('lifecycle', v as AddModelValues['lifecycle'])}
                  options={[
                    { value: 'ga', label: 'Available' },
                    { value: 'preview', label: 'Preview' },
                    { value: 'restricted', label: 'Restricted' },
                  ]}
                />
              </Field>
              {values.lifecycle !== 'ga' ? (
                <Field
                  label="Teams that can use it"
                  optional
                  error={errors.accessTeams}
                  className="sm:col-span-2"
                  hint="Pick teams, or type a team's name. Leave empty for none yet."
                >
                  <MultiSelect
                    mono
                    creatable
                    noun="team"
                    placeholder="Choose teams"
                    options={teamOptions}
                    value={splitValues(values.accessTeams)}
                    onChange={(v) => set('accessTeams', joinValues(v))}
                  />
                </Field>
              ) : null}
              <Field label="Labels" optional error={errors.labels} hint="e.g. chat, vision">
                <MultiSelect
                  mono
                  creatable
                  noun="label"
                  placeholder="Add labels"
                  options={[]}
                  value={splitValues(values.labels)}
                  onChange={(v) => set('labels', joinValues(v))}
                />
              </Field>
              <Field label="Description" optional error={errors.description}>
                <Textarea
                  rows={2}
                  value={values.description}
                  onChange={(e) => set('description', e.target.value)}
                />
              </Field>
            </div>
          </Section>
          <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
        </form>
      </Drawer>
      <AddProviderDrawer
        open={open && addingProvider}
        onClose={() => setAddingProvider(false)}
        taken={providers.map((p) => p.name)}
        onAdded={(name) => {
          void loadProviders().then(() => {
            setValues((v) => ({ ...v, deploymentMode: 'new', provider: name }));
            setErrors(({ provider: _p, ...rest }) => rest);
          });
        }}
      />
    </>
  );
}
