'use client';

import { useEffect, useState } from 'react';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Meter } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { useCan } from '@/components/ui/gate';
import { Drawer } from '@/components/ui/overlay';
import { ProviderMark } from '@/components/ui/marks';
import { formatCount } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { providerTypeLabel } from '@/lib/providers';
import type { ProviderResource } from '@/lib/api/types';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';

import type { ProviderStatus } from '../api';
import { AUTH_LABELS, type AuthMethod } from '../form';
import {
  ageText,
  type KeyState,
  providerKeys,
  type ProviderKeyRow,
  type RateLimits,
  rotation,
} from '../keys';
import { useProviders } from '../store';
import { api, describeError } from '@/lib/api/client';
import { collectLeaves } from '@/features/models/types';
import { hasGatewayPermission, useSession } from '@/stores/session';

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
function DeploymentList({ provider }: { provider: string }) {
  const deployments = useProviders((s) => s.deployments.data);
  const load = useProviders((s) => s.load);
  const canDelete = useCan('deleteDeployment');
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
    </section>
  );
}

const HEALTH: Record<KeyState['state'], { tone: 'success' | 'warn' | 'error' | 'neutral'; label: string }> = {
  working: { tone: 'success', label: 'Working' },
  resting: { tone: 'warn', label: 'Resting after errors' },
  not_working: { tone: 'error', label: 'Not working' },
  untested: { tone: 'neutral', label: 'Not tested yet' },
  unknown: { tone: 'neutral', label: 'Not used yet' },
};

