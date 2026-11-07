'use client';

import { useEffect, useMemo, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button, IconButton } from '@/components/ui/button';
import { Meter } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { useCan } from '@/components/ui/gate';
import { Field, Input, NumberInput, Segmented, Select, Textarea } from '@/components/ui/form';
import { Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { ProviderMark } from '@/components/ui/marks';
import { formatCount, formatPercent } from '@/lib/format';
import { useEdited, useNow } from '@/lib/hooks';
import { providerTypeLabel } from '@/lib/providers';
import type { DeploymentResource, ProviderResource } from '@/lib/api/types';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';

import { providersApi, type ProviderStatus, type ProviderTestResult } from '../api';
import {
  baseUrlChanged,
  connectionChanged,
  draftErrors,
  editedSpec,
  fromProvider,
  keyTestSpec,
  mixedUpKeys,
  NEEDS_BASE_URL,
  newKey,
  type KeyDraft,
  type ProviderDraft,
  ROTATION_CHOICES,
  type SettingsDraft,
  shares,
  toPatch,
  TYPE_BLOCKS,
} from '../edit';
import { AUTH_LABELS, type AuthMethod, specWithKey } from '../form';
import { testFailure, testSummary } from '../test-result';
import { passed, TestConnection, type TestState } from './test-connection';
import {
  ageText,
  type KeyState,
  providerKeys,
  type ProviderKeyRow,
  type RateLimits,
  rotation,
} from '../keys';
import { useProviders } from '../store';
import { api, ApiError, describeError, isChangeControl } from '@/lib/api/client';
import { DeploymentDrawer } from '@/features/models/components/deployment-drawer';
import { collectLeaves } from '@/features/models/types';
import { hasGatewayPermission, useSession } from '@/stores/session';
import { translateNow } from '@/lib/i18n';

/** The deployments a Model routes to (its target or route, followed into shared routes). */
function useUsedDeployments(open: boolean): Set<string> | null {
  const me = useSession((s) => s.me);
  const may = hasGatewayPermission(me, 'models:read');
  const [used, setUsed] = useState<Set<string> | null>(null);
  useEffect(() => {
    if (!open || !may) return;
    let live = true;
    Promise.all([
      api.get('/models', { query: { limit: 500 } }),
      api.get('/routes', { query: { limit: 500 } }),
    ]).then(
      ([models, routes]) => {
        const map = new Map(routes.items.map((r) => [r.name, r]));
        const out = new Set<string>();
        for (const m of models.items) {
          const tree = (m.spec.target ?? (m.spec.route ? { route: m.spec.route } : null)) as Parameters<
            typeof collectLeaves
          >[0];
          for (const leaf of collectLeaves(tree, map)) out.add(leaf);
        }
        if (live) setUsed(out);
      },
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, [open, may]);
  return used;
}

/**
 * The provider's deployments, each with the upstream model it calls and whether a Model routes to
 * it; one no Model uses (left by a deleted Model) can be deleted here, by typing its name.
 */
function DeploymentList({ provider, type }: { provider: string; type: string | undefined }) {
  const deployments = useProviders((s) => s.deployments.data);
  const load = useProviders((s) => s.load);
  const canDelete = useCan('deleteDeployment');
  const canEdit = useCan('replaceDeployment');
  const [editing, setEditing] = useState<DeploymentResource | null>(null);
  const used = useUsedDeployments(true);
  const [deleting, setDeleting] = useState<string | null>(null);
  const mine = (deployments ?? []).filter((d) => d.spec.provider === provider);
  if (mine.length === 0) return null;
  return (
    <section className="flex flex-col gap-2">
      <h3 className="text-ui font-semibold text-fg">Deployments ({String(mine.length)})</h3>
      <ul className="flex flex-col border-y border-line">
        {mine.map((d) => {
          const orphan = used !== null && !used.has(d.name);
          return (
            <li
              key={d.name}
              className="flex items-center gap-3 border-b border-line py-2 text-ui last:border-0"
            >
              <span className="min-w-0 flex-1">
                <span className="mono block truncate text-fg">{d.name}</span>
                <span className="mono block truncate text-caption text-muted">{d.spec.model}</span>
              </span>
              {orphan ? <Badge tone="warn">No Model uses it</Badge> : null}
              {d.spec.enabled === false ? <Badge tone="error">Off</Badge> : null}
              {canEdit ? (
                <Button size="sm" variant="ghost" icon="edit" onClick={() => setEditing(d)}>
                  Edit
                </Button>
              ) : null}
              {canDelete ? (
                <Button size="sm" variant="ghost" icon="trash" onClick={() => setDeleting(d.name)}>
                  Delete
                </Button>
              ) : null}
            </li>
          );
        })}
      </ul>
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title={`Delete deployment ${deleting ?? ''}?`}
        objectName={deleting ?? undefined}
        typeToConfirm={deleting ?? undefined}
        confirmLabel="Delete deployment"
        effect={
          deleting && used?.has(deleting)
            ? 'A Model still routes to it: its requests move on to its other deployments, or fail when it has none. Its prices go with it. This can’t be undone.'
            : 'No Model routes to it, so no request is affected. Its prices go with it. This can’t be undone.'
        }
        onConfirm={async () => {
          if (!deleting) return;
          await api.delete('/deployments/{name}', { path: { name: deleting } });
          notify('success', 'Deployment deleted', deleting);
          await load({ quiet: true });
        }}
      />
      {editing ? (
        <DeploymentDrawer
          deployment={editing}
          providerType={type}
          onClose={() => setEditing(null)}
          onSaved={() => void load({ quiet: true })}
        />
      ) : null}
    </section>
  );
}

const HEALTH: Record<KeyState['state'], { tone: 'success' | 'warn' | 'error' | 'neutral'; label: string }> = {
  working: { tone: 'success', label: 'Working' },
  resting: { tone: 'warn', label: 'Cooling down' },
  not_working: { tone: 'error', label: 'Not working' },
  untested: { tone: 'neutral', label: 'Not tested yet' },
  unknown: { tone: 'neutral', label: 'Not used yet' },
};

const DEAD_REASON: Record<string, string> = {
  credential: 'the provider refused it',
  quota: 'its quota ran out',
  permission: "it isn't allowed to use this model",
  test: 'its last test failed',
};

function Limit({ label, value }: { label: string; value: RateLimits['requests'] }) {
  const now = useNow(15_000);
  if (value?.limit === null || value?.limit === undefined) return null;
  const remaining = value.remaining ?? null;
  const used = remaining === null ? null : value.limit - remaining;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-3 text-caption">
        <span className="text-muted">{label}</span>
        <span className="mono text-fg">
          {remaining === null ? '—' : formatCount(remaining)} of {formatCount(value.limit)} left
          {value.reset_at && Date.parse(value.reset_at) > now ? (
            <span className="text-muted"> · resets {formatRelative(value.reset_at)}</span>
          ) : null}
        </span>
      </div>
      {used !== null ? (
        <Meter
          value={used}
          max={value.limit}
          tone={used / value.limit >= 0.8 ? 'warn' : 'accent'}
          label={`${label} used against the provider's limit`}
        />
      ) : null}
    </div>
  );
}

function Readings({ readings }: { readings: RateLimits | null }) {
  if (!readings)
    return (
      <p className="text-caption text-muted">
        The provider hasn&apos;t reported its limits for this key yet.
      </p>
    );
  return (
    <div className="flex flex-col gap-2">
      <Limit label="Requests" value={readings.requests} />
      <Limit label="Tokens" value={readings.tokens} />
      <Limit label="Input tokens" value={readings.input_tokens} />
      <Limit label="Output tokens" value={readings.output_tokens} />
      {readings.observed_at ? (
        <p className="text-caption text-dim">
          Reported {formatRelative(readings.observed_at)}
          {readings.stale ? ' · older than 5 minutes, so it may have changed' : ''}
        </p>
      ) : null}
    </div>
  );
}

/** A key's state line: what it is, and until when or why. */
function KeyStateLine({ row }: { row: ProviderKeyRow }) {
  if (row.health.state !== 'not_working' && row.health.state !== 'resting') return null;
  return (
    <p className="text-caption text-muted">
      {row.health.reason ? `Set aside because ${DEAD_REASON[row.health.reason] ?? row.health.reason}. ` : ''}
      {row.health.until
        ? `${row.health.state === 'resting' ? 'Used again' : 'Tried again'} ${formatRelative(row.health.until)}.`
        : ''}
    </p>
  );
}

/** "75% of requests", or that it waits as a standby. */
function ShareText({ share, weight }: { share: number; weight: string }) {
  return (
    <span className="text-caption text-muted">
      {(Number(weight) || 0) === 0 ? 'Standby: takes no requests' : `${formatPercent(share, 0)} of requests`}
    </span>
  );
}

/** A key's name and weight, editable once the provider has more than one key. */
function KeyFields({
  draft,
  editing,
  errors,
  share,
  onChange,
}: {
  draft: KeyDraft;
  editing: boolean;
  errors: Record<string, string>;
  share: number;
  onChange: (patch: Partial<KeyDraft>) => void;
}) {
  if (!editing) return null;
  return (
    <div className="grid gap-3 sm:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
      <Field label="Name" error={errors[`keys.${draft.id}.name`]}>
        <Input
          mono
          autoComplete="off"
          value={draft.name}
          onChange={(e) => onChange({ name: e.target.value })}
        />
      </Field>
      <Field
        label="Weight"
        error={errors[`keys.${draft.id}.weight`]}
        hint={<ShareText share={share} weight={draft.weight} />}
      >
        <NumberInput mono min={0} value={draft.weight} onChange={(v) => onChange({ weight: v })} />
      </Field>
    </div>
  );
}

function KeyCard({
  provider,
  row,
  draft,
  editing,
  errors,
  share,
  maxDays,
  onChange,
  onRemove,
  removeReason,
  onReplace,
}: {
  provider: string;
  row: ProviderKeyRow;
  draft: KeyDraft;
  editing: boolean;
  errors: Record<string, string>;
  share: number;
  maxDays: number | null;
  onChange: (patch: Partial<KeyDraft>) => void;
  onRemove: (() => void) | null;
  removeReason: string | undefined;
  onReplace: ((key: string) => void) | null;
}) {
  const resetKey = useProviders((s) => s.resetKey);
  const testKey = useProviders((s) => s.testKey);
  const canReset = useCan('resetProviderKey');
  const canTest = useCan('testProvider');
  const [testing, setTesting] = useState(false);
  const now = useNow(60_000);
  const h = HEALTH[row.health.state];
  const age = row.rotatedAt ? (now - Date.parse(row.rotatedAt)) / 86_400_000 : null;
  const ageShare = age !== null && maxDays ? age / maxDays : null;
  const renamed = draft.saved !== null && draft.name.trim() !== draft.saved;
  return (
    <li className="flex flex-col gap-3 rounded-control border border-line px-3.5 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span className="mono text-body text-fg">{row.name}</span>
          {row.hint ? <span className="mono text-caption text-muted">{row.hint}</span> : null}
          {renamed ? <span className="text-caption text-dim">renamed to {draft.name.trim()}</span> : null}
        </div>
        <span className="flex items-center gap-1.5 text-ui">
          <Dot tone={h.tone} />
          {h.label}
        </span>
      </div>
      <KeyStateLine row={row} />
      <KeyFields draft={draft} editing={editing} errors={errors} share={share} onChange={onChange} />
      <div className="flex flex-col gap-1">
        <div className="flex items-baseline justify-between gap-3 text-caption">
          <span className="text-muted">Last replaced</span>
          <span className="text-fg">
            {age === null ? 'Not recorded' : age < 1 ? 'Today' : `${ageText(age)} ago`}
            {maxDays ? <span className="text-muted"> · replace every {Math.round(maxDays)} days</span> : null}
          </span>
        </div>
        {ageShare !== null ? (
          <Meter
            value={Math.min(ageShare, 1)}
            max={1}
            tone={ageShare >= 1 ? 'error' : ageShare >= 0.85 ? 'warn' : 'accent'}
            label="Age of the key against the rotation policy"
          />
        ) : null}
      </div>
      <Readings readings={row.readings} />
      <div className="flex flex-wrap gap-2">
        {onReplace ? (
          <Button size="sm" icon="rotate" onClick={() => onReplace(row.name)}>
            Replace key
          </Button>
        ) : null}
        {canTest ? (
          <Button
            size="sm"
            icon="play"
            busy={testing}
            onClick={() => {
              setTesting(true);
              void testKey(provider, row.name)
                .then(
                  (r) =>
                    r.ok
                      ? notify('success', translateNow('{0} answered', row.name), testSummary(r))
                      : notify('error', translateNow("{0} didn't pass the test", row.name), testFailure(r)),
                  (e: unknown) =>
                    notify('error', translateNow("Couldn't test {0}", row.name), describeError(e)),
                )
                .finally(() => setTesting(false));
            }}
          >
            Test key
          </Button>
        ) : null}
        {canReset && (row.health.state === 'not_working' || row.health.state === 'resting') ? (
          <Button
            size="sm"
            icon="refresh"
            onClick={() =>
              void resetKey(provider, row.name).then(
                () => notify('success', 'Key back in use', `${provider} · ${row.name}`),
                (e: unknown) =>
                  notify('error', translateNow("Couldn't reset {0}", row.name), describeError(e)),
              )
            }
          >
            Use it again now
          </Button>
        ) : null}
        {onRemove ? (
          <Button
            size="sm"
            variant="ghost"
            icon="trash"
            className="ml-auto"
            disabled={removeReason !== undefined}
            disabledReason={removeReason ?? ''}
            onClick={onRemove}
          >
            Remove
          </Button>
        ) : null}
      </div>
    </li>
  );
}

/** A key added in the drawer: its name, weight and secret, and the test it must pass. */
function NewKeyCard({
  draft,
  errors,
  share,
  test,
  of,
  testing,
  onChange,
  onTest,
  onRemove,
}: {
  draft: KeyDraft;
  errors: Record<string, string>;
  share: number;
  test: TestState | null;
  of: string;
  testing: boolean;
  onChange: (patch: Partial<KeyDraft>) => void;
  onTest: () => void;
  onRemove: () => void;
}) {
  return (
    <li className="flex flex-col gap-3 rounded-control border border-dashed border-line-strong px-3.5 py-3">
      <div className="flex items-center justify-between gap-2">
        <span className="text-ui font-medium text-fg">New key</span>
        <IconButton icon="x" label={`Don’t add ${draft.name || 'this key'}`} size={14} onClick={onRemove} />
      </div>
      <KeyFields draft={draft} editing errors={errors} share={share} onChange={onChange} />
      <Field
        label="API key"
        required
        error={errors[`keys.${draft.id}.secret`]}
        hint="Stored encrypted and never shown again. A reference such as ${env:NAME} also works."
      >
        <Input
          mono
          type="password"
          autoComplete="off"
          value={draft.secret}
          onChange={(e) => onChange({ secret: e.target.value })}
        />
      </Field>
      <TestConnection
        test={test}
        of={of}
        busy={testing}
        disabled={draft.secret.trim() === ''}
        label="Test this key"
        onTest={onTest}
      />
    </li>
  );
}

/** Header rows: a name and a value each; the values aren't secret, so they show. */
function HeaderRows({
  rows,
  errors,
  onChange,
}: {
  rows: SettingsDraft['headers'];
  errors: Record<string, string>;
  onChange: (rows: SettingsDraft['headers']) => void;
}) {
  const update = (i: number, patch: Partial<SettingsDraft['headers'][number]>) =>
    onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div role="group" aria-label="Extra headers" className="flex flex-col gap-2">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-ui font-medium text-fg">
          Extra headers <span className="font-normal text-dim">(optional)</span>
        </span>
        <Button
          size="sm"
          variant="ghost"
          icon="plus"
          onClick={() => onChange([...rows, { name: '', value: '' }])}
        >
          Add header
        </Button>
      </div>
      {rows.map((row, i) => (
        <div key={i} className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)_auto] items-start gap-2">
          <Input
            mono
            aria-label={`Header ${String(i + 1)} name`}
            aria-invalid={errors[`headers.${String(i)}`] ? true : undefined}
            placeholder="x-team"
            value={row.name}
            onChange={(e) => update(i, { name: e.target.value })}
          />
          <Input
            mono
            aria-label={`Header ${String(i + 1)} value`}
            placeholder="Value or ${env:NAME}"
            value={row.value}
            onChange={(e) => update(i, { value: e.target.value })}
          />
          <IconButton
            icon="trash"
            label={`Remove header ${row.name || String(i + 1)}`}
            onClick={() => onChange(rows.filter((_, j) => j !== i))}
          />
        </div>
      ))}
      <p className="text-caption text-muted">
        {rows.length === 0
          ? 'None. Headers added here go with every request to the provider.'
          : 'Sent with every request to the provider. Sign-in headers are set by the key, not here.'}
      </p>
    </div>
  );
}

