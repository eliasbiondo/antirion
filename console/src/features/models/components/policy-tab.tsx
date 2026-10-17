'use client';

import Link from 'next/link';
import { useCallback, useEffect, useState } from 'react';

import { useScopedHref } from '@/components/layout/url-sync';
import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { BarRow } from '@/components/ui/charts';
import { Switch } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Combobox, Field, Input, NumberInput, Select } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { Panel, TileGrid } from '@/components/ui/layout';
import { MenuButton } from '@/components/ui/menu';
import { ProviderMark } from '@/components/ui/marks';
import { ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { Tooltip } from '@/components/ui/tooltip';
import { api } from '@/lib/api/client';
import { formatCount, formatPercent, plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { scopeOrganization, useScope } from '@/stores/scope';
import { notify } from '@/stores/ui';

import {
  type GuardrailResource,
  modelsApi,
  type OrganizationResource,
  type PoliciesSettings,
  type ProviderStatus,
} from '../api';
import { useProviderMap } from '../hooks';
import { patchRouting, useCatalog } from '../store';
import { BUILTIN_TIERS, type ModelView, tierLabel } from '../types';
import { endpointLabel } from './models-screen';
import { auditActorBy } from '@/lib/audit-words';

/** The endpoints a request may leave the model out of, so a default applies (the others take it from the path or require it). */
export const DEFAULT_ENDPOINTS = [
  'chat_completions',
  'responses',
  'completions',
  'embeddings',
  'images',
  'audio',
  'moderations',
  'rerank',
  'realtime',
] as const;

/** A read the tab makes for itself: data, or the error, or null while loading. */
function useRead<T>(
  enabled: boolean,
  read: () => Promise<T>,
): { data: T | null; error: unknown; reload: () => void } {
  const [state, setState] = useState<{ data: T | null; error: unknown }>({ data: null, error: null });
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    read().then(
      (data) => live && setState({ data, error: null }),
      (error: unknown) => live && setState({ data: null, error }),
    );
    return () => {
      live = false;
    };
    // `read` is a module function per call site; `tick` asks for a fresh read.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, tick]);
  const reload = useCallback(() => setTick((t) => t + 1), []);
  return { ...state, reload };
}

type Retry = { max_attempts?: number; fallback?: boolean; backoff?: { initial?: string; max?: string } };
type PromptCaching = { auto_inject?: boolean; ttl?: '5m' | '1h' };

function PolicyRow({
  label,
  children,
  action,
}: {
  label: string;
  children: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <div className="grid grid-cols-[160px_minmax(0,1fr)_auto] items-start gap-3 border-b border-line py-2.5 text-ui last:border-0 max-sm:grid-cols-1">
      <dt className="text-muted">{label}</dt>
      <dd className="min-w-0 text-fg">{children}</dd>
      <div className="flex justify-end">{action}</div>
    </div>
  );
}

/** Edits the default Model per endpoint (`routing.default_models`). */
function DefaultsDialog({ views, onClose }: { views: ModelView[]; onClose: () => void }) {
  const current = useCatalog((s) => s.routing.data?.spec.default_models ?? {});
  const [values, setValues] = useState<Record<string, string>>({ ...current });
  return (
    <ConfirmDialog
      open
      onClose={onClose}
      tone="primary"
      title="Change default models?"
      confirmLabel="Save defaults"
      effect="Requests that name no model go to the model set for their endpoint. Requests without a default are refused, as now."
      onConfirm={async (reason) => {
        const patch: Record<string, string | null> = {};
        for (const e of DEFAULT_ENDPOINTS) {
          const next = values[e] ?? '';
          if (next !== (current[e] ?? '')) patch[e] = next || null;
        }
        if (Object.keys(patch).length === 0) return;
        await patchRouting({ default_models: patch }, reason);
        notify('success', 'Default models saved');
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        {DEFAULT_ENDPOINTS.map((e) => (
          <Field key={e} label={endpointLabel(e).replace(/^./, (c) => c.toUpperCase())} optional>
            <Combobox
              value={values[e] ?? ''}
              onChange={(v) => setValues((s) => ({ ...s, [e]: v }))}
              emptyOption="No default"
              options={views.map((v) => ({ value: v.name, label: v.name }))}
            />
          </Field>
        ))}
      </div>
    </ConfirmDialog>
  );
}

/** Edits the gateway's retry and fallback defaults (`policies.resilience.retry`). */
function RetryDialog({
  retry,
  etag,
  onClose,
  onSaved,
}: {
  retry: Retry;
  etag: string | undefined;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [attempts, setAttempts] = useState(String(retry.max_attempts ?? 5));
  const [fallback, setFallback] = useState(retry.fallback !== false);
  return (
    <ConfirmDialog
      open
      onClose={onClose}
      tone="primary"
      title="Change retries and fallback?"
      confirmLabel="Save retries"
      effect="Applies to every request, unless an organization, team, key or route sets its own."
      onConfirm={async (reason) => {
        const n = Number(attempts);
        if (!Number.isInteger(n) || n < 1 || n > 10)
          throw new Error('Enter a number of attempts from 1 to 10');
        await modelsApi.patchPolicies({ resilience: { retry: { max_attempts: n, fallback } } }, etag, reason);
        notify('success', 'Retries saved');
        onSaved();
      }}
    >
      <Field
        label="Attempts per request"
        hint="Every try counts, the first one and fallbacks included (1 to 10)."
      >
        <NumberInput value={attempts} onChange={setAttempts} min={1} max={10} step={1} steppers />
      </Field>
      <Switch
        checked={fallback}
        onChange={setFallback}
        label="Fall back to the next deployment when one fails"
      />
    </ConfirmDialog>
  );
}

/** Edits provider prompt caching (`policies.cache.prompt_caching`). */
function PromptCachingDialog({
  caching,
  etag,
  onClose,
  onSaved,
}: {
  caching: PromptCaching | undefined;
  etag: string | undefined;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [on, setOn] = useState(caching?.auto_inject === true);
  const [ttl, setTtl] = useState<'5m' | '1h'>(caching?.ttl ?? '5m');
  return (
    <ConfirmDialog
      open
      onClose={onClose}
      tone="primary"
      title="Change provider prompt caching?"
      confirmLabel="Save prompt caching"
      effect="The gateway marks the start of long prompts so providers that support it bill repeated prefixes at the cached price."
      onConfirm={async (reason) => {
        await modelsApi.patchPolicies(
          { cache: { prompt_caching: { ...(caching ?? {}), auto_inject: on, ttl } } },
          etag,
          reason,
        );
        notify('success', 'Prompt caching saved');
        onSaved();
      }}
    >
      <Switch checked={on} onChange={setOn} label="Mark cacheable prefixes for providers that support it" />
      <Field label="Cache for">
        <Select
          value={ttl}
          onChange={(v) => setTtl(v as '5m' | '1h')}
          options={[
            { value: '5m', label: '5 minutes' },
            { value: '1h', label: '1 hour' },
          ]}
        />
      </Field>
    </ConfirmDialog>
  );
}

/**
 * The catalog's policy: the default Model per endpoint, retries and fallback, provider prompt caching,
 * PII redaction before providers, allowed regions, and the last change to them.
 */
function CatalogPolicy({ views }: { views: ModelView[] }) {
  const scope = useScope((s) => s.scope);
  const org = scopeOrganization(scope);
  const routing = useCatalog((s) => s.routing);
  const audit = useCatalog((s) => s.catalogAudit.data);
  const canPolicies = useCan('getPoliciesSettings');
  const canEditPolicies = useCan('patchPoliciesSettings');
  const canEditRouting = useCan('patchRoutingSettings');
  const canOrgs = useCan('listOrganizations');
  const policies = useRead<PoliciesSettings>(canPolicies, modelsApi.policies);
  const guardrails = useRead<GuardrailResource[]>(canPolicies, modelsApi.guardrails);
  const orgs = useRead<OrganizationResource[]>(canOrgs, modelsApi.organizations);
  const safetyHref = useScopedHref('/safety/');
  const [dialog, setDialog] = useState<'defaults' | 'retry' | 'caching' | null>(null);
  const defaults = routing.data?.spec.default_models ?? {};
  const retry = (policies.data?.spec.resilience?.retry ?? {}) as Retry;
  const caching = policies.data?.spec.cache?.prompt_caching;
  const inputGuards = policies.data?.spec.guardrails?.input ?? [];
  const pii = (guardrails.data ?? []).filter(
    (g) => inputGuards.includes(g.name) && g.spec.type === 'pii' && g.spec.enabled !== false,
  );
  const regionOrgs = (orgs.data ?? []).filter((o) => (org ? o.name === org : true));
  const restricted = regionOrgs.filter(
    (o) => (o.spec.regions ?? []).length > 0 || (o.spec.deny_regions ?? []).length > 0,
  );
  const last = (audit ?? []).find(
    (e) => e.resource === 'settings/routing' || e.resource === 'settings/policies',
  );

  return (
    <Panel
      className="border-b border-line"
      title="Catalog policy"
      subtitle="what applies to every model unless a narrower level sets it"
    >
      {routing.error ? <ErrorState error={routing.error} subject="Routing settings" size="sm" /> : null}
      <dl>
        <PolicyRow
          label="Default models"
          action={
            canEditRouting ? (
              <Button size="sm" variant="ghost" icon="edit" onClick={() => setDialog('defaults')}>
                Edit
              </Button>
            ) : null
          }
        >
          {Object.keys(defaults).length === 0 ? (
            <span className="text-muted">None: requests must name a model.</span>
          ) : (
            <ul className="flex flex-col gap-0.5">
              {Object.entries(defaults).map(([e, m]) => (
                <li key={e}>
                  <span className="text-muted">{endpointLabel(e)}</span> →{' '}
                  <span className="mono text-accent">{m}</span>
                </li>
              ))}
            </ul>
          )}
        </PolicyRow>
        <PolicyRow
          label="Retries and fallback"
          action={
            canEditPolicies && policies.data ? (
              <Button size="sm" variant="ghost" icon="edit" onClick={() => setDialog('retry')}>
                Edit
              </Button>
            ) : null
          }
        >
          {!canPolicies ? (
            <span className="text-muted">Needs access to policies.</span>
          ) : policies.error ? (
            <span className="text-muted">Couldn’t be read.</span>
          ) : !policies.data ? (
            <span className="text-dim">…</span>
          ) : (
            <>
              <span className={retry.fallback === false ? 'text-warn' : 'text-success'}>
                {retry.fallback === false ? 'No fallback' : 'Falls back to the next deployment'}
              </span>
              <span className="text-muted">
                {' '}
                · up to {plural(retry.max_attempts ?? 5, 'attempt')} · backoff{' '}
                {retry.backoff?.initial ?? '500ms'} to {retry.backoff?.max ?? '8s'}
              </span>
            </>
          )}
        </PolicyRow>
        <PolicyRow
          label="Provider prompt caching"
          action={
            canEditPolicies && policies.data ? (
              <Button size="sm" variant="ghost" icon="edit" onClick={() => setDialog('caching')}>
                Edit
              </Button>
            ) : null
          }
        >
          {!policies.data ? (
            <span className="text-dim">{canPolicies ? '…' : 'Needs access to policies.'}</span>
          ) : caching?.auto_inject ? (
            <span>
              <span className="text-success">On</span>
              <span className="text-muted">
                {' '}
                · cached for {caching.ttl === '1h' ? '1 hour' : '5 minutes'} · where the provider supports it
              </span>
            </span>
          ) : (
            <span className="text-muted">Off: only prompts that mark their own prefixes are cached.</span>
          )}
        </PolicyRow>
        <PolicyRow
          label="PII before providers"
          action={
            <Link href={safetyHref} className="text-caption text-accent hover:underline">
              Safety & PII
            </Link>
          }
        >
          {!policies.data ? (
            <span className="text-dim">{canPolicies ? '…' : 'Needs access to policies.'}</span>
          ) : pii.length === 0 ? (
            <span className="text-muted">Sent as written: no gateway-wide PII guardrail.</span>
          ) : (
            <span>
              {pii.some((g) => g.spec.action === 'redact') ? (
                <span className="text-success">Redacted</span>
              ) : (
                <span>Checked</span>
              )}
              <span className="text-muted"> by {pii.map((g) => g.name).join(', ')}</span>
            </span>
          )}
        </PolicyRow>
        <PolicyRow label="Allowed regions">
          {!canOrgs ? (
            <span className="text-muted">Needs access to organizations.</span>
          ) : !orgs.data ? (
            <span className="text-dim">…</span>
          ) : restricted.length === 0 ? (
            <span className="text-muted">Any region{org ? '' : ' in every organization'}.</span>
          ) : (
            <ul className="flex flex-col gap-0.5">
              {restricted.map((o) => (
                <li key={o.name}>
                  {!org ? <span className="text-muted">{o.name}: </span> : null}
                  {(o.spec.regions ?? []).length > 0 ? (
                    <span className="mono">{(o.spec.regions ?? []).join(', ')}</span>
                  ) : (
                    <span className="text-muted">any</span>
                  )}
                  {(o.spec.deny_regions ?? []).length > 0 ? (
                    <span className="text-muted"> · not {(o.spec.deny_regions ?? []).join(', ')}</span>
                  ) : null}
                </li>
              ))}
            </ul>
          )}
        </PolicyRow>
        <PolicyRow label="Last change">
          {last ? (
            <span>
              {last.resource === 'settings/routing' ? 'Routing settings' : 'Policies'} by{' '}
              <span>{auditActorBy(last.actor)}</span>{' '}
              <span className="text-muted">{formatRelative(last.time)}</span>
            </span>
          ) : (
            <span className="text-muted">No change recorded.</span>
          )}
        </PolicyRow>
      </dl>
      {dialog === 'defaults' ? <DefaultsDialog views={views} onClose={() => setDialog(null)} /> : null}
      {dialog === 'retry' ? (
        <RetryDialog
          retry={retry}
          etag={policies.data?.metadata.etag}
          onClose={() => setDialog(null)}
          onSaved={policies.reload}
        />
      ) : null}
      {dialog === 'caching' ? (
        <PromptCachingDialog
          caching={caching}
          etag={policies.data?.metadata.etag}
          onClose={() => setDialog(null)}
          onSaved={policies.reload}
        />
      ) : null}
    </Panel>
  );
}

const TIER_NAME = /^[a-z][a-z0-9_-]{0,31}$/;

/** Adds a custom tier or edits one's description (`routing.tiers`). */
function TierDialog({ name, onClose }: { name: string | null; onClose: () => void }) {
  const tiers = useCatalog((s) => s.tiers.data ?? []);
  const existing = tiers.find((t) => t.name === name);
  const [value, setValue] = useState(name ?? '');
  const [description, setDescription] = useState(existing?.description ?? '');
  const [invalid, setInvalid] = useState<string | null>(null);
  return (
    <ConfirmDialog
      open
      onClose={onClose}
      tone="primary"
      title={name ? `Edit the ${tierLabel(name)} tier` : 'Add a tier'}
      confirmLabel={name ? 'Save tier' : 'Add tier'}
      effect="A tier groups models of one kind; rules and aliases can send requests to a tier."
      onConfirm={async (reason) => {
        const n = value.trim();
        const problem = !TIER_NAME.test(n)
          ? 'Use lowercase letters, digits, - and _, starting with a letter (up to 32)'
          : !name && tiers.some((t) => t.name === n)
            ? 'A tier already has this name'
            : null;
        setInvalid(problem);
        if (problem) throw new Error(problem);
        await patchRouting(
          { tiers: { [n]: description.trim() ? { description: description.trim() } : {} } },
          reason,
        );
        notify('success', name ? 'Tier saved' : 'Tier added', tierLabel(n));
      }}
    >
      <Field label="Name" error={invalid ?? undefined} hint="Models name it in their tier field.">
        <Input
          mono
          value={value}
          onChange={(e) => setValue(e.target.value)}
          disabled={name !== null}
          data-autofocus
        />
      </Field>
      <Field label="Description" optional>
        <Input value={description} onChange={(e) => setDescription(e.target.value)} />
      </Field>
    </ConfirmDialog>
  );
}

/** Built-in and custom tiers with their Models; custom ones are added, edited and removed here. */
function TiersPanel() {
  const tiers = useCatalog((s) => s.tiers);
  const canEdit = useCan('patchRoutingSettings');
  const [dialog, setDialog] = useState<
    { kind: 'edit'; name: string | null } | { kind: 'remove'; name: string } | null
  >(null);
  const builtin = new Set<string>(BUILTIN_TIERS);
  return (
    <Panel
      title="Tiers"
      subtitle="kinds of model that rules and aliases can target"
      actions={
        <Gate operation="patchRoutingSettings" mode="hide">
          <Button size="sm" icon="plus" onClick={() => setDialog({ kind: 'edit', name: null })}>
            Add tier
          </Button>
        </Gate>
      }
    >
      {tiers.status === 'loading' && !tiers.data ? <LoadingRows rows={3} /> : null}
      {tiers.error ? <ErrorState error={tiers.error} subject="Tiers" size="sm" /> : null}
      {tiers.data ? (
        <Table label="Tiers">
          <thead>
            <tr>
              <Th>Tier</Th>
              <Th>Description</Th>
              <Th numeric>Models</Th>
              {canEdit ? (
                <Th className="w-12">
                  <span className="sr-only">Actions</span>
                </Th>
              ) : null}
            </tr>
          </thead>
          <tbody>
            {tiers.data.map((t) => {
              const custom = !t.builtin && !builtin.has(t.name);
              return (
                <Tr key={t.name}>
                  <Td>
                    <span className="flex items-center gap-1.5">
                      {tierLabel(t.name)}
                      {custom ? <Badge tone="accent">Custom</Badge> : null}
                    </span>
                  </Td>
                  <Td muted>{t.description || '—'}</Td>
                  <Td numeric>{formatCount(t.models)}</Td>
                  {canEdit ? (
                    <Td>
                      {custom ? (
                        <MenuButton
                          iconOnly
                          icon="more"
                          variant="ghost"
                          size="sm"
                          align="right"
                          label={`Actions for the ${t.name} tier`}
                          items={[
                            {
                              label: 'Edit',
                              icon: 'edit',
                              onSelect: () => setDialog({ kind: 'edit', name: t.name }),
                            },
                            {
                              label: 'Remove',
                              icon: 'trash',
                              danger: true,
                              disabled: t.models > 0,
                              disabledReason: 'Models still use it',
                              onSelect: () => setDialog({ kind: 'remove', name: t.name }),
                            },
                          ]}
                        />
                      ) : null}
                    </Td>
                  ) : null}
                </Tr>
              );
            })}
          </tbody>
        </Table>
      ) : null}
      {dialog?.kind === 'edit' ? <TierDialog name={dialog.name} onClose={() => setDialog(null)} /> : null}
      {dialog?.kind === 'remove' ? (
        <ConfirmDialog
          open
          onClose={() => setDialog(null)}
          title={`Remove the ${tierLabel(dialog.name)} tier?`}
          objectName={dialog.name}
          confirmLabel="Remove tier"
          effect="No model uses it. Rules or aliases that target it must be changed first, or the gateway refuses the change."
          onConfirm={async (reason) => {
            await patchRouting({ tiers: { [dialog.name]: null } }, reason);
            notify('success', 'Tier removed', tierLabel(dialog.name));
          }}
        />
      ) : null}
    </Panel>
  );
}

const WARN_AT = 0.8;

interface Reading {
  used: number;
  limit: number;
  share: number;
}

/** A provider's last upstream reading for one unit, as used of limit; null when it reports none. */
export function readingOf(p: ProviderStatus | undefined, unit: 'requests' | 'tokens'): Reading | null {
  const r = p?.rate_limits?.[unit];
  if (r?.limit == null || r.limit <= 0 || r.remaining == null) return null;
  const used = Math.max(0, r.limit - r.remaining);
  return { used, limit: r.limit, share: used / r.limit };
}

/** The last minute's requests and tokens per provider, from the gateway's own records. */
function useLastMinute(enabled: boolean): Map<string, { requests: number; tokens: number }> | null {
  const [map, setMap] = useState<Map<string, { requests: number; tokens: number }> | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    const load = () => {
      const to = new Date();
      api
        .get('/analytics/usage', {
          query: {
            from: new Date(to.getTime() - 60_000).toISOString(),
            to: to.toISOString(),
            interval: '1m',
            group_by: ['provider'],
          },
        })
        .then(
          (r) => {
            if (!live) return;
            const m = new Map<string, { requests: number; tokens: number }>();
            for (const b of r.items) {
              const name = b.group.provider;
              if (!name) continue;
              const prev = m.get(name) ?? { requests: 0, tokens: 0 };
              m.set(name, {
                requests: prev.requests + b.requests,
                tokens: prev.tokens + (b.input ?? 0) + (b.output ?? 0),
              });
            }
            setMap(m);
          },
          () => live && setMap(new Map()),
        );
    };
    load();
    const timer = setInterval(load, 15_000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [enabled]);
  return map;
}

/**
 * Each provider's headroom: requests and tokens per minute against the limits it last reported,
 * with its account tier, warning from 80 %; the gateway's own last-minute rate when it reports none.
 */
function HeadroomPanel({ views }: { views: ModelView[] }) {
  const canStatus = useCan('listProviderStatus');
  const canUsage = useCan('getUsageAnalytics');
  const providers = useProviderMap();
  const providersHref = useScopedHref('/providers/');
  const status = useRead<ProviderStatus[]>(canStatus, modelsApi.providerStatus);
  const minute = useLastMinute(canUsage);
  const peak = {
    requests: Math.max(1, ...[...(minute?.values() ?? [])].map((m) => m.requests)),
    tokens: Math.max(1, ...[...(minute?.values() ?? [])].map((m) => m.tokens)),
  };
  const names = [...new Set([...providers.keys(), ...views.flatMap((v) => v.providers)])].sort();
  useEffect(() => {
    if (!canStatus) return;
    const timer = setInterval(status.reload, 15_000);
    return () => clearInterval(timer);
  }, [canStatus, status.reload]);

  const bar = (
    name: string,
    unit: 'requests' | 'tokens',
    reading: Reading | null,
    own: number | undefined,
  ) => {
    const tone = reading
      ? reading.share >= 0.95
        ? 'error'
        : reading.share >= WARN_AT
          ? 'warn'
          : 'accent'
      : 'muted';
    const noun = unit === 'requests' ? 'requests' : 'tokens';
    return (
      <BarRow
        key={unit}
        name={
          <span className="text-caption text-muted">
            {unit === 'requests' ? 'Requests a minute' : 'Tokens a minute'}
          </span>
        }
        value={reading ? reading.used : (own ?? 0)}
        max={reading ? reading.limit : peak[unit]}
        tone={tone}
        label={
          reading
            ? `${name}: ${formatCount(reading.used)} of ${formatCount(reading.limit)} ${noun} a minute used upstream`
            : `${name}: ${formatCount(own ?? 0)} ${noun} in the last minute; the provider reports no limit`
        }
        display={
          reading ? (
            <Tooltip
              content={`Last reading from the provider: ${formatCount(reading.used)} of ${formatCount(reading.limit)} ${noun} a minute`}
            >
              <span className={tone === 'error' ? 'text-error' : tone === 'warn' ? 'text-warn' : undefined}>
                {formatCount(reading.used, { compact: true })}
                <span className="text-dim"> / {formatCount(reading.limit, { compact: true })}</span>
                <span className="ml-2 inline-block w-9 text-right">{formatPercent(reading.share, 0)}</span>
              </span>
            </Tooltip>
          ) : (
            <Tooltip content="The provider reports no limit; this is the gateway's own rate over the last minute">
              <span className="text-muted">
                {own === undefined ? '—' : formatCount(own, { compact: true })}
                <span className="ml-2 inline-block w-9 text-right text-dim">—</span>
              </span>
            </Tooltip>
          )
        }
      />
    );
  };

  return (
    <Panel
      className="h-full"
      title="Provider headroom"
      subtitle="last 60 seconds against upstream limits"
      actions={
        <Link href={providersHref} className="text-caption text-muted hover:text-fg">
          View providers
        </Link>
      }
    >
      {!canStatus ? <p className="text-ui text-muted">Needs access to providers.</p> : null}
      {status.error ? <ErrorState error={status.error} subject="Provider readings" size="sm" /> : null}
      {canStatus && !status.data && !status.error ? <LoadingRows rows={3} /> : null}
      {status.data ? (
        <ul aria-label="Provider headroom" className="flex flex-col gap-3">
          {names.map((name) => {
            const s = status.data?.find((p) => p.name === name);
            const own = minute?.get(name);
            const requests = readingOf(s, 'requests');
            const tokens = readingOf(s, 'tokens');
            const worst = Math.max(requests?.share ?? 0, tokens?.share ?? 0);
            return (
              <li key={name} className="flex flex-col gap-0.5">
                <div className="flex items-center gap-2 text-ui">
                  <ProviderMark type={providers.get(name)?.spec.type} className="text-muted" />
                  <span className="text-fg">{name}</span>
                  {s?.account_tier ? <span className="text-caption text-dim">· {s.account_tier}</span> : null}
                  {!requests && !tokens ? (
                    <span className="text-caption text-dim">· reports no limits</span>
                  ) : null}
                  {worst >= WARN_AT ? (
                    <span className="ml-auto flex items-center gap-1 text-caption text-warn">
                      <Dot tone="warn" /> Near the limit
                    </span>
                  ) : null}
                </div>
                <ul className="flex flex-col">
                  {bar(name, 'requests', requests, own?.requests)}
                  {bar(name, 'tokens', tokens, own?.tokens)}
                </ul>
              </li>
            );
          })}
        </ul>
      ) : null}
    </Panel>
  );
}

/** The catalog's policy, tiers and provider headroom. */
export function PolicyTab({ views }: { views: ModelView[] }) {
  return (
    <TileGrid
      label="Catalog policy"
      cols={{ base: 1, xl: 2 }}
      template="xl:grid-cols-[minmax(0,1.3fr)_minmax(0,1fr)]"
    >
      <div className="flex min-w-0 flex-col bg-raised">
        <CatalogPolicy views={views} />
        <TiersPanel />
      </div>
      <HeadroomPanel views={views} />
    </TileGrid>
  );
}