const DEAD_REASON: Record<string, string> = {
  credential: 'the provider refused it',
  quota: 'its quota ran out',
  permission: "it isn't allowed to use this model",
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

function KeyCard({
  provider,
  row,
  maxDays,
  onReplace,
}: {
  provider: string;
  row: ProviderKeyRow;
  maxDays: number | null;
  onReplace: ((key: string) => void) | null;
}) {
  const resetKey = useProviders((s) => s.resetKey);
  const canReset = useCan('resetProviderKey');
  const now = useNow(60_000);
  const h = HEALTH[row.health.state];
  const age = row.rotatedAt ? (now - Date.parse(row.rotatedAt)) / 86_400_000 : null;
  const share = age !== null && maxDays ? age / maxDays : null;
  return (
    <li className="flex flex-col gap-3 rounded-control border border-line px-3.5 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span className="mono text-body text-fg">{row.name}</span>
          {row.hint ? <span className="mono text-caption text-muted">{row.hint}</span> : null}
          {row.weight !== null ? <span className="text-caption text-dim">weight {row.weight}</span> : null}
        </div>
        <span className="flex items-center gap-1.5 text-ui">
          <Dot tone={h.tone} />
          {h.label}
        </span>
      </div>
      {row.health.state === 'not_working' || row.health.state === 'resting' ? (
        <p className="text-caption text-muted">
          {row.health.reason
            ? `Set aside because ${DEAD_REASON[row.health.reason] ?? row.health.reason}. `
            : ''}
          {row.health.until
            ? `${row.health.state === 'resting' ? 'Used again' : 'Tried again'} ${formatRelative(row.health.until)}.`
            : ''}
        </p>
      ) : null}
      <div className="flex flex-col gap-1">
        <div className="flex items-baseline justify-between gap-3 text-caption">
          <span className="text-muted">Last replaced</span>
          <span className="text-fg">
            {age === null ? 'Not recorded' : age < 1 ? 'Today' : `${ageText(age)} ago`}
            {maxDays ? <span className="text-muted"> · replace every {Math.round(maxDays)} days</span> : null}
          </span>
        </div>
        {share !== null ? (
          <Meter
            value={Math.min(share, 1)}
            max={1}
            tone={share >= 1 ? 'error' : share >= 0.85 ? 'warn' : 'accent'}
            label="Age of the key against the rotation policy"
          />
        ) : null}
      </div>
      <Readings readings={row.readings} />
      {(canReset && row.health.state !== 'unknown') || onReplace ? (
        <div className="flex flex-wrap gap-2">
          {onReplace ? (
            <Button size="sm" icon="rotate" onClick={() => onReplace(row.name)}>
              Replace key
            </Button>
          ) : null}
          {canReset && (row.health.state === 'not_working' || row.health.state === 'resting') ? (
            <Button
              size="sm"
              icon="refresh"
              onClick={() =>
                void resetKey(provider, row.name).then(
                  () => notify('success', 'Key back in use', `${provider} · ${row.name}`),
                  (e: unknown) => notify('error', `Couldn't reset ${row.name}`, describeError(e)),
                )
              }
            >
              Use it again now
            </Button>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

/** A provider's details: type, sign-in, tier, rotation policy and each key's state, age and limits. */
export function ProviderDrawer({
  provider,
  status,
  onClose,
  onReplace,
  onTest,
}: {
  provider: ProviderResource | null;
  status: ProviderStatus | undefined;
  onClose: () => void;
  onReplace: ((name: string, key?: string) => void) | null;
  onTest: ((name: string) => void) | null;
}) {
  const deployments = useProviders((s) => s.deployments);
  const health = useProviders((s) => s.health);
  const now = useNow(60_000);
  if (!provider) return null;
  const method = status?.credential.method ?? provider.spec.auth?.method ?? 'api_key';
  const keys = providerKeys(provider, status, deployments.data, health.data);
  const age = rotation(provider, now);
  const tier = status?.account_tier ?? provider.spec.account_tier ?? null;
  const single = keys.length <= 1;
  const untestable =
    deployments.data !== null && !deployments.data.some((d) => d.spec.provider === provider.name);
  return (
    <Drawer
      open
      onClose={onClose}
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
            <Button
              icon="play"
              disabled={untestable}
              disabledReason="Add a deployment first: the test sends a request through one of its deployments."
              onClick={() => onTest(provider.name)}
            >
              Test connection
            </Button>
          ) : null}
          <Button variant="ghost" onClick={onClose}>
            Close
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-5">
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-2 text-ui">
          <dt className="text-muted">Signs in with</dt>
          <dd className="text-fg">{method in AUTH_LABELS ? AUTH_LABELS[method as AuthMethod] : method}</dd>
          <dt className="text-muted">Account tier</dt>
          <dd className="text-fg">{tier ?? <span className="text-muted">Not set</span>}</dd>
          <dt className="text-muted">Key rotation</dt>
          <dd className="text-fg">
            {age?.maxDays
              ? `Replace keys every ${String(Math.round(age.maxDays))} days`
              : 'No rotation policy'}
            {age?.due ? (
              <Badge tone="warn" className="ml-2">
                Rotation due
              </Badge>
            ) : null}
          </dd>
          {status && !status.tls_verify ? (
            <>
              <dt className="text-muted">Certificate check</dt>
              <dd className="text-warn">Off: the provider&apos;s certificate isn&apos;t verified</dd>
            </>
          ) : null}
        </dl>
        <section className="flex flex-col gap-2">
          <h3 className="text-ui font-semibold text-fg">
            {single ? 'Key' : `Keys (${String(keys.length)})`}
          </h3>
          {keys.length === 0 ? (
            <div className="flex flex-col gap-2">
              <p className="text-ui text-muted">
                This provider signs in without an API key, so there are no keys to replace or reset.
              </p>
              {status?.rate_limits ? <Readings readings={status.rate_limits} /> : null}
            </div>
          ) : (
            <ul className="flex flex-col gap-2">
              {keys.map((k) => (
                <KeyCard
                  key={k.name}
                  provider={provider.name}
                  row={k}
                  maxDays={age?.maxDays ?? null}
                  onReplace={
                    onReplace && method === 'api_key'
                      ? (key) => onReplace(provider.name, single ? undefined : key)
                      : null
                  }
                />
              ))}
            </ul>
          )}
        </section>
        <DeploymentList provider={provider.name} />
      </div>
    </Drawer>
  );
}
