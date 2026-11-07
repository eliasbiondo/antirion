'use client';

import { useEffect, useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Combobox, Field, Input, MultiSelect, Segmented, Select, Textarea } from '@/components/ui/form';
import { Autocomplete, type SelectOption } from '@/components/ui/select';
import { useCan } from '@/components/ui/gate';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { Icon } from '@/components/ui/icon';
import { InlineError } from '@/components/ui/states';
import { ProviderMark } from '@/components/ui/marks';
import { useEdited, useTokenPrice } from '@/lib/hooks';
import { joinValues, splitValues } from '@/components/ui/select';
import { providerTypeLabel } from '@/lib/providers';
import { useTeamOptions } from '@/lib/team-options';
import { serverFieldErrors, validate, type FieldErrors } from '@/lib/validation';
import { notify } from '@/stores/ui';

import { AddProviderDrawer } from '@/features/providers/components/add-provider-drawer';
import type { ProviderModel } from '@/features/providers/api';
import { contextText, useProviderModels } from '@/features/providers/models';
import { formatMoney, type TokenUnit } from '@/lib/format';

import {
  type AddModelValues,
  addModelSchema,
  deploymentNameFor,
  EMPTY_ADD_MODEL,
  toModelSpec,
  unusedDeployments,
} from '../form';
import { fromPerMillion, toPerMillion } from '../catalog';
import { useEffectivePrice } from '../hooks';
import { addModel, useCatalog, useModels } from '../store';
import { BUILTIN_TIERS, rate, tierLabel } from '../types';
import { translateNow } from '@/lib/i18n';

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

/** A catalog price in the reader's unit: "$0.15" per 1M, "$0.00015" per 1K. */
function money(perMillion: string, unit: TokenUnit): string {
  const n = Number(perMillion);
  return formatMoney(unit === '1K' ? n / 1000 : n);
}

/** A provider's model as an option: its id, its name, context window and price, and where it's from. */
function modelOption(m: ProviderModel, unit: TokenUnit): SelectOption {
  const name = m.name && m.name.toLowerCase() !== m.id.toLowerCase() ? m.name : null;
  const description = [name, contextText(m.context_window), m.listed_by_provider ? null : 'from catalog']
    .filter(Boolean)
    .join(' · ');
  return {
    value: m.id,
    label: m.id,
    ...(description ? { description } : {}),
    ...(m.price ? { meta: `${money(m.price.input, unit)} in · ${money(m.price.output, unit)} out` } : {}),
  };
}

/** The catalog's prices of a model in the reader's unit, as the price fields hold them. */
interface CatalogPrice {
  id: string;
  input: string;
  output: string;
  /** Per 1M, as the API states it. */
  cacheRead: string | null;
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
  const deploymentsData = useCatalog((s) => s.deployments.data);
  const deployments = useMemo(() => deploymentsData ?? [], [deploymentsData]);
  const routes = useCatalog((s) => s.routes.data);
  const models = useModels((s) => s.items);
  const modelsLoaded = useModels((s) => s.loadedAt !== null);
  // "Unused deployment" only when some deployment serves no Model; the path stays "New from a provider".
  const unused = useMemo(
    () => (modelsLoaded && routes ? unusedDeployments(deployments, models, routes) : []),
    [deployments, models, routes, modelsLoaded],
  );
  if (values.deploymentMode === 'existing' && unused.length === 0)
    setValues((v) => ({ ...v, deploymentMode: 'new', existingDeployment: '' }));
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

  // The provider's models: listed ones first, then the catalog's; any other id can be typed.
  const modelsEntry = useProviderModels((s) => (values.provider ? s.byProvider[values.provider] : undefined));
  const loadModels = useProviderModels((s) => s.load);
  const resolveModel = useProviderModels((s) => s.resolve);
  const upstreamId = values.upstreamModel.trim();
  const resolved = useProviderModels((s) =>
    values.provider && upstreamId ? s.resolved[`${values.provider}/${upstreamId}`] : undefined,
  );
  useEffect(() => {
    if (!open || !values.provider) return;
    const entry = useProviderModels.getState().byProvider[values.provider];
    if (!entry?.loaded && !entry?.loading) void loadModels(values.provider);
  }, [open, values.provider, loadModels]);
  const listedModel = modelsEntry?.list?.items.find((m) => m.id === upstreamId);
  // A typed id the list doesn't have: ask the gateway once typing pauses.
  useEffect(() => {
    if (!open || !values.provider || !upstreamId || listedModel || resolved !== undefined) return;
    const provider = values.provider;
    const timer = window.setTimeout(() => {
      resolveModel(provider, upstreamId).catch(() => undefined);
    }, 400);
    return () => window.clearTimeout(timer);
  }, [open, values.provider, upstreamId, listedModel, resolved, resolveModel]);
  const known = listedModel ?? resolved ?? null;
  const tokenPriceUnit = useTokenPrice().unit;
  const modelOptions = useMemo(
    () => (modelsEntry?.list?.items ?? []).map((m) => modelOption(m, tokenPriceUnit)),
    [modelsEntry?.list?.items, tokenPriceUnit],
  );