/** The provider's settings: where its API is, display facts, and what its type takes. */
function SettingsFields({
  type,
  defaultUrl,
  draft,
  errors,
  disabled,
  onChange,
}: {
  type: string;
  defaultUrl: string | null | undefined;
  draft: SettingsDraft;
  errors: Record<string, string>;
  disabled: boolean;
  onChange: (patch: Partial<SettingsDraft>) => void;
}) {
  const rotationOptions = ROTATION_CHOICES.some((c) => c.value === draft.rotationMaxAge)
    ? ROTATION_CHOICES
    : [...ROTATION_CHOICES, { value: draft.rotationMaxAge, label: draft.rotationMaxAge }];
  const needsUrl = NEEDS_BASE_URL.includes(type);
  return (
    <fieldset disabled={disabled} className="grid min-w-0 gap-4 sm:grid-cols-2">
      <Field
        label="Base URL"
        required={needsUrl}
        optional={!needsUrl}
        error={errors.baseUrl}
        className="sm:col-span-2"
        hint={
          needsUrl
            ? 'The address of the provider’s API, including any path such as /v1.'
            : `Leave blank to use ${defaultUrl ?? 'the provider’s own address'}.`
        }
      >
        <Input
          mono
          inputMode="url"
          value={draft.baseUrl}
          onChange={(e) => onChange({ baseUrl: e.target.value })}
        />
      </Field>
      {TYPE_BLOCKS.openai(type) ? (
        <>
          <Field label="Organization ID" optional>
            <Input
              mono
              placeholder="org-…"
              value={draft.openaiOrganization}
              onChange={(e) => onChange({ openaiOrganization: e.target.value })}
            />
          </Field>
          <Field label="Project ID" optional>
            <Input
              mono
              placeholder="proj_…"
              value={draft.openaiProject}
              onChange={(e) => onChange({ openaiProject: e.target.value })}
            />
          </Field>
        </>
      ) : null}
      {TYPE_BLOCKS.azure(type) ? (
        <>
          <Field label="API" className="sm:col-span-2">
            <Segmented<'v1' | 'deployments'>
              label="API"
              value={draft.azureApiStyle}
              onChange={(v) => onChange({ azureApiStyle: v })}
              options={[
                { value: 'v1', label: 'Current API (v1)' },
                { value: 'deployments', label: 'Deployments API' },
              ]}
            />
          </Field>
          {draft.azureApiStyle === 'deployments' ? (
            <Field label="API version" required error={errors.azureApiVersion} hint="e.g. 2024-10-21">
              <Input
                mono
                value={draft.azureApiVersion}
                onChange={(e) => onChange({ azureApiVersion: e.target.value })}
              />
            </Field>
          ) : null}
        </>
      ) : null}
      {type === 'anthropic' ? (
        <Field label="Anthropic API version" optional hint="Leave blank for 2023-06-01.">
          <Input
            mono
            value={draft.anthropicVersion}
            onChange={(e) => onChange({ anthropicVersion: e.target.value })}
          />
        </Field>
      ) : null}
      {TYPE_BLOCKS.aws(type) ? (
        <Field label="AWS region" required error={errors.awsRegion} hint="e.g. us-east-1">
          <Input mono value={draft.awsRegion} onChange={(e) => onChange({ awsRegion: e.target.value })} />
        </Field>
      ) : null}
      {TYPE_BLOCKS.gcp(type) ? (
        <>
          <Field label="Google Cloud project" required error={errors.gcpProject}>
            <Input mono value={draft.gcpProject} onChange={(e) => onChange({ gcpProject: e.target.value })} />
          </Field>
          <Field label="Location" optional hint="Leave blank for global.">
            <Input
              mono
              value={draft.gcpLocation}
              onChange={(e) => onChange({ gcpLocation: e.target.value })}
            />
          </Field>
        </>
      ) : null}
      {TYPE_BLOCKS.gemini(type) ? (
        <Field label="API version">
          <Select
            value={draft.geminiApiVersion}
            onChange={(v) => onChange({ geminiApiVersion: v as SettingsDraft['geminiApiVersion'] })}
            options={[
              { value: '', label: 'Default (v1beta)' },
              { value: 'v1beta', label: 'v1beta' },
              { value: 'v1', label: 'v1' },
            ]}
          />
        </Field>
      ) : null}
      <Field
        label="Account tier"
        optional
        error={errors.accountTier}
        hint="For your reference, e.g. Tier 4. It doesn’t change how the gateway calls the provider."
      >
        <Input value={draft.accountTier} onChange={(e) => onChange({ accountTier: e.target.value })} />
      </Field>
      <Field label="Warn when a key is older than" error={errors.rotationMaxAge}>
        <Select
          value={draft.rotationMaxAge}
          onChange={(v) => onChange({ rotationMaxAge: v })}
          options={rotationOptions}
        />
      </Field>
      <div className="sm:col-span-2">
        <HeaderRows rows={draft.headers} errors={errors} onChange={(headers) => onChange({ headers })} />
      </div>
      <Field
        label="Request parameters (JSON)"
        optional
        error={errors.params}
        className="sm:col-span-2"
        hint="Changes to every request body: defaults, overrides and members to remove, by JSON pointer."
      >
        <Textarea
          mono
          rows={4}
          placeholder={'{\n  "defaults": { "/temperature": 0.2 }\n}'}
          value={draft.params}
          onChange={(e) => onChange({ params: e.target.value })}
        />
      </Field>
    </fieldset>
  );
}

