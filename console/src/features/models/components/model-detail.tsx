'use client';

import { useEffect, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { FavouriteButton } from '@/components/ui/favourite';
import { ProviderMark } from '@/components/ui/marks';
import { Icon } from '@/components/ui/icon';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Combobox, Field, Input } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { DefinitionGrid } from '@/components/ui/layout';
import { DetailHeader, DetailSection } from '@/components/ui/list-detail';
import { CopyLinkButton } from '@/components/ui/misc';
import { Banner, LoadError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { formatCount, formatMoney, plural } from '@/lib/format';
import { useTokenPrice } from '@/lib/hooks';
import { formatUnitAmount } from '@/lib/price';
import { daysUntil, formatDate } from '@/lib/time';
import { money } from '@/lib/validation';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { PlaygroundLink } from '@/features/playground/components/playground-link';

import { activityFeed } from '../activity';
import { modelsApi } from '../api';
import {
  capabilityTags,
  formatContext,
  fromPerMillion,
  MODALITY_LABEL,
  priceLines,
  sumBuckets,
  toPerMillion,
  tokenLimit,
  outputSpeed,
} from '../catalog';
import { useDeploymentMap, useModelViews, useProviderMap, useRouteMap } from '../hooks';
import {
  overridePrice,
  patchModelSpec,
  setModelEnabled,
  useCatalog,
  useModelInsight,
  useModels,
} from '../store';
import { PRICE_FIELDS } from '../deployment-form';
import { describeNode, stepsOf } from '../tree';
import { healthName, type ModelView, rate, statusOf, STATUS_LABEL, STATUS_TONE, tierLabel } from '../types';
import { AccessDrawer, AccessPanel } from './access-panel';
import { ActivityList } from './activity-tab';
import { AliasDialog, paramCount, RemoveAliasDialog } from './alias-dialog';
import { DeploymentDrawer } from './deployment-drawer';
import { DeploymentMenu, reloadHealth } from './deployment-menu';
import { RuntimeState, TurnOffDialog, useDisablesFor } from './deployment-runtime';
import { FallbackEditor } from './fallback-editor';
import { MigrationPanel } from './migration-panel';
import { ModelUsage } from './model-usage';
import { PoliciesPanel } from './policies-panel';
import { TierPrices } from './tier-prices';
import { translateNow } from '@/lib/i18n';

/** The rates behind "More rates" in the price dialog: per token in the reader's unit, or flat in USD. */
const MORE_RATES = PRICE_FIELDS.filter((p) => p.field !== 'input' && p.field !== 'output');

function PriceDialog({
  deployments,
  open,
  onClose,
}: {
  deployments: string[];
  open: boolean;
  onClose: () => void;
}) {
  const tokenPrice = useTokenPrice();
  const prices = useCatalog((s) => s.prices);
  const [deployment, setDeployment] = useState(deployments[0] ?? '');
  const current = prices[deployment]?.price;
  const ratesOf = (price: Record<string, unknown> | null | undefined) =>
    Object.fromEntries(
      MORE_RATES.map((p) => [
        p.field,
        p.perToken ? fromPerMillion(rate(price, p.field), tokenPrice.unit) : (rate(price, p.field) ?? ''),
      ]),
    );
  const [input, setInput] = useState(() => fromPerMillion(rate(current, 'input'), tokenPrice.unit));
  const [output, setOutput] = useState(() => fromPerMillion(rate(current, 'output'), tokenPrice.unit));
  const [more, setMore] = useState<Record<string, string>>(() => ratesOf(current));
  const [invalid, setInvalid] = useState<string | null>(null);
  const unit = `USD per ${tokenPrice.unit} tokens`;
  const moreSet = Object.values(more).filter((v) => v.trim() !== '').length;
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title="Set a price for this deployment?"
      tone="primary"
      confirmLabel="Save price"
      effect="The price replaces the list price for every model that uses this deployment. Spend is counted with it from now on."
      onConfirm={async (reason) => {
        const a = toPerMillion(input, tokenPrice.unit);
        const b = toPerMillion(output, tokenPrice.unit);
        if (!money.safeParse(a).success || !money.safeParse(b).success) {
          const message = `Enter both prices in ${unit}, e.g. ${tokenPrice.unit === '1K' ? '0.0025' : '2.50'}`;
          setInvalid(message);
          throw new Error(message);
        }
        const before = ratesOf(current);
        const extra: Record<string, string | null> = {};
        for (const p of MORE_RATES) {
          const typed = (more[p.field] ?? '').trim();
          if (typed === (before[p.field] ?? '')) continue;
          const value = p.perToken ? toPerMillion(typed, tokenPrice.unit) : typed;
          if (value && !money.safeParse(value).success) {
            const message = `${p.label}: enter an amount in ${p.perToken ? unit : 'USD'}, or leave it empty`;
            setInvalid(message);
            throw new Error(message);
          }
          extra[p.field] = value || null;
        }
        await overridePrice(deployment, { input: a, output: b, ...extra }, reason);
        notify('success', 'Price saved', deployment);
      }}
    >
      {deployments.length > 1 ? (
        <Field label="Deployment">
          <Combobox
            value={deployment}
            onChange={(v) => {
              setDeployment(v);
              setInput(fromPerMillion(rate(prices[v]?.price, 'input'), tokenPrice.unit));
              setOutput(fromPerMillion(rate(prices[v]?.price, 'output'), tokenPrice.unit));
              setMore(ratesOf(prices[v]?.price));
            }}
            options={deployments.map((d) => ({ value: d, label: d }))}
          />
        </Field>
      ) : (
        <p className="text-ui text-muted">
          Deployment <span className="mono text-fg">{deployment}</span>
        </p>
      )}
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label={`Input (${unit})`} error={invalid ?? undefined}>
          <Input
            mono
            inputMode="decimal"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            data-autofocus
          />
        </Field>
        <Field label={`Output (${unit})`}>
          <Input mono inputMode="decimal" value={output} onChange={(e) => setOutput(e.target.value)} />
        </Field>
      </div>
      <details className="group" open={moreSet > 0 ? true : undefined}>
        <summary className="flex w-fit cursor-pointer list-none items-center gap-1 text-ui text-muted hover:text-fg [&::-webkit-details-marker]:hidden">
          <Icon name="chevronRight" size={12} className="transition-transform group-open:rotate-90" />
          More rates{moreSet > 0 ? ` (${String(moreSet)} set)` : ''}
        </summary>
        <div className="mt-3 grid gap-3 sm:grid-cols-2">
          {MORE_RATES.map((p) => (
            <Field key={p.field} label={`${p.label} (${p.perToken ? unit : 'USD'})`} optional>
              <Input
                mono
                inputMode="decimal"
                value={more[p.field] ?? ''}
                onChange={(e) => setMore((m) => ({ ...m, [p.field]: e.target.value }))}
              />
            </Field>
          ))}
        </div>
      </details>
    </ConfirmDialog>
  );
}