  // Prices from the catalog fill the fields the person hasn't typed in, and follow the model.
  const catalog: CatalogPrice | null =
    known?.price && upstreamId
      ? {
          id: upstreamId,
          input: fromPerMillion(known.price.input, tokenPriceUnit),
          output: fromPerMillion(known.price.output, tokenPriceUnit),
          cacheRead: known.price.cache_read,
        }
      : null;
  const [prefilled, setPrefilled] = useState<CatalogPrice | null>(null);
  const catalogKey = catalog ? `${values.provider}/${catalog.id}/${catalog.input}/${catalog.output}` : '';
  const [appliedKey, setAppliedKey] = useState('');
  if (catalogKey !== appliedKey) {
    setAppliedKey(catalogKey);
    setValues((v) => {
      const untouched = (field: 'inputPrice' | 'outputPrice', was: string | undefined) =>
        v[field] === '' || v[field] === was;
      return {
        ...v,
        inputPrice: untouched('inputPrice', prefilled?.input) ? (catalog?.input ?? '') : v.inputPrice,
        outputPrice: untouched('outputPrice', prefilled?.output) ? (catalog?.output ?? '') : v.outputPrice,
      };
    });
    setPrefilled(catalog);
  }
  const usesCatalog =
    catalog !== null && values.inputPrice === catalog.input && values.outputPrice === catalog.output;

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
                        // The catalog's cached-read price comes along while its prices are kept.
                        ...(usesCatalog && catalog.cacheRead ? { cache_read: catalog.cacheRead } : {}),
                      },
                    }
                  : {}),
              }
            : null,
      });
      notify('success', 'Model added', translateNow('Apps can call it as {0}.', v.name));
      onAdded(v.name);
      setValues(EMPTY_ADD_MODEL);
      setPrefilled(null);
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
            {unused.length > 0 ? (
              <Segmented<'new' | 'existing'>
                label="Source"
                value={values.deploymentMode}
                onChange={(m) => set('deploymentMode', m)}
                options={[
                  { value: 'new', label: 'New from a provider' },
                  { value: 'existing', label: `Unused deployment (${String(unused.length)})` },
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
                  aside={
                    values.provider ? (
                      <button
                        type="button"
                        className="flex items-center gap-1 text-caption text-accent hover:underline disabled:text-muted disabled:no-underline"
                        disabled={modelsEntry?.loading}
                        onClick={() => void loadModels(values.provider, { refresh: true })}
                      >
                        <Icon name="refresh" size={12} />
                        {modelsEntry?.loading ? 'Refreshing…' : 'Refresh list'}
                      </button>
                    ) : undefined
                  }
                  hint={
                    <>
                      {modelsEntry?.list?.error ? (
                        <span className="block">
                          The provider didn’t list its models, so this list comes from the catalog.
                        </span>
                      ) : null}
                      {reuse
                        ? `Uses the existing deployment ${reuse.name}.`
                        : values.provider && values.upstreamModel
                          ? `Creates the deployment ${deploymentNameFor(values.provider, values.upstreamModel)}.`
                          : 'As the provider names it, e.g. gpt-4o-mini. Pick from the list or type any ID.'}
                    </>
                  }
                >
                  <Autocomplete
                    mono
                    value={values.upstreamModel}
                    onChange={setUpstream}
                    options={modelOptions}
                    placeholder={values.provider ? 'Search or type an ID' : undefined}
                    noMatch="Not in the list. You can still use this ID."
                    status={
                      !values.provider
                        ? 'Choose a provider to see its models.'
                        : modelsEntry?.loading && !modelsEntry.list
                          ? 'Loading the provider’s models…'
                          : modelsEntry?.error && !modelsEntry.list
                            ? 'Couldn’t load the provider’s models. You can still type an ID.'
                            : modelOptions.length > 0
                              ? `Prices in USD per ${tokenPriceUnit} tokens, input and output.`
                              : undefined
                    }
                  />
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
              <Field
                label="Deployment"
                error={errors.existingDeployment}
                required
                hint="A deployment set up in the configuration or left after its Model was deleted."
              >
                <Combobox
                  value={values.existingDeployment}
                  onChange={(v) => pickDeployment(v)}
                  placeholder="Select a deployment"
                  options={unused.map((d) => ({
                    value: d.name,
                    label: `${d.name} · ${d.spec.provider}/${d.spec.model}`,
                  }))}
                />
              </Field>
            )}
            <p className="text-caption text-muted">
              To call a model you already offer by another name, add an alias on that Model instead.
            </p>
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
                  hint={catalog ? undefined : 'Leave both blank to use list prices.'}
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
                {catalog ? (
                  <p className="-mt-2 flex flex-wrap items-baseline gap-x-2 text-caption text-muted sm:col-span-2">
                    {usesCatalog ? (
                      'Prices from the catalog · edit to override'
                    ) : (
                      <>
                        <span>
                          Catalog price: {money(known?.price?.input ?? '', tokenPriceUnit)} input,{' '}
                          {money(known?.price?.output ?? '', tokenPriceUnit)} output
                        </span>
                        <button
                          type="button"
                          className="text-accent hover:underline"
                          onClick={() => {
                            setValues((v) => ({
                              ...v,
                              inputPrice: catalog.input,
                              outputPrice: catalog.output,
                            }));
                            setErrors(({ inputPrice: _i, outputPrice: _o, ...rest }) => rest);
                          }}
                        >
                          Use catalog price
                        </button>
                      </>
                    )}
                  </p>
                ) : null}
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