/** A provider's details: type, sign-in, tier, rotation policy and each key's state, age and limits. */
export function ProviderDrawer(props: {
  /** The provider's last test in this session. */
  test?: ProviderTestResult | undefined;
  provider: ProviderResource | null;
  status: ProviderStatus | undefined;
  onClose: () => void;
  onReplace: ((name: string, key?: string) => void) | null;
  onTest: ((name: string) => void) | null;
}) {
  if (!props.provider) return null;
  return <ProviderEditor key={props.provider.name} {...props} provider={props.provider} />;
}

/**
 * The open provider, editable: its keys (add, rename, weigh, remove; a new key saves only once
 * its test passed) and its settings, saved together as one merge patch with the read's ETag.
 */
function ProviderEditor({
  provider,
  status,
  onClose,
  onReplace,
  onTest,
  test,
}: {
  test?: ProviderTestResult | undefined;
  provider: ProviderResource;
  status: ProviderStatus | undefined;
  onClose: () => void;
  onReplace: ((name: string, key?: string) => void) | null;
  onTest: ((name: string) => void) | null;
}) {
  const deployments = useProviders((s) => s.deployments);
  const health = useProviders((s) => s.health);
  const allKeyTests = useProviders((s) => s.keyTests);
  const update = useProviders((s) => s.update);
  const types = useProviders((s) => s.types.data);
  const loadTypes = useProviders((s) => s.loadTypes);
  const canEdit = useCan('patchProvider');
  const now = useNow(60_000);
  const [base, setBase] = useState(provider);
  const [draft, setDraft] = useState<ProviderDraft>(() => fromProvider(provider));
  const [newTests, setNewTests] = useState<Record<string, TestState>>({});
  const [testingKey, setTestingKey] = useState<string | null>(null);
  const [showErrors, setShowErrors] = useState(false);
  // A test of changed connection settings, with a pasted key when the saved one can't be used.
  const [settingsTest, setSettingsTest] = useState<TestState | null>(null);
  const [testingSettings, setTestingSettings] = useState(false);
  const [testKeyText, setTestKeyText] = useState('');
  const [redactedRefused, setRedactedRefused] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const edited = useEdited(draft, base.metadata.etag);
  // A newer read (a key replaced, another edit) replaces an untouched form.
  if (provider.metadata.etag !== base.metadata.etag && !edited) {
    setBase(provider);
    setDraft(fromProvider(provider));
  }

  useEffect(() => {
    void loadTypes();
  }, [loadTypes]);

  const method = status?.credential.method ?? provider.spec.auth?.method ?? 'api_key';
  const keyTests = useMemo(
    () =>
      Object.fromEntries(
        Object.entries(allKeyTests)
          .filter(([k]) => k.startsWith(`${provider.name}/`))
          .map(([k, v]) => [k.slice(provider.name.length + 1), v]),
      ),
    [allKeyTests, provider.name],
  );
  const rows = providerKeys(provider, status, deployments.data, health.data, { test, keyTests });
  const age = rotation(provider, now);
  const errors = showErrors ? draftErrors(provider.spec.type, draft) : {};
  const keys = draft.keys ?? [];
  const keyShares = shares(keys);
  const multi = keys.length > 1 || !draft.single;
  const fingerprint = (k: KeyDraft) => JSON.stringify(keyTestSpec(provider, draft, k.secret));
  const untested = keys.filter((k) => k.saved === null && !passed(newTests[k.id] ?? null, fingerprint(k)));
  const connChanged = connectionChanged(base, draft);
  const urlChanged = baseUrlChanged(base, draft);
  const signsInWithKey = draft.keys !== null;
  const needKey = signsInWithKey && (urlChanged || redactedRefused);
  const settingsSpec = needKey ? specWithKey(editedSpec(base, draft), testKeyText) : editedSpec(base, draft);
  const settingsOf = JSON.stringify(settingsSpec);
  const settingsPassed = !connChanged || passed(settingsTest, settingsOf);
  const defaultUrl = types?.find((t) => t.type === provider.spec.type)?.default_base_url;

  const setKey = (id: string, patch: Partial<KeyDraft>) =>
    setDraft((d) => ({ ...d, keys: (d.keys ?? []).map((k) => (k.id === id ? { ...k, ...patch } : k)) }));
  const setSettings = (patch: Partial<SettingsDraft>) =>
    setDraft((d) => ({ ...d, settings: { ...d.settings, ...patch } }));

  async function testNewKey(k: KeyDraft) {
    setTestingKey(k.id);
    const of = fingerprint(k);
    try {
      const result = await providersApi.testSpec(provider.name, keyTestSpec(provider, draft, k.secret));
      setNewTests((t) => ({ ...t, [k.id]: { of, result, error: null } }));
    } catch (e) {
      setNewTests((t) => ({ ...t, [k.id]: { of, result: null, error: e } }));
    } finally {
      setTestingKey(null);
    }
  }

  async function testSettings() {
    setTestingSettings(true);
    const of = settingsOf;
    try {
      const result = await providersApi.testSpec(provider.name, settingsSpec);
      setSettingsTest({ of, result, error: null });
    } catch (e) {
      // The saved key can't be used for this test (change control): ask for one.
      if (!needKey && e instanceof ApiError && e.status === 400 && signsInWithKey) setRedactedRefused(true);
      setSettingsTest({ of, result: null, error: e });
    } finally {
      setTestingSettings(false);
    }
  }

  async function save() {
    const found = draftErrors(provider.spec.type, draft);
    if (Object.keys(found).length > 0) {
      setShowErrors(true);
      return;
    }
    if (untested.length > 0 || !settingsPassed) return;
    const patch = toPatch(base, draft);
    if (Object.keys(patch).length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const saved = await update(provider.name, patch, base.metadata.etag);
      const mixed = mixedUpKeys(base, saved, draft);
      if (mixed.length > 0)
        notify(
          'error',
          'Check the keys you kept',
          `The gateway may have kept another key’s secret for ${mixed.join(', ')}. Replace ${mixed.length === 1 ? 'it' : 'them'} to be sure.`,
        );
      else notify('success', 'Provider saved', provider.name);
      setBase(saved);
      setDraft(fromProvider(saved));
      setNewTests({});
      setSettingsTest(null);
      setTestKeyText('');
      setRedactedRefused(false);
      setShowErrors(false);
    } catch (e) {
      // A change request opened or offered instead: the shell says so.
      if (!isChangeControl(e)) setError(e);
    } finally {
      setBusy(false);
    }
  }

  const blockReason = !edited
    ? 'Nothing changed yet.'
    : untested.length > 0
      ? `Test the new key${untested.length === 1 ? '' : 's'} before you save.`
      : !settingsPassed
        ? 'Test the new settings before you save.'
        : undefined;

  return (
    <Drawer
      open
      onClose={onClose}
      dirty={edited}
      width="lg"
      title={
        <span className="flex items-center gap-2">
          <ProviderMark type={provider.spec.type} size="md" />
          <span className="mono">{provider.name}</span>
        </span>
      }
      subtitle={`${providerTypeLabel(provider.spec.type)}${provider.spec.base_url ? ` · ${provider.spec.base_url}` : ''}`}
      footer={
        <>
          {onTest ? (
            <Button icon="play" className="mr-auto" onClick={() => onTest(provider.name)}>
              Test connection
            </Button>
          ) : null}
          <Button variant="ghost" onClick={onClose}>
            {canEdit && edited ? 'Cancel' : 'Close'}
          </Button>
          {canEdit ? (
            <Button
              variant="primary"
              busy={busy}
              disabled={blockReason !== undefined}
              disabledReason={blockReason ?? ''}
              onClick={() => void save()}
            >
              Save changes
            </Button>
          ) : null}
        </>
      }
    >
      <form
        noValidate
        className="flex flex-col gap-6"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <InlineError error={error} />
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-2 text-ui">
          <dt className="text-muted">Signs in with</dt>
          <dd className="text-fg">{method in AUTH_LABELS ? AUTH_LABELS[method as AuthMethod] : method}</dd>
          {age?.due ? (
            <>
              <dt className="text-muted">Key rotation</dt>
              <dd>
                <Badge tone="warn">Rotation due</Badge>
              </dd>
            </>
          ) : null}
          {status && !status.tls_verify ? (
            <>
              <dt className="text-muted">Certificate check</dt>
              <dd className="text-warn">Off: the provider&apos;s certificate isn&apos;t verified</dd>
            </>
          ) : null}
        </dl>
        <section className="flex flex-col gap-2">
          <div className="flex items-baseline justify-between gap-3">
            <h3 className="text-ui font-semibold text-fg">
              {keys.length <= 1 ? 'Key' : `Keys (${String(keys.length)})`}
            </h3>
            {canEdit && draft.keys ? (
              <Button
                size="sm"
                variant="ghost"
                icon="plus"
                disabled={keys.length >= 256}
                disabledReason="A provider takes at most 256 keys."
                onClick={() => setDraft((d) => ({ ...d, keys: [...(d.keys ?? []), newKey(d.keys ?? [])] }))}
              >
                Add key
              </Button>
            ) : null}
          </div>
          {draft.keys === null ? (
            <div className="flex flex-col gap-2">
              <p className="text-ui text-muted">
                This provider signs in without an API key, so there are no keys to replace or reset.
              </p>
              {status?.rate_limits ? <Readings readings={status.rate_limits} /> : null}
            </div>
          ) : (
            <>
              {multi && keys.length > 1 ? (
                <p className="text-caption text-muted">
                  Requests are spread over the keys by weight. A key with weight 0 is a standby.
                </p>
              ) : null}
              {errors.keys ? (
                <p role="alert" className="text-caption text-error">
                  {errors.keys}
                </p>
              ) : null}
              <ul className="flex flex-col gap-2">
                {keys.map((k) => {
                  if (k.saved === null)
                    return (
                      <NewKeyCard
                        key={k.id}
                        draft={k}
                        errors={errors}
                        share={keyShares.get(k.id) ?? 0}
                        test={newTests[k.id] ?? null}
                        of={fingerprint(k)}
                        testing={testingKey === k.id}
                        onChange={(patch) => setKey(k.id, patch)}
                        onTest={() => void testNewKey(k)}
                        onRemove={() =>
                          setDraft((d) => ({ ...d, keys: (d.keys ?? []).filter((x) => x.id !== k.id) }))
                        }
                      />
                    );
                  const row = rows.find((r) => r.name === k.saved);
                  if (!row) return null;
                  const savedCount = keys.filter((x) => x.saved !== null).length;
                  return (
                    <KeyCard
                      key={k.id}
                      provider={provider.name}
                      row={row}
                      draft={k}
                      editing={canEdit && keys.length > 1}
                      errors={errors}
                      share={keyShares.get(k.id) ?? 0}
                      maxDays={age?.maxDays ?? null}
                      onChange={(patch) => setKey(k.id, patch)}
                      onRemove={
                        canEdit
                          ? () =>
                              setDraft((d) => ({ ...d, keys: (d.keys ?? []).filter((x) => x.id !== k.id) }))
                          : null
                      }
                      removeReason={
                        keys.length <= 1 || (savedCount <= 1 && keys.length === savedCount)
                          ? 'A provider needs at least one key.'
                          : undefined
                      }
                      onReplace={
                        onReplace && method === 'api_key'
                          ? (key) => onReplace(provider.name, draft.single ? undefined : key)
                          : null
                      }
                    />
                  );
                })}
              </ul>
            </>
          )}
        </section>
        <section className="flex flex-col gap-3">
          <h3 className="text-ui font-semibold text-fg">Settings</h3>
          <SettingsFields
            type={provider.spec.type}
            defaultUrl={defaultUrl}
            draft={draft.settings}
            errors={errors}
            disabled={!canEdit}
            onChange={setSettings}
          />
          {canEdit && connChanged ? (
            <div className="flex flex-col gap-3 border-t border-line pt-3">
              {needKey ? (
                <>
                  <p className="text-caption text-muted">
                    {urlChanged
                      ? 'The base URL changed, so the gateway won’t send the saved key to the new address. Paste a key to test with.'
                      : 'This gateway tests changed settings only with a key you paste here, not the saved one.'}
                  </p>
                  <Field label="API key for the test" hint="Used for this test only. It isn’t saved.">
                    <Input
                      mono
                      type="password"
                      autoComplete="off"
                      value={testKeyText}
                      onChange={(e) => setTestKeyText(e.target.value)}
                    />
                  </Field>
                </>
              ) : null}
              <TestConnection
                test={settingsTest}
                of={settingsOf}
                busy={testingSettings}
                disabled={needKey && testKeyText.trim() === ''}
                label="Test these settings"
                onTest={() => void testSettings()}
              />
            </div>
          ) : null}
        </section>
        <DeploymentList provider={provider.name} type={provider.spec.type} />
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
    </Drawer>
  );
}
