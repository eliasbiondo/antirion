'use client';

import { useEffect, useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';

import { FavouriteButton } from '@/components/ui/favourite';
import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Meter } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Field, Select } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { DefinitionGrid } from '@/components/ui/layout';
import { DetailHeader, DetailSection } from '@/components/ui/list-detail';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { CopyButton } from '@/components/ui/misc';
import { Banner, EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { auditActionLabel, auditActorBy, auditActorLabel, auditToggle } from '@/lib/audit-words';
import { useTokenNames } from '@/stores/actors';
import type { BudgetCounter } from '@/lib/api/types';
import { formatCount, formatMoney, formatPercent } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { formatDate, formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { budgetsFor } from '../hooks';
import { rotateKey, setKeyEnabled, useKeys, useKeysContext } from '../store';
import { KeyHours, KeyLimits, TopIps } from './key-facts';
import { endpointLabel, type KeyView, permissionLabel, STATUS_LABEL, statusTone, TYPE_LABEL } from '../types';
import { Tooltip } from '@/components/ui/tooltip';

function hintPrefix(hint: string | undefined): string | null {
  if (!hint) return null;
  const i = hint.indexOf('…');
  return i > 0 ? hint.slice(0, i) : hint;
}

const GRACE_LABEL: Record<string, string> = {
  '0s': 'no time at all',
  '1h': '1 hour',
  '24h': '24 hours',
  '72h': '3 days',
  '7d': '7 days',
};

function RotateDialog({
  name,
  open,
  onClose,
  onSecret,
}: {
  name: string;
  open: boolean;
  onClose: () => void;
  onSecret: (secret: string) => void;
}) {
  const [grace, setGrace] = useState('24h');
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title={`Rotate ${name}?`}
      tone="primary"
      confirmLabel="Rotate key"
      effect={
        grace === '0s'
          ? 'A new secret is created and the old one stops working at once. Apps still using it will be refused.'
          : `A new secret is created. The old one keeps working for ${GRACE_LABEL[grace] ?? grace} so you can update your apps.`
      }
      withReason
      onConfirm={async (reason) => {
        const secret = await rotateKey(name, grace, reason);
        onSecret(secret);
      }}
    >
      <Field label="Old secret stays valid for" hint="At most 7 days.">
        <Select
          value={grace}
          onChange={(v) => setGrace(v)}
          options={[
            { value: '0s', label: 'Stop it now' },
            { value: '1h', label: '1 hour' },
            { value: '24h', label: '24 hours' },
            { value: '72h', label: '3 days' },
            { value: '7d', label: '7 days' },
          ]}
        />
      </Field>
    </ConfirmDialog>
  );
}

export interface KeyDetailProps {
  view: KeyView;
  team: string | undefined;
  organization: string | undefined;
  onEdit: () => void;
  onSecret: (title: string, secret: string) => void;
}

/** One key: identity, the reveal-free hint, actions, limits, budgets and its audit history. */
export function KeyDetail({ view, team, organization, onEdit, onSecret }: KeyDetailProps) {
  const tokens = useTokenNames();
  const [, setKeyParam] = useQueryParam('key');
  const timeZone = usePreferences((s) => s.timeZone);
  const budgets = useKeysContext((s) => s.budgets);
  const audit = useKeysContext((s) => s.audit[view.name]);
  const loadAudit = useKeysContext((s) => s.loadAudit);
  const remove = useKeys((s) => s.remove);
  const canPatch = useCan('patchKey');
  const canDelete = useCan('deleteKey');
  const [dialog, setDialog] = useState<'rotate' | 'revoke' | 'disable' | null>(null);
  const { key } = view;
  const prefix = hintPrefix(key.metadata.hint);
  const playground = key.metadata.created_by === 'playground';
  const disabled = key.spec.enabled === false;
  const rotationDue = Boolean(key.metadata.rotation_due);
  const limits = (key.spec.policies?.limits ?? {}) as Record<
    string,
    { unit?: string; limit?: number; per?: string } | undefined
  >;
  const limitRows = Object.entries(limits).filter(([n]) => n !== 'locked' && n !== 'request_overridable');

  const loadKeyFacts = useKeysContext((s) => s.loadKeyFacts);
  const ipSets = useKeysContext((s) => s.auth.data?.spec.ip_sets);
  const now = useNow(60_000);
  useEffect(() => {
    void loadAudit(view.name);
    void loadKeyFacts(view.name);
  }, [view.name, loadAudit, loadKeyFacts]);

  const org = key.metadata.organization ?? organization;
  const applicable = budgets.data ? budgetsFor(view, budgets.data, team, org) : [];
  const enable = () =>
    void setKeyEnabled(view.name, true).then(() => notify('success', 'Key enabled', view.name));

  const editButton = (primary: boolean) => (
    <Gate operation="replaceKey">
      <Button
        variant={primary ? 'primary' : 'secondary'}
        icon="edit"
        onClick={onEdit}
        disabled={playground}
        disabledReason={playground ? "Keys made in the playground can't be edited" : undefined}
      >
        Edit
      </Button>
    </Gate>
  );
  const rotateButton = (primary: boolean) => (
    <Gate operation="rotateKey" mode="hide">
      <Button
        variant={primary ? 'primary' : 'secondary'}
        icon="rotate"
        onClick={() => setDialog('rotate')}
        disabled={playground}
        disabledReason={playground ? "Keys made in the playground can't be rotated" : undefined}
      >
        Rotate
      </Button>
    </Gate>
  );

  const more: MenuItem[] = [
    {
      label: 'Copy link',
      icon: 'link',
      onSelect: () => void navigator.clipboard.writeText(window.location.href),
    },
  ];
  if (canPatch)
    more.push(
      disabled
        ? { label: 'Enable', icon: 'play', onSelect: enable }
        : { label: 'Disable', icon: 'pause', onSelect: () => setDialog('disable') },
    );
  if (canDelete) more.push({ label: 'Revoke', icon: 'x', danger: true, onSelect: () => setDialog('revoke') });

  return (
    <div>
      <DetailHeader
        eyebrow={
          <>
            <Badge tone={statusTone(view.status)} dot>
              {STATUS_LABEL[view.status]}
            </Badge>
            <Badge tone="accent">{TYPE_LABEL[view.type]}</Badge>
            <span className="font-sans">
              {key.spec.expires_at
                ? `${view.status === 'expired' ? 'Expired' : 'Expires'} ${formatDate(key.spec.expires_at, timeZone)}`
                : 'Never expires'}
            </span>
          </>
        }
        title={
          <span className="flex min-w-0 items-center gap-1.5">
            <span className="truncate">{view.name}</span>
            <FavouriteButton kind="key" name={view.name} label={`key ${view.name}`} />
          </span>
        }
        subtitle={
          key.spec.description ? <span className="font-sans">{key.spec.description}</span> : undefined
        }
        aside={
          key.metadata.hint ? (
            <div className="flex items-center gap-1 rounded-control border border-line bg-sunken py-1 pr-1 pl-3 max-sm:hidden">
              <Tooltip content="Key prefix. The full key was shown once.">
                <span className="mono text-ui text-fg">{key.metadata.hint}</span>
              </Tooltip>
              {prefix ? <CopyButton value={prefix} label="Copy key prefix" /> : null}
            </div>
          ) : null
        }
        actions={
          <>
            {rotationDue ? rotateButton(true) : editButton(true)}
            {rotationDue ? editButton(false) : rotateButton(false)}
            <span className="ml-auto">
              <MenuButton items={more} label="More" />
            </span>
          </>
        }
      />
      {playground ? (
        <Banner tone="info" title="Playground key">
          Made in the playground{view.owner ? ` for ${view.owner}` : ''}
          {key.spec.expires_at ? `; it expires ${formatRelative(key.spec.expires_at)}` : ''}. It can&apos;t be
          edited or rotated; the playground makes a new one when it needs to.
        </Banner>
      ) : null}
      {view.status === 'rotating' && key.metadata.rotation ? (
        <Banner tone="info" title="Rotating">
          The old secret works until {formatDateTime(key.metadata.rotation.valid_until, timeZone)}. Update
          your apps before then.
        </Banner>
      ) : null}
      {rotationDue && view.status !== 'rotating' ? (
        <Banner tone="warn" title="This key needs rotation">
          It is older than your rotation policy allows. Rotate it and update your apps with the new secret.
        </Banner>
      ) : null}
      {disabled ? (
        <Banner
          tone="warn"
          title="This key is disabled"
          actions={
            canPatch ? (
              <Button size="sm" onClick={enable}>
                Enable
              </Button>
            ) : undefined
          }
        >
          Apps using it are refused until it is enabled again
          {key.metadata.disabled_reason ? ` (${key.metadata.disabled_reason})` : ''}.
        </Banner>
      ) : null}

      <DetailSection title="Details">
        <DefinitionGrid
          columns={3}
          items={[
            { label: 'Project', value: view.project },
            { label: 'Team', value: team ?? '—' },
            { label: 'Owner', value: view.owner ?? 'None (service key)' },
            {
              label: 'Created',
              value: formatDate(key.metadata.created_at, timeZone),
            },
            {
              label: 'Created by',
              value: playground
                ? 'The playground'
                : key.metadata.created_by
                  ? auditActorLabel({ id: key.metadata.created_by }, tokens)
                  : '—',
            },
            {
              label: 'Expires',
              value: key.spec.expires_at ? formatDateTime(key.spec.expires_at, timeZone) : 'Never',
            },
            {
              label: 'Last used',
              value: key.metadata.last_used_at ? formatRelative(key.metadata.last_used_at) : 'Never',
            },
            { label: 'Organization', value: key.metadata.organization ?? org ?? '—' },
            {
              label: 'Rotation due',
              value: key.metadata.rotation_due_at
                ? formatDate(key.metadata.rotation_due_at, timeZone)
                : 'No policy',
            },
            {
              label: 'Last rotated',
              value: key.metadata.last_rotated_at ? formatRelative(key.metadata.last_rotated_at) : 'Never',
            },
          ]}
        />
      </DetailSection>

      <DetailSection title="Access and limits">
        <dl className="grid grid-cols-2 gap-x-6 gap-y-4 text-ui max-sm:grid-cols-1">
          <div>
            <dt className="eyebrow mb-1.5">Endpoints</dt>
            <dd className="text-fg">
              {key.spec.endpoints?.length
                ? key.spec.endpoints.map(endpointLabel).join(', ')
                : 'All endpoints'}
            </dd>
          </div>
          <div>
            <dt className="eyebrow mb-1.5">Models</dt>
            <dd className={key.spec.models?.length ? 'mono text-fg' : 'text-fg'}>
              {key.spec.models?.length ? key.spec.models.join(', ') : 'All models'}
            </dd>
          </div>
          <div>
            <dt className="eyebrow mb-1.5">Rate limits</dt>
            <dd>
              <KeyLimits name={view.name} declared={limitRows} />
            </dd>
          </div>
          <div>
            <dt className="eyebrow mb-1.5">Allowed IP addresses</dt>
            <dd>
              {key.spec.allowed_ips?.length ? (
                <ul className="flex flex-col gap-1 text-fg">
                  {key.spec.allowed_ips.map((ip) =>
                    ip.startsWith('set:') ? (
                      <li key={ip}>
                        <span className="mono">{ip.slice(4)}</span>{' '}
                        <span className="text-caption text-muted">
                          IP set{ipSets?.[ip.slice(4)] ? `: ${ipSets[ip.slice(4)]?.join(', ')}` : ''}
                        </span>
                      </li>
                    ) : (
                      <li key={ip} className="mono">
                        {ip}
                      </li>
                    ),
                  )}
                </ul>
              ) : (
                <p className="text-muted">Any address</p>
              )}
            </dd>
          </div>
          <div>
            <dt className="eyebrow mb-1.5">Regions</dt>
            <dd className="text-fg">
              {key.spec.regions?.length ? (
                <span className="mono">{key.spec.regions.join(', ')}</span>
              ) : (
                'Any region'
              )}
              {key.spec.deny_regions?.length ? (
                <span className="block text-caption text-muted">
                  Never <span className="mono">{key.spec.deny_regions.join(', ')}</span>
                </span>
              ) : null}
            </dd>
          </div>
          <div>
            <dt className="eyebrow mb-1.5">Priority class</dt>
            <dd className="text-fg">
              {key.spec.priority_class ? <span className="mono">{key.spec.priority_class}</span> : 'None'}
            </dd>
          </div>
          <div>
            <dt className="eyebrow mb-1.5">Client certificate</dt>
            <dd className="text-fg">
              {key.spec.require_client_cert?.sha256_fingerprint ? (
                <>
                  <span className="block text-caption text-muted">SHA-256 fingerprint</span>
                  <span className="mono break-all">{key.spec.require_client_cert.sha256_fingerprint}</span>
                </>
              ) : key.spec.require_client_cert?.san ? (
                <>
                  <span className="block text-caption text-muted">Subject alternative name</span>
                  <span className="mono break-all">{key.spec.require_client_cert.san}</span>
                </>
              ) : (
                'Not required'
              )}
            </dd>
          </div>
          <div className="col-span-2 max-sm:col-span-1">
            <dt className="eyebrow mb-1.5">Key permissions</dt>
            <dd>
              {key.spec.permissions?.length ? (
                <div className="flex flex-wrap gap-1.5">
                  {key.spec.permissions.map((p) => (
                    <Badge key={p} title={p}>
                      {permissionLabel(p)}
                    </Badge>
                  ))}
                </div>
              ) : (
                <p className="text-muted">None</p>
              )}
            </dd>
          </div>
        </dl>
      </DetailSection>

      <DetailSection title="Last 24 hours">
        <KeyHours name={view.name} now={now} />
      </DetailSection>

      <DetailSection title="Where it was used from">
        <TopIps name={view.name} />
      </DetailSection>

      <DetailSection title="Budgets">
        {budgets.status === 'loading' ? <LoadingRows rows={2} /> : null}
        {budgets.error ? <ErrorState error={budgets.error} subject="Budgets" size="sm" /> : null}
        {budgets.data && applicable.length === 0 ? (
          <EmptyState size="sm" title="No budget applies to this key" />
        ) : null}
        <ul className="flex flex-col gap-3">
          {applicable.map((b) => {
            const spent = Number(b.spent);
            const amount = Number(b.amount);
            const ratio = amount > 0 ? spent / amount : 0;
            const tone = ratio >= 1 ? 'error' : ratio >= 0.8 ? 'warn' : 'accent';
            return (
              <li
                key={`${b.level}/${b.owner}/${b.name}/${b.partition ?? ''}`}
                className="flex flex-col gap-1.5"
              >
                <div className="flex flex-wrap items-center gap-2 text-ui">
                  <Dot tone={tone === 'accent' ? 'success' : tone} />
                  <span className="text-muted">{LEVEL_LABEL[b.level] ?? b.level}</span>
                  <span className="font-semibold text-fg">{b.owner}</span>
                  <span className="mono text-caption text-muted">{b.name}</span>
                  <Badge tone={b.mode === 'soft' && !b.downgrade ? 'neutral' : 'error'}>
                    {enforcementText(b)}
                  </Badge>
                </div>
                <Meter
                  value={spent}
                  max={amount}
                  tone={tone}
                  label={`${b.owner} ${b.name}: ${formatPercent(ratio, 0)}`}
                />
                <div className="text-caption text-muted">
                  <span className="mono">
                    {b.unit === 'usd'
                      ? `${formatMoney(spent)} of ${formatMoney(amount)}`
                      : `${formatCount(spent)} of ${formatCount(amount)} tokens`}
                  </span>{' '}
                  · {formatPercent(ratio, 0)}
                  {b.resets_at ? ` · resets ${formatRelative(b.resets_at)}` : ''}
                </div>
              </li>
            );
          })}
        </ul>
      </DetailSection>

      <DetailSection title="Activity">
        {!audit || audit.status === 'loading' ? <LoadingRows rows={3} /> : null}
        {audit?.error ? <ErrorState error={audit.error} subject="The audit log" size="sm" /> : null}
        {audit?.data?.length === 0 ? <EmptyState size="sm" title="No changes recorded yet" /> : null}
        <ol className="flex flex-col">
          {(audit?.data ?? []).map((entry) => (
            <li
              key={entry.id}
              className="flex items-center gap-3 border-b border-line py-2 text-ui last:border-0"
            >
              <Dot tone={entry.outcome === 'success' ? 'success' : 'error'} />
              <span className="text-fg">
                {auditToggle(entry.changes) !== null && /\.(update|patch|replace)$/.test(entry.action)
                  ? auditToggle(entry.changes)
                    ? 'Enabled'
                    : 'Disabled'
                  : auditActionLabel(entry.action)}
              </span>
              <span className="text-muted">by {auditActorBy(entry.actor, tokens)}</span>
              <Tooltip content={formatDateTime(entry.time, timeZone)}>
                <span className="ml-auto text-caption text-dim">{formatRelative(entry.time)}</span>
              </Tooltip>
            </li>
          ))}
        </ol>
      </DetailSection>

      <RotateDialog
        name={view.name}
        open={dialog === 'rotate'}
        onClose={() => setDialog(null)}
        onSecret={(secret) => onSecret(`New secret for ${view.name}`, secret)}
      />
      <ConfirmDialog
        open={dialog === 'disable'}
        onClose={() => setDialog(null)}
        title={`Disable ${view.name}?`}
        tone="primary"
        confirmLabel="Disable key"
        effect="Apps using this key are refused until you enable it again. Nothing is deleted."
        withReason
        onConfirm={async (reason) => {
          await setKeyEnabled(view.name, false, reason);
          notify('success', 'Key disabled', view.name, {
            label: 'Undo',
            onClick: () => void setKeyEnabled(view.name, true),
          });
        }}
      />
      <ConfirmDialog
        open={dialog === 'revoke'}
        onClose={() => setDialog(null)}
        title={`Revoke ${view.name}?`}
        confirmLabel="Revoke key"
        objectName={view.name}
        typeToConfirm={view.name}
        withReason
        effect={
          <>
            Apps using this key will stop working immediately. This can&apos;t be undone. Its history stays in
            the audit log.{' '}
            {canPatch && !disabled ? (
              <span className="text-muted">To pause the key instead, disable it.</span>
            ) : null}
          </>
        }
        onConfirm={async (reason) => {
          await remove(view.name, reason);
          // The key is gone: its link would only say so.
          setKeyParam(null);
          notify('success', 'Key revoked', view.name);
        }}
      />
    </div>
  );
}

function enforcementText(b: BudgetCounter): string {
  if (b.downgrade) return `Switches to ${b.downgrade.to_model ?? b.downgrade.to_tier ?? 'a cheaper Model'}`;
  if (b.mode === 'hard') return 'Blocks at the limit';
  if (b.mode === 'approval') return 'Blocks, then asks to approve';
  return 'Alerts only';
}

const LEVEL_LABEL: Record<string, string> = {
  gateway: 'Gateway',
  organization: 'Organization',
  team: 'Team',
  project: 'Project',
  key: 'Key',
};