export interface ModelDetailProps {
  view: ModelView;
  /** The endpoints it is the default Model for, in words. */
  defaults?: string[];
  onEdit: (focus?: 'lifecycle') => void;
  onOpenModel?: (name: string) => void;
}

/** A spec cell: the figure large, what it is above, a detail under. */
function SpecCell({
  label,
  value,
  unit,
  sub,
}: {
  label: string;
  value: string;
  unit?: string;
  sub?: string;
}) {
  return (
    <div className="min-w-0 bg-raised px-3.5 py-2.5">
      <div className="text-caption text-muted">{label}</div>
      <div className="mono mt-1 truncate text-[16px] leading-6 font-semibold text-fg tabular-nums">
        {value}
        {unit ? <span className="ml-1 font-sans text-caption font-normal text-muted">{unit}</span> : null}
      </div>
      {sub ? <div className="mt-0.5 truncate text-caption text-muted">{sub}</div> : null}
    </div>
  );
}

/** A model: its status, actions, definition, prices and usage, access, the tree behind it, aliases and activity. */
export function ModelDetail({ view, defaults = [], onEdit, onOpenModel }: ModelDetailProps) {
  const timeZone = usePreferences((s) => s.timeZone);
  const deployments = useDeploymentMap();
  const providers = useProviderMap();
  const routes = useRouteMap();
  const health = useCatalog((s) => s.health.data);
  useDisablesFor(health);
  const prices = useCatalog((s) => s.prices);
  const capabilities = useCatalog((s) => s.capabilities);
  const loadPrices = useCatalog((s) => s.loadPrices);
  const loadCapabilities = useCatalog((s) => s.loadCapabilities);
  const usage = useCatalog((s) => s.usage.data?.get(view.name));
  const audit = useCatalog((s) => s.audit[view.name]);
  const loadAudit = useCatalog((s) => s.loadAudit);
  const requested = useCatalog((s) => s.requested.data);
  const insight = useModelInsight((s) => s.slots[view.name]?.data);
  const remove = useModels((s) => s.remove);
  const canPrice = useCan('patchDeployment');
  const canEditDeployment = useCan('replaceDeployment');
  const canReplace = useCan('replaceModel');
  const canToggle = useCan('patchModel');
  const canDelete = useCan('deleteModel');
  const canDeleteDeployments = useCan('deleteDeployment');
  const views = useModelViews();
  // The deployments only this Model routes to: deleting the Model can take them too.
  const ownDeployments = view.leaves.filter(
    (leaf) => !views.some((other) => other.name !== view.name && other.leaves.includes(leaf)),
  );
  const [alsoDeployments, setAlsoDeployments] = useState(false);
  const canAudit = useCan('listAudit');
  const [dialog, setDialog] = useState<
    | 'disable'
    | 'delete'
    | 'price'
    | 'promote'
    | 'access'
    | 'chain'
    | { alias: string | null }
    | { removeAlias: string }
    | { deployment: string }
    | { turnOff: string }
    | null
  >(null);
  const spec = view.model.spec;
  const status = statusOf(view);
  const eolDays = daysUntil(spec.lifecycle?.eol_at);
  const steps = stepsOf(spec);
  const providerType = (name: string) => providers.get(name)?.spec.type;

  useEffect(() => {
    void loadPrices(view.leaves);
    void loadCapabilities(view.leaves);
    if (canAudit) void loadAudit(view.name);
  }, [view.leaves, view.name, loadPrices, loadCapabilities, loadAudit, canAudit]);

  const tokenPrice = useTokenPrice();
  const leafPrices = view.leaves.map((l) => prices[l]).filter((p) => p !== undefined && p !== null);
  const lowest = (field: string) => {
    const values = leafPrices
      .map((p) => rate(p.price, field))
      .filter((v): v is string => v !== null)
      .map(Number)
      .filter((n) => Number.isFinite(n));
    return values.length > 0 ? Math.min(...values) : null;
  };
  const from = leafPrices.length > 1 ? 'from ' : '';
  const firstPrice = leafPrices[0]?.price ?? null;
  const lines = priceLines(firstPrice, view.modality);
  const leafCaps = view.leaves.map((l) => capabilities[l]?.capabilities);
  const tags = [...new Set(leafCaps.flatMap((c) => capabilityTags(c)))];
  const smallest = (name: string) => {
    const known = leafCaps.map((c) => tokenLimit(c, name)).filter((n): n is number => n !== null);
    return known.length > 0 ? Math.min(...known) : null;
  };
  const context = smallest('context_window');
  const maxOutput = smallest('max_output_tokens');
  const capsKnown = view.leaves.some((l) => capabilities[l] !== undefined);
  const totals = usage ? sumBuckets(usage.buckets) : null;
  const speed = outputSpeed(totals?.tokensPerSecond);
  const access = spec.access ?? {};
  const accessCount = ['organizations', 'teams', 'projects', 'keys'].reduce(
    (a, k) => a + ((access as Record<string, string[] | undefined>)[k]?.length ?? 0),
    0,
  );

  const more: MenuItem[] = [
    ...(canReplace
      ? [
          {
            label: view.lifecycle === 'deprecated' ? 'Change end of life' : 'Deprecate',
            icon: 'clock' as const,
            onSelect: () => onEdit('lifecycle'),
          },
          { label: 'Edit fallback chain', icon: 'route' as const, onSelect: () => setDialog('chain') },
          { label: 'Add alias', icon: 'link' as const, onSelect: () => setDialog({ alias: null }) },
        ]
      : []),
    ...(canToggle && (view.lifecycle === 'preview' || view.lifecycle === 'restricted')
      ? [{ label: 'Edit access', icon: 'lock' as const, onSelect: () => setDialog('access') }]
      : []),
    ...(canToggle
      ? [
          view.enabled
            ? {
                label: 'Turn off',
                icon: 'pause' as const,
                danger: true,
                onSelect: () => setDialog('disable'),
              }
            : {
                label: 'Turn on',
                icon: 'play' as const,
                onSelect: () =>
                  void setModelEnabled(view.name, true).then(() =>
                    notify('success', 'Model turned on', view.name),
                  ),
              },
        ]
      : []),
    ...(canDelete
      ? [{ label: 'Delete', icon: 'trash' as const, danger: true, onSelect: () => setDialog('delete') }]
      : []),
  ];

  const activity = activityFeed(audit?.data ?? [], insight?.alerts ?? [], 20);
  const aliasDialog = typeof dialog === 'object' && dialog !== null && 'alias' in dialog ? dialog : null;
  const removeAlias =
    typeof dialog === 'object' && dialog !== null && 'removeAlias' in dialog ? dialog : null;
  const editing =
    typeof dialog === 'object' && dialog !== null && 'deployment' in dialog
      ? deployments.get(dialog.deployment)
      : undefined;
  const turningOff =
    typeof dialog === 'object' && dialog !== null && 'turnOff' in dialog ? dialog.turnOff : null;

  return (
    <div>
      <DetailHeader
        eyebrow={
          <span className="flex flex-wrap items-center gap-x-2 gap-y-1 font-sans text-ui">
            <span className="flex items-center gap-1.5 text-fg">
              <Dot tone={STATUS_TONE[status]} />
              {STATUS_LABEL[status]}
            </span>
            {view.tier ? <span>· {tierLabel(view.tier)} tier</span> : null}
            <span>· {MODALITY_LABEL[view.modality]}</span>
            {view.providers.map((p) => (
              <span key={p} className="flex items-center gap-1.5">
                · <ProviderMark type={providerType(p)} /> {p}
              </span>
            ))}
            {(spec.labels ?? []).map((l) => (
              <Badge key={l}>{l}</Badge>
            ))}
            {spec.visibility === 'internal' ? <Badge>Hidden from /v1/models</Badge> : null}
          </span>
        }
        title={
          <span className="flex min-w-0 items-center gap-1.5">
            <span className="truncate">
              {view.displayName}{' '}
              {spec.catalog?.version ? (
                <span className="mono text-body font-normal text-muted">v{spec.catalog.version}</span>
              ) : null}
            </span>
            <FavouriteButton kind="model" name={view.name} label={`model ${view.name}`} />
          </span>
        }
        subtitle={
          <>
            {view.name}
            {view.aliases.length > 0 ? (
              <span className="text-accent"> · @{view.aliases.join(' @')}</span>
            ) : null}
            {defaults.length > 0 ? (
              <span className="text-success"> · default for {defaults.join(', ')}</span>
            ) : null}
            {view.upstream.length > 0 ? <span> · calls {view.upstream.join(', ')}</span> : null}
          </>
        }
        actions={
          <>
            {view.lifecycle === 'preview' && canToggle ? (
              <Button variant="primary" icon="arrowUp" onClick={() => setDialog('promote')}>
                Promote to available
              </Button>
            ) : null}
            <Gate operation="replaceModel">
              <Button
                variant={view.lifecycle === 'preview' && canToggle ? 'secondary' : 'primary'}
                icon="edit"
                onClick={() => onEdit()}
              >
                Edit
              </Button>
            </Gate>
            {canPrice ? (
              <Button icon="dollar" onClick={() => setDialog('price')} disabled={view.leaves.length === 0}>
                Override price
              </Button>
            ) : null}
            <PlaygroundLink model={view.name} />
            {more.length > 0 ? <MenuButton items={more} label="More" /> : null}
            <span className="ml-auto">
              <CopyLinkButton />
            </span>
          </>
        }
      />
      {!view.enabled ? (
        <Banner tone="error" title="Off">
          Apps asking for {view.name} get an error, and nothing fails over to another model. Turn it on to
          serve it again.
        </Banner>
      ) : null}
      {view.lifecycle === 'preview' ? (
        <Banner tone="info" title="Preview">
          Only{' '}
          {accessCount > 0 ? 'the teams, projects and keys on its access list' : 'those on its access list'}{' '}
          can use it, and it has no uptime commitment. Promote it when it is ready for every app.
        </Banner>
      ) : null}
      {view.lifecycle === 'restricted' ? (
        <Banner tone="warn" title="Restricted">
          Apps outside its access list must ask for access, and an admin approves each request.
        </Banner>
      ) : null}
      {view.lifecycle === 'deprecated' ? (
        <Banner tone="warn" title="Deprecated">
          {eolDays !== null
            ? `It stops working in ${plural(eolDays, 'day')} (${formatDate(spec.lifecycle?.eol_at, timeZone)}).`
            : 'No end date is set.'}{' '}
          {spec.lifecycle?.replacement ? `Move apps to ${spec.lifecycle.replacement}.` : ''}
        </Banner>
      ) : null}

      <DetailSection title="Details">
        <div className="grid grid-cols-2 gap-px border-y border-line bg-line sm:grid-cols-4">
          <SpecCell
            label="Context window"
            value={formatContext(context)}
            unit={context !== null ? 'tokens' : undefined}
            sub={
              maxOutput !== null
                ? `up to ${formatContext(maxOutput)} out`
                : capsKnown
                  ? 'not stated by the provider'
                  : undefined
            }
          />
          {lines.slice(0, 2).map((line) => {
            const v = lowest(line.field);
            return (
              <SpecCell
                key={line.field}
                label={`${line.label} price`}
                value={
                  v === null ? '—' : line.perToken ? formatUnitAmount(v, tokenPrice.unit) : formatMoney(v)
                }
                unit={v === null ? undefined : line.perToken ? `/ ${tokenPrice.unit}` : `a ${line.per}`}
                sub={
                  from
                    ? `lowest of ${plural(leafPrices.length, 'deployment')}`
                    : line.field === 'input' && view.modality === 'embeddings'
                      ? 'input only'
                      : line.field === 'output'
                        ? 'billed as it streams'
                        : line.field === 'per_request'
                          ? 'each search'
                          : undefined
                }
              />
            );
          })}
          {lines.length < 2 ? (
            <SpecCell
              label="Output price"
              value="—"
              sub={view.modality === 'embeddings' ? 'no output tokens' : undefined}
            />
          ) : null}
          <SpecCell
            label="Output speed"
            value={speed ?? '—'}
            unit={speed !== null ? 'tok/s' : undefined}
            sub={
              speed === null && totals?.tokensPerSecond != null
                ? 'not measured reliably'
                : totals?.ttftP50 != null
                  ? `first token ${formatCount(Math.round(totals.ttftP50))} ms`
                  : speed === null
                    ? 'no streamed answers yet'
                    : 'from streamed answers'
            }
          />
        </div>
        {lines.length > 2 ? (
          <dl className="mt-3 flex flex-wrap gap-x-5 gap-y-1 text-ui">
            {lines.slice(2).map((line) => {
              const v = lowest(line.field);
              return (
                <div key={line.field} className="flex gap-1.5">
                  <dt className="text-muted">{line.label}</dt>
                  <dd className="mono text-fg">
                    {v === null
                      ? '—'
                      : line.perToken
                        ? `${from}${tokenPrice.format(v)}`
                        : `${formatMoney(v)} a ${line.per}`}
                  </dd>
                </div>
              );
            })}
          </dl>
        ) : null}
        {view.modality === 'embeddings' || view.modality === 'rerank' ? null : (
          <TierPrices leaves={view.leaves} />
        )}
        {leafPrices.length === 0 && view.leaves.length > 0 && view.leaves.every((l) => prices[l] === null) ? (
          <p className="mt-3 text-caption text-warn">
            There’s no list price for this model, so its spend isn’t counted and priced budgets refuse it. Use
            Override price to set one.
          </p>
        ) : null}
        <div className="mt-4">
          <DefinitionGrid
            columns={4}
            items={[
              { label: 'Family', value: spec.catalog?.family ?? '—' },
              { label: 'Version', value: spec.catalog?.version ?? '—' },
              { label: 'Revision', value: spec.catalog?.revision ?? '—' },
              { label: 'Released', value: spec.catalog?.released_at ?? '—' },
              {
                label: 'End of life',
                value: spec.lifecycle?.eol_at ? formatDate(spec.lifecycle.eol_at, timeZone) : '—',
              },
              {
                label: 'Access',
                value:
                  view.lifecycle === 'preview' || view.lifecycle === 'restricted'
                    ? accessCount > 0
                      ? plural(accessCount, 'entry', 'entries')
                      : 'No one yet'
                    : 'All teams',
              },
              { label: 'Requests, 30 days', value: usage ? formatCount(usage.requests) : '—' },
              { label: 'Managed by', value: spec.owned_by ?? 'antirion' },
            ]}
          />
        </div>
        <div className="mt-4 flex flex-wrap items-center gap-1.5">
          <span className="mr-1 text-caption text-muted">Capabilities</span>
          {tags.length > 0 ? (
            tags.map((t) => <Badge key={t}>{t}</Badge>)
          ) : (
            <Tooltip content="The deployments don’t state their capabilities, so the provider type's defaults apply">
              <span className="text-caption text-dim">The provider type’s defaults</span>
            </Tooltip>
          )}
          {canEditDeployment && view.leaves.length === 1 && deployments.has(view.leaves[0] ?? '') ? (
            <Button
              size="sm"
              variant="ghost"
              icon="edit"
              onClick={() => setDialog({ deployment: view.leaves[0] ?? '' })}
            >
              Change
            </Button>
          ) : null}
        </div>
        {spec.description ? <p className="mt-4 text-body text-muted">{spec.description}</p> : null}
        {spec.annotations && Object.keys(spec.annotations).length > 0 ? (
          <dl className="mt-3 flex flex-col gap-0.5 text-ui">
            {Object.entries(spec.annotations).map(([k, v]) => (
              <div key={k} className="flex gap-2">
                <dt className="mono text-muted">{k}</dt>
                <dd className="text-fg">{v}</dd>
              </div>
            ))}
          </dl>
        ) : null}
      </DetailSection>

      <MigrationPanel view={view} />
      <ModelUsage view={view} />
      <AccessPanel view={view} onEdit={() => setDialog('access')} />

      <DetailSection
        title="Deployments"
        aside={
          canReplace ? (
            <Button size="sm" icon="route" onClick={() => setDialog('chain')}>
              Edit fallback chain
            </Button>
          ) : null
        }
      >
        {view.leaves.length === 0 ? (
          <p className="text-ui text-muted">
            This model doesn’t reach any deployment yet, so requests to it fail.
          </p>
        ) : (
          <div className="overflow-x-auto">
            <Table label={`Deployments of ${view.name}`}>
              <thead>
                <tr>
                  <Th>Deployment</Th>
                  <Th>Provider</Th>
                  <Th>Provider’s model</Th>
                  <Th>Health</Th>
                  <Th numeric>In / out, {tokenPrice.label}</Th>
                  <Th>
                    <span className="sr-only">Actions</span>
                  </Th>
                </tr>
              </thead>
              <tbody>
                {view.leaves.map((leaf) => {
                  const dep = deployments.get(leaf);
                  const h = health?.find((x) => healthName(x) === leaf);
                  const leafPrice = prices[leaf];
                  return (
                    <Tr key={leaf}>
                      <Td mono>
                        {leaf}
                        {dep?.spec.enabled === false ? <Badge tone="error"> Off</Badge> : null}
                      </Td>
                      <Td>
                        <span className="flex items-center gap-1.5">
                          <ProviderMark
                            type={providerType(dep?.spec.provider ?? '')}
                            className="text-muted"
                          />
                          {dep?.spec.provider ?? '—'}
                        </span>
                      </Td>
                      <Td mono muted>
                        {dep?.spec.model ?? '—'}
                      </Td>
                      <Td>
                        <RuntimeState name={leaf} health={h} />
                      </Td>
                      <Td numeric>
                        {leafPrice
                          ? `${formatUnitAmount(rate(leafPrice.price, 'input'), tokenPrice.unit)} / ${formatUnitAmount(rate(leafPrice.price, 'output'), tokenPrice.unit)}`
                          : leafPrice === null
                            ? 'No price'
                            : '…'}
                      </Td>
                      <Td className="text-right">
                        {dep ? (
                          <DeploymentMenu
                            deployment={dep}
                            health={h}
                            onEdit={() => setDialog({ deployment: leaf })}
                            onTurnOff={() => setDialog({ turnOff: leaf })}
                          />
                        ) : null}
                      </Td>
                    </Tr>
                  );
                })}
              </tbody>
            </Table>
          </div>
        )}
        <div className="mt-4 text-ui">
          <div className="mb-1.5 text-caption text-muted">If a deployment fails</div>
          {spec.route ? (
            <p className="text-fg">
              Follows the shared route <span className="mono">{spec.route}</span>, with its rules and
              fallbacks.
            </p>
          ) : steps.length > 1 ? (
            <ol aria-label="Fallback chain" className="flex flex-wrap items-center gap-2">
              {steps.map((s, i) => (
                <li key={s.key} className="flex items-center gap-2">
                  {i > 0 ? <span className="text-dim">→</span> : null}
                  <span className="rounded-control border border-line bg-sunken px-2 py-1">
                    <span className="block text-caption text-muted">
                      {i === 0 ? 'Primary' : `Fallback ${i}`}
                    </span>
                    <span className="mono text-fg">{describeNode(s.node, routes)}</span>
                  </span>
                </li>
              ))}
            </ol>
          ) : (
            <p className="text-muted">
              No fallback: requests fail when this deployment fails. Add a second deployment to its fallback
              chain, or route it through a rule on the Routing policy screen.
            </p>
          )}
        </div>
      </DetailSection>

      <PoliciesPanel view={view} />

      <DetailSection
        title="Aliases"
        aside={
          canReplace ? (
            <Button size="sm" icon="plus" onClick={() => setDialog({ alias: null })}>
              Add alias
            </Button>
          ) : null
        }
      >
        {view.aliases.length === 0 ? (
          <p className="text-ui text-muted">
            No aliases. An alias is another name apps can send for this model.
          </p>
        ) : (
          <ul className="flex flex-col">
            {Object.entries(spec.aliases ?? {}).map(([alias, a]) => (
              <li
                key={alias}
                className="flex items-center gap-2 border-b border-line py-1.5 text-ui last:border-0"
              >
                <span className="mono text-accent">@{alias}</span>
                <span className="min-w-0 flex-1 truncate text-muted">{a.description ?? ''}</span>
                {paramCount(a.params) > 0 ? (
                  <Badge mono>sets {plural(paramCount(a.params), 'param')}</Badge>
                ) : null}
                {a.owner ? <Badge>{a.owner}</Badge> : null}
                <span className="mono w-24 text-right text-caption text-muted tabular-nums">
                  {requested ? `${formatCount(requested.get(alias) ?? 0, { compact: true })} req` : ''}
                </span>
                {canReplace ? (
                  <MenuButton
                    iconOnly
                    icon="more"
                    variant="ghost"
                    size="sm"
                    align="right"
                    label={`Actions for @${alias}`}
                    items={[
                      { label: 'Edit', icon: 'edit', onSelect: () => setDialog({ alias }) },
                      {
                        label: 'Remove',
                        icon: 'trash',
                        danger: true,
                        onSelect: () => setDialog({ removeAlias: alias }),
                      },
                    ]}
                  />
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </DetailSection>

      <DetailSection title="Activity">
        {canAudit && (!audit || (audit.status === 'loading' && !audit.data)) ? (
          <LoadingRows rows={3} />
        ) : null}
        <LoadError of={audit} subject="Activity" size="sm" />
        {audit?.data && activity.length === 0 ? (
          <p className="text-ui text-muted">No activity recorded yet.</p>
        ) : null}
        <ActivityList items={activity} onOpenModel={onOpenModel} />
      </DetailSection>

      <PriceDialog
        key={view.name}
        deployments={view.leaves}
        open={dialog === 'price'}
        onClose={() => setDialog(null)}
      />
      {editing ? <DeploymentDrawer deployment={editing} onClose={() => setDialog(null)} /> : null}
      <TurnOffDialog
        name={turningOff ?? ''}
        open={turningOff !== null}
        onClose={() => setDialog(null)}
        onDone={reloadHealth}
      />
      {dialog === 'access' ? <AccessDrawer view={view} onClose={() => setDialog(null)} /> : null}
      {dialog === 'chain' ? <FallbackEditor view={view} onClose={() => setDialog(null)} /> : null}
      {aliasDialog ? (
        <AliasDialog model={view.name} alias={aliasDialog.alias} onClose={() => setDialog(null)} />
      ) : null}
      {removeAlias ? (
        <RemoveAliasDialog
          model={view.name}
          alias={removeAlias.removeAlias}
          onClose={() => setDialog(null)}
        />
      ) : null}
      <ConfirmDialog
        open={dialog === 'promote'}
        onClose={() => setDialog(null)}
        tone="primary"
        title={`Promote ${view.name} to available?`}
        confirmLabel="Promote to available"
        effect="Every app can use it from then on, and its access list no longer applies."
        onConfirm={async (reason) => {
          await patchModelSpec(view.name, { lifecycle: { state: 'ga' } }, reason);
          notify('success', 'Model promoted', translateNow('{0} is available to every app', view.name));
        }}
      />
      <ConfirmDialog
        open={dialog === 'disable'}
        onClose={() => setDialog(null)}
        title={`Turn off ${view.name}?`}
        objectName={view.name}
        confirmLabel="Turn off model"
        effect="Apps asking for it get an error, and nothing fails over to another model. To keep serving it from another deployment, turn off the failing deployment instead."
        onConfirm={async (reason) => {
          await setModelEnabled(view.name, false, reason);
          notify('success', 'Model turned off', view.name, {
            label: 'Undo',
            onClick: () => void setModelEnabled(view.name, true),
          });
        }}
      />
      <ConfirmDialog
        open={dialog === 'delete'}
        onClose={() => setDialog(null)}
        title={`Delete ${view.name}`}
        confirmLabel="Delete permanently"
        typeToConfirm={view.name}
        effect={
          alsoDeployments && ownDeployments.length > 0
            ? `Apps asking for it get an error, and its ${plural(ownDeployments.length, 'deployment')} and their prices go with it. This can’t be undone.`
            : 'Apps asking for it get an error. Its deployments and their prices stay. This can’t be undone.'
        }
        onConfirm={async (reason) => {
          await remove(view.name, reason);
          const gone = alsoDeployments ? ownDeployments : [];
          const results = await Promise.allSettled(gone.map((d) => modelsApi.deleteDeployment(d)));
          const failed = gone.filter((_, i) => results[i]?.status === 'rejected');
          void useCatalog.getState().loadCatalog();
          notify(
            failed.length ? 'warn' : 'success',
            'Model deleted',
            gone.length
              ? failed.length
                ? `${view.name}; ${plural(failed.length, 'deployment')} could not be deleted: ${failed.join(', ')}`
                : `${view.name} and ${plural(gone.length, 'deployment')}`
              : view.name,
          );
        }}
      >
        {ownDeployments.length > 0 && canDeleteDeployments ? (
          <Checkbox
            className="mt-3"
            checked={alsoDeployments}
            onChange={setAlsoDeployments}
            label={`Also delete its ${plural(ownDeployments.length, 'deployment')}`}
            description={ownDeployments.join(', ')}
          />
        ) : null}
      </ConfirmDialog>
    </div>
  );
}
