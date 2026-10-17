'use client';

import Link from 'next/link';
import { useEffect, useMemo, useState } from 'react';
import { useQueryParam } from '@/components/layout/url-sync';

import { useScopedHref, useSettingsHref } from '@/components/layout/url-sync';
import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Input, Select, Switch } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { PageHeader, Panel } from '@/components/ui/layout';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { EmptyAction, EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { ExportCsvButton, Table, Td, Th, Tr } from '@/components/ui/table';
import { ApiError, describeError } from '@/lib/api/client';
import type { ProviderResource } from '@/lib/api/types';
import type { CsvColumn } from '@/lib/csv';
import { plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { ProviderMark } from '@/components/ui/marks';
import { providerTypeLabel } from '@/lib/providers';
import { notify } from '@/stores/ui';

import { providersApi, type ProviderStatus } from '../api';
import { AUTH_LABELS, type AuthMethod } from '../form';
import { useProviders } from '../store';
import { ageText, providerKeys, providerKeyStatus, rotation } from '../keys';
import { AddProviderDrawer } from './add-provider-drawer';
import { ProviderDrawer } from './provider-drawer';

const STATE_LABEL: Record<string, string> = {
  valid: 'Working',
  untested: 'Not tested yet',
  refreshing: 'Refreshing',
  failed: 'Failing',
  dead: 'Not working',
  none: 'No key needed',
};

function credentialTone(state: string | undefined): 'success' | 'error' | 'warn' | 'neutral' {
  if (state === 'untested') return 'neutral';
  if (state === 'valid') return 'success';
  if (state === 'failed' || state === 'dead') return 'error';
  if (state === 'refreshing') return 'warn';
  return 'neutral';
}

function RotateDialog({
  target,
  onClose,
}: {
  target: { name: string; key?: string | undefined } | null;
  onClose: () => void;
}) {
  const name = target?.name ?? null;
  const [secret, setSecret] = useState('');
  const [overlap, setOverlap] = useState('1h');
  return (
    <ConfirmDialog
      open={name !== null}
      onClose={() => {
        setSecret('');
        onClose();
      }}
      title={
        target?.key ? `Replace key ${target.key} of ${name ?? ''}?` : `Replace the API key of ${name ?? ''}?`
      }
      objectName={name ?? undefined}
      tone="primary"
      confirmLabel="Replace key"
      effect="The gateway starts using the new key at once and stops using the old one after the time you choose."
      onConfirm={async () => {
        if (!name) return;
        if (secret.trim() === '') throw new Error('Paste the new API key.');
        await providersApi.rotateCredential(name, secret.trim(), overlap, target?.key);
        void useProviders.getState().load({ quiet: true });
        setSecret('');
        notify('success', 'API key replaced', `${name} uses the new key now.`);
      }}
    >
      <Field label="New API key" hint="Stored encrypted and never shown again.">
        <Input
          mono
          type="password"
          autoComplete="off"
          value={secret}
          onChange={(e) => setSecret(e.target.value)}
          data-autofocus
        />
      </Field>
      <Field label="Stop using the old key">
        <Select
          value={overlap}
          onChange={(v) => setOverlap(v)}
          options={[
            { value: '0s', label: 'Right away' },
            { value: '15m', label: 'After 15 minutes' },
            { value: '1h', label: 'After 1 hour' },
            { value: '24h', label: 'After 24 hours' },
          ]}
        />
      </Field>
    </ConfirmDialog>
  );
}

export const NO_DEPLOYMENT_TO_TEST =
  'Add a deployment first: the test sends a request through one of its deployments.';

/** The providers table as CSV: the columns on screen, with the key's age in days. */
export function providerCsvColumns(
  status: ProviderStatus[] | null,
  usage: Map<string, number>,
  now: number,
  keyStatus: (p: ProviderResource) => string | undefined,
): CsvColumn<ProviderResource>[] {
  const st = (p: ProviderResource) => status?.find((x) => x.name === p.name);
  return [
    { header: 'provider', value: (p) => p.name },
    { header: 'type', value: (p) => p.spec.type },
    { header: 'base_url', value: (p) => p.spec.base_url ?? '' },
    { header: 'signs_in_with', value: (p) => st(p)?.credential.method ?? p.spec.auth?.method ?? 'api_key' },
    { header: 'key_status', value: (p) => keyStatus(p) ?? '' },
    { header: 'key_age_days', value: (p) => rotation(p, now)?.ageDays ?? '' },
    { header: 'deployments', value: (p) => usage.get(p.name) ?? 0 },
    { header: 'enabled', value: (p) => p.spec.enabled !== false },
  ];
}

/**
 * Providers: the AI providers the gateway calls, their credential state and age, how many
 * deployments use each, and "Add provider" as the page's one primary action. A row opens the
 * provider's keys with their state, age and the provider's reported limits.
 */
export function ProvidersScreen() {
  const providers = useProviders((s) => s.providers);
  const status = useProviders((s) => s.status);
  const deployments = useProviders((s) => s.deployments);
  const health = useProviders((s) => s.health);
  const load = useProviders((s) => s.load);
  /** "Working" needs evidence: a key that has answered; until then "Not tested yet". */
  const keyStatus = (p: ProviderResource) => {
    const st = status.data?.find((x) => x.name === p.name);
    return providerKeyStatus(st?.credential.state, providerKeys(p, st, deployments.data, health.data));
  };
  const setEnabled = useProviders((s) => s.setEnabled);
  const remove = useProviders((s) => s.remove);
  const canEdit = useCan('patchProvider');
  const canTest = useCan('testProvider');
  const canRotate = useCan('rotateProviderCredential');
  const canDelete = useCan('deleteProvider');
  const [adding, setAdding] = useState(false);
  const [rotating, setRotating] = useState<{ name: string; key?: string | undefined } | null>(null);
  // The open provider is in the URL (`provider`), so its drawer can be linked.
  const [opened, setOpened] = useQueryParam('provider');
  const now = useNow(60_000);
  const [deleting, setDeleting] = useState<ProviderResource | null>(null);
  const [turningOff, setTurningOff] = useState<ProviderResource | null>(null);
  const modelsHref = useScopedHref('/models/');
  const configHref = useSettingsHref('configuration');

  useEffect(() => {
    void load();
  }, [load]);

  const usage = useMemo(() => {
    const map = new Map<string, number>();
    for (const d of deployments.data ?? []) map.set(d.spec.provider, (map.get(d.spec.provider) ?? 0) + 1);
    return map;
  }, [deployments.data]);

  const test = (name: string) =>
    void providersApi.test(name).then(
      () => notify('success', `${name} answered`, 'The gateway reached the provider with its key.'),
      (e: unknown) =>
        notify(
          'error',
          `Couldn't reach ${name}`,
          e instanceof ApiError && e.details[0]?.message ? e.details[0].message : undefined,
        ),
    );

  const list = providers.data ?? [];
  return (
    <>
      <PageHeader
        title="Providers"
        description="The AI providers the gateway calls, and the keys it uses for each"
        actions={
          <Gate operation="createProvider" mode="hide">
            <Button variant="primary" icon="plus" onClick={() => setAdding(true)}>
              Add provider
            </Button>
          </Gate>
        }
      />
      <div className="flex flex-col">
        <Panel
          flush
          {...(list.length > 0
            ? {
                title: 'Providers',
                subtitle: plural(list.length, 'provider'),
                actions: (
                  <ExportCsvButton
                    filename="providers"
                    rows={list}
                    columns={providerCsvColumns(status.data ?? null, usage, now, keyStatus)}
                  />
                ),
              }
            : {})}
        >
          {providers.status === 'loading' && !providers.data ? <LoadingRows rows={3} /> : null}
          {providers.error ? (
            <ErrorState error={providers.error} subject="Providers" onRetry={() => void load()} />
          ) : null}
          {providers.data && list.length === 0 ? (
            <EmptyState
              title="No providers yet"
              description="Add the AI provider you use, with its API key."
              action={
                <Gate operation="createProvider" mode="hide">
                  <EmptyAction onClick={() => setAdding(true)}>Add provider</EmptyAction>
                </Gate>
              }
              secondaryAction={<EmptyAction href={configHref}>Set them in the configuration</EmptyAction>}
            />
          ) : null}
          {list.length > 0 ? (
            <div className="overflow-x-auto">
              <Table label="Providers">
                <thead>
                  <tr>
                    <Th>Provider</Th>
                    <Th className="max-sm:hidden">Signs in with</Th>
                    <Th>
                      <span className="max-sm:hidden">Key status</span>
                      <span className="sm:hidden">Key</span>
                    </Th>
                    <Th className="max-lg:hidden">Key age</Th>
                    <Th className="max-sm:hidden">Used by</Th>
                    <Th>On</Th>
                    <Th>
                      <span className="sr-only">Actions</span>
                    </Th>
                  </tr>
                </thead>
                <tbody>
                  {list.map((p) => {
                    const st = status.data?.find((x) => x.name === p.name);
                    const method = st?.credential.method ?? p.spec.auth?.method ?? 'api_key';
                    const hint = Object.values(p.metadata.secret_hints ?? {})[0];
                    const used = usage.get(p.name) ?? 0;
                    const age = rotation(p, now);
                    const items: MenuItem[] = [
                      { label: 'Keys and limits', icon: 'key' as const, onSelect: () => setOpened(p.name) },
                      ...(canTest
                        ? [
                            {
                              label: 'Test connection',
                              icon: 'play' as const,
                              // The test sends a request through a deployment of the provider.
                              disabled: deployments.data !== null && used === 0,
                              disabledReason: NO_DEPLOYMENT_TO_TEST,
                              onSelect: () => test(p.name),
                            },
                          ]
                        : []),
                      ...(canRotate && method === 'api_key'
                        ? [
                            {
                              label: 'Replace API key',
                              icon: 'rotate' as const,
                              onSelect: () => setRotating({ name: p.name }),
                            },
                          ]
                        : []),
                      ...(canDelete
                        ? [
                            {
                              label: 'Delete provider',
                              icon: 'trash' as const,
                              danger: true,
                              disabled: used > 0,
                              disabledReason: used > 0 ? 'Delete its deployments first' : undefined,
                              onSelect: () => setDeleting(p),
                            },
                          ]
                        : []),
                    ];
                    return (
                      <Tr
                        key={p.name}
                        className="group"
                        interactive
                        tabIndex={0}
                        aria-label={`${p.name}: keys and limits`}
                        onClick={() => setOpened(p.name)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter' && e.target === e.currentTarget) setOpened(p.name);
                        }}
                      >
                        <Td className="py-2.5">
                          <div className="flex items-center gap-2.5">
                            <ProviderMark type={p.spec.type} size="md" className="shrink-0 text-muted" />
                            <div className="min-w-0">
                              <div className="mono text-body text-fg">{p.name}</div>
                              <div className="text-caption text-muted">
                                {providerTypeLabel(p.spec.type)}
                                {p.spec.base_url ? (
                                  <span className="mono max-sm:hidden"> · {p.spec.base_url}</span>
                                ) : null}
                              </div>
                            </div>
                          </div>
                        </Td>
                        <Td className="max-sm:hidden">
                          {method in AUTH_LABELS ? AUTH_LABELS[method as AuthMethod] : method}
                          {hint ? <span className="mono text-caption text-muted"> {hint}</span> : null}
                        </Td>
                        <Td>
                          {st ? (
                            <span className="flex items-center gap-1.5">
                              <Dot tone={credentialTone(keyStatus(p))} />
                              <span className="max-sm:sr-only">
                                {STATE_LABEL[keyStatus(p) ?? ''] ?? keyStatus(p)}
                              </span>
                            </span>
                          ) : (
                            <span className="text-muted">—</span>
                          )}
                        </Td>
                        <Td className="max-lg:hidden">
                          {age ? (
                            <span className="flex items-center gap-2">
                              <span className={age.due ? 'text-warn' : 'text-fg'}>
                                {age.ageDays < 1 ? 'Replaced today' : `${ageText(age.ageDays)} old`}
                              </span>
                              {age.maxDays ? (
                                <span className="text-caption text-muted">
                                  max {Math.round(age.maxDays)} days
                                </span>
                              ) : null}
                              {age.due ? <Badge tone="warn">Replace now</Badge> : null}
                            </span>
                          ) : (
                            <span className="text-muted">—</span>
                          )}
                        </Td>
                        <Td muted className="max-sm:hidden">
                          {deployments.data ? (used > 0 ? plural(used, 'deployment') : 'Not used yet') : '—'}
                        </Td>
                        <Td onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
                          <Switch
                            label={`Turn ${p.name} ${p.spec.enabled !== false ? 'off' : 'on'}`}
                            hideLabel
                            checked={p.spec.enabled !== false}
                            disabled={!canEdit}
                            disabledReason="You don't have permission to change providers."
                            onChange={(on) => {
                              // Turning a provider off stops traffic: it is confirmed, then undoable.
                              if (!on) {
                                setTurningOff(p);
                                return;
                              }
                              void setEnabled(p.name, true).then(
                                () => notify('success', `${p.name} turned on`),
                                (e: unknown) =>
                                  notify('error', `Couldn't change ${p.name}`, describeError(e)),
                              );
                            }}
                          />
                        </Td>
                        <Td
                          className="text-right"
                          onClick={(e) => e.stopPropagation()}
                          onKeyDown={(e) => e.stopPropagation()}
                        >
                          {items.length > 0 ? (
                            <MenuButton
                              items={items}
                              label={`Actions for ${p.name}`}
                              iconOnly
                              variant="ghost"
                              size="sm"
                            />
                          ) : null}
                        </Td>
                      </Tr>
                    );
                  })}
                </tbody>
              </Table>
            </div>
          ) : null}
        </Panel>
        {list.length > 0 && (deployments.data?.length ?? 0) === 0 && deployments.data ? (
          <p className="px-gutter py-3 text-ui text-muted max-sm:px-gutter-sm">
            Next:{' '}
            <Link href={modelsHref} className="text-accent hover:underline">
              add a model
            </Link>{' '}
            from one of these providers so your apps can call it.
          </p>
        ) : null}
      </div>
      <AddProviderDrawer open={adding} onClose={() => setAdding(false)} />
      <RotateDialog target={rotating} onClose={() => setRotating(null)} />
      <ProviderDrawer
        provider={list.find((p) => p.name === opened) ?? null}
        status={status.data?.find((x) => x.name === opened)}
        onClose={() => setOpened(null)}
        onReplace={canRotate ? (name, key) => setRotating({ name, key }) : null}
        onTest={canTest ? test : null}
      />
      <ConfirmDialog
        open={turningOff !== null}
        onClose={() => setTurningOff(null)}
        title={`Turn ${turningOff?.name ?? ''} off?`}
        tone="primary"
        confirmLabel="Turn off"
        effect={(() => {
          const n = turningOff ? (usage.get(turningOff.name) ?? 0) : 0;
          return n > 0
            ? `Its ${plural(n, 'deployment')} stop taking requests: Models fall back to other deployments where they have one, and apps using the rest get errors until you turn it on again. Nothing is deleted.`
            : 'No deployment uses it yet, so no request is affected. Nothing is deleted.';
        })()}
        onConfirm={async () => {
          if (!turningOff) return;
          const name = turningOff.name;
          await setEnabled(name, false);
          notify('success', `${name} turned off`, undefined, {
            label: 'Undo',
            onClick: () =>
              void setEnabled(name, true).then(
                () => notify('success', `${name} turned on`),
                (e: unknown) => notify('error', `Couldn't change ${name}`, describeError(e)),
              ),
          });
        }}
      />
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title={`Delete provider ${deleting?.name ?? ''}?`}
        objectName={deleting?.name}
        typeToConfirm={deleting?.name}
        confirmLabel="Delete provider"
        effect="The gateway forgets this provider and its key. Models can't use it until you add it again."
        onConfirm={async (reason) => {
          if (!deleting) return;
          await remove(deleting.name, reason);
          notify('success', 'Provider deleted', deleting.name);
        }}
      />
    </>
  );
}
