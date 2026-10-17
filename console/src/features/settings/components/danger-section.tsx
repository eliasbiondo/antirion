'use client';

import Link from 'next/link';
import { type ReactNode, useEffect, useState } from 'react';

import { changeRequestHref } from '@/components/layout/change-control';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Meter } from '@/components/ui/charts';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Checkbox, Field, Input, Select, Switch } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { API_BASE } from '@/lib/api/client';
import { usePolling } from '@/lib/hooks';
import { daysUntil, formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { scopeOrganization, useScope } from '@/stores/scope';
import { useShell } from '@/stores/shell';
import { notify } from '@/stores/ui';

import { settingsApi } from '../api';
import { type DualAction, heldActionLabel, useDanger } from '../danger-store';
import { MaintenanceCard } from './maintenance-card';
import { Card, SaveBar } from './parts';

export const DUAL_ACTIONS: { value: DualAction; label: string; description: string }[] = [
  { value: 'ingress_pause', label: 'Pause traffic', description: 'Gateway-wide or one organization.' },
  { value: 'cache_flush_all', label: 'Clear the whole cache', description: 'Not a single model or team.' },
  {
    value: 'organization_delete',
    label: "Schedule an organization's deletion",
    description: 'Cancelling needs no approval.',
  },
  {
    value: 'secrets_rotate',
    label: 'Rotate gateway secrets',
    description: 'Sessions, SCIM token, webhooks.',
  },
  { value: 'budget_reset', label: 'Reset a budget', description: "Starts its period's spend from zero." },
  {
    value: 'audit_sink_delete',
    label: 'Remove an audit log destination',
    description: 'Copies of the audit log stop.',
  },
];

/** Whether the two-person rule holds `action`: a note for the card that runs it. */
function useHeld(action: DualAction): boolean {
  return useDanger((s) => s.governance.data?.spec.dual_control?.actions?.includes(action) === true);
}

function SecondPersonNote({ action }: { action: DualAction }) {
  const held = useHeld(action);
  if (!held) return null;
  return (
    <p className="mt-3 flex items-center gap-1.5 text-caption text-info">
      <Icon name="users" size={14} />
      Needs a second person&apos;s approval before it runs.
    </p>
  );
}

/** Actions the two-person rule holds, waiting for someone else to approve them. */
function WaitingCard() {
  const pending = useDanger((s) => s.pending);
  const list = pending.data ?? [];
  if (list.length === 0) return null;
  return (
    <Card title="Waiting for approval" description="These run once a second person approves them.">
      <ul className="flex flex-col divide-y divide-line rounded-control border border-line">
        {list.map((r) => (
          <li key={r.id} className="flex flex-wrap items-center gap-x-4 gap-y-1 px-3.5 py-2.5">
            <span className="text-info" aria-hidden="true">
              <Icon name="pullRequest" size={16} />
            </span>
            <div className="min-w-0 flex-1">
              <div className="text-ui font-medium text-fg">{heldActionLabel(r)}</div>
              <div className="text-caption text-muted">
                Requested by <span className="mono">{r.author}</span> {formatRelative(r.created_at)}
                {r.expires_at ? ` · expires ${formatRelative(r.expires_at)}` : ''}
                {r.state === 'approved' ? ' · approved, waiting to run' : ''}
              </div>
            </div>
            <Link href={changeRequestHref(r.id)} className="text-ui font-medium text-accent hover:underline">
              View request
            </Link>
          </li>
        ))}
      </ul>
    </Card>
  );
}

function PauseCard() {
  const org = scopeOrganization(useScope((s) => s.scope));
  const status = useDanger((s) => s.status);
  const loadStatus = useDanger((s) => s.loadStatus);
  const loadPending = useDanger((s) => s.loadPending);
  const timeZone = usePreferences((s) => s.timeZone);
  const [open, setOpen] = useState(false);
  const [minutes, setMinutes] = useState('15');
  const pause = (status.data?.ingress_pauses ?? []).find((p) => (p.organization ?? null) === org);
  return (
    <Card
      danger
      title="Pause all traffic"
      description={`The gateway refuses new requests ${org ? `for ${org}` : 'from every app'} for the time you choose, then resumes automatically.`}
      actions={
        pause ? (
          <Gate operation="resumeIngress">
            <Button
              onClick={() =>
                void settingsApi
                  .resumeIngress(org ?? undefined)
                  .then(() => notify('success', 'Traffic resumed'))
                  .finally(() => void loadStatus())
              }
            >
              Resume traffic
            </Button>
          </Gate>
        ) : (
          <Gate operation="pauseIngress">
            <Button variant="danger-solid" onClick={() => setOpen(true)}>
              Pause traffic
            </Button>
          </Gate>
        )
      }
    >
      {pause ? (
        <p className="flex flex-wrap items-center gap-2 text-ui text-fg" role="status">
          <Badge tone="error" dot>
            Paused
          </Badge>
          Until {formatDateTime(pause.resumes_at, timeZone)}, by{' '}
          <span className="mono">{pause.paused_by}</span>: {pause.reason}
        </p>
      ) : (
        <ul className="list-disc pl-5 text-ui text-muted">
          <li>Requests already in progress finish.</li>
          <li>Apps get an error until the pause ends or someone resumes traffic.</li>
        </ul>
      )}
      <SecondPersonNote action="ingress_pause" />
      <ConfirmDialog
        open={open}
        onClose={() => setOpen(false)}
        title="Pause all traffic?"
        confirmLabel="Pause traffic"
        typeToConfirm={org ?? 'gateway'}
        withReason
        effect="New requests are refused until the pause ends or someone resumes it. Requests in progress finish."
        onConfirm={async (reason) => {
          try {
            await settingsApi.pauseIngress({
              duration: `${minutes || '15'}m`,
              reason: reason ?? 'Paused from the console',
              ...(org ? { organization: org } : {}),
            });
            const until = new Date(Date.now() + Number(minutes || '15') * 60_000);
            notify(
              'success',
              `Traffic paused until ${until.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`,
            );
          } finally {
            void loadStatus();
            void loadPending();
          }
        }}
      >
        <Field label="Pause for (minutes)" hint="From 1 minute to 24 hours.">
          <Input
            mono
            value={minutes}
            onChange={(e) => setMinutes(e.target.value.replace(/\D/g, '').slice(0, 4))}
          />
        </Field>
      </ConfirmDialog>
    </Card>
  );
}

function DryRunCard() {
  const scope = useScope((s) => s.scope);
  const status = useDanger((s) => s.status);
  const setDryRun = useDanger((s) => s.setDryRun);
  const canEdit = useCan('patchRoutingSettings');
  const [confirm, setConfirm] = useState<boolean | null>(null);
  const on = status.data?.dry_run_all === true;
  return (
    <Card
      danger
      title="Dry run every request"
      description="Every request is answered with the model the gateway would choose and its estimated cost. No provider is called and nothing is spent."
      actions={
        <Switch
          label="Dry run every request"
          hideLabel
          checked={on}
          disabled={!canEdit || !status.data}
          disabledReason="You don't have permission to change routing settings."
          onChange={(next) => setConfirm(next)}
        />
      }
    >
      <ul className="list-disc pl-5 text-ui text-muted">
        <li>For staging gateways and routing rollouts, not for live traffic: apps get no real answers.</li>
        <li>
          Applies to the whole gateway
          {scope.kind === 'organization' ? ', not only this organization' : ''}. Every page shows a notice
          while it&apos;s on.
        </li>
      </ul>
      <ConfirmDialog
        open={confirm !== null}
        onClose={() => setConfirm(null)}
        title={confirm ? 'Dry run every request?' : 'Turn off the dry run?'}
        tone={confirm ? 'danger' : 'primary'}
        confirmLabel={confirm ? 'Start dry run' : 'Turn off dry run'}
        typeToConfirm={confirm ? 'dry run' : undefined}
        withReason
        effect={
          confirm
            ? 'Apps stop getting real answers until someone turns it off. Each answer names the model the gateway would have used and what it would have cost.'
            : 'Requests go to providers again and are charged as usual.'
        }
        onConfirm={async (reason) => {
          await setDryRun(confirm === true, reason);
          void useShell.getState().loadPending();
          notify('success', confirm ? 'Dry run on for every request' : 'Dry run turned off');
        }}
      />
    </Card>
  );
}

function PurgeCard() {
  const org = scopeOrganization(useScope((s) => s.scope));
  const loadPending = useDanger((s) => s.loadPending);
  const [open, setOpen] = useState(false);
  return (
    <Card
      danger
      title={org ? `Clear ${org}'s cache` : 'Clear the whole cache'}
      description="Removes every cached response, exact and similar matches alike. Requests go to providers until the cache fills again."
      actions={
        <Gate operation="invalidateCache">
          <Button variant="danger-solid" onClick={() => setOpen(true)}>
            Clear cache
          </Button>
        </Gate>
      }
    >
      <ul className="list-disc pl-5 text-ui text-muted">
        <li>Spend and response times rise for a while: expect several times the usual provider calls.</li>
        <li>
          To clear one model, team or route, use{' '}
          <Link href="/cache/" className="text-accent hover:underline">
            Cache
          </Link>
          .
        </li>
      </ul>
      {org ? null : <SecondPersonNote action="cache_flush_all" />}
      <ConfirmDialog
        open={open}
        onClose={() => setOpen(false)}
        title={org ? `Clear ${org}'s cache?` : 'Clear the whole cache?'}
        confirmLabel="Clear cache"
        typeToConfirm={org ?? 'gateway'}
        withReason
        effect="Every cached response is removed. Requests go to providers until the cache fills again."
        onConfirm={async (reason) => {
          try {
            await settingsApi.purgeCache(org, reason);
            notify('success', org ? `${org}'s cache cleared` : 'Cache cleared');
          } finally {
            void loadPending();
          }
        }}
      />
    </Card>
  );
}

function RotateCard() {
  const loadPending = useDanger((s) => s.loadPending);
  const [open, setOpen] = useState(false);
  return (
    <Card
      danger
      title="Rotate gateway secrets"
      description="Signs everyone out of the console and replaces the SCIM token and webhook signing secrets."
      actions={
        <Gate operation="rotateGatewaySecrets">
          <Button variant="danger-solid" onClick={() => setOpen(true)}>
            Rotate secrets
          </Button>
        </Gate>
      }
    >
      <ul className="list-disc pl-5 text-ui text-muted">
        <li>Everyone, including you, has to sign in again.</li>
        <li>Your identity provider needs the new SCIM token.</li>
        <li>Webhook receivers need the new signing secret.</li>
      </ul>
      <SecondPersonNote action="secrets_rotate" />
      <ConfirmDialog
        open={open}
        onClose={() => setOpen(false)}
        title="Rotate gateway secrets?"
        confirmLabel="Rotate secrets"
        typeToConfirm="rotate"
        withReason
        effect="Everyone is signed out of the console, including you. Integrations using the SCIM token or webhook secrets must be updated."
        onConfirm={async (reason) => {
          try {
            await settingsApi.rotateSecrets(['sessions', 'scim_token', 'webhook_secrets'], reason);
            notify('success', 'Gateway secrets rotated');
          } finally {
            void loadPending();
          }
        }}
      />
    </Card>
  );
}

function OrganizationPicker({ value, onChange }: { value: string | null; onChange: (v: string) => void }) {
  const organizations = useDanger((s) => s.organizations);
  return (
    <Field label="Organization" hint="Or switch to an organization at the top.">
      <Select
        value={value ?? ''}
        onChange={onChange}
        emptyOption="Choose an organization"
        options={(organizations.data ?? []).map((o) => ({
          value: o.name,
          label: o.spec.display_name ? `${o.spec.display_name} (${o.name})` : o.name,
        }))}
      />
    </Field>
  );
}

function ExportStatus() {
  const job = useDanger((s) => s.job);
  const organization = useDanger((s) => s.jobOrganization);
  const pollJob = useDanger((s) => s.pollJob);
  const clearJob = useDanger((s) => s.clearJob);
  const timeZone = usePreferences((s) => s.timeZone);
  const running = job !== null && (job.state === 'queued' || job.state === 'running');
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => void pollJob(), 2000);
    return () => clearInterval(timer);
  }, [running, pollJob]);
  if (!job) return null;
  return (
    <div className="flex flex-col gap-2 rounded-control border border-line px-3.5 py-3" role="status">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-ui text-fg">
          Export of <span className="mono">{organization}</span>
        </span>
        {running ? (
          <Badge tone="info" dot>
            {Math.round((job.progress ?? 0) * 100)}% done
          </Badge>
        ) : job.state === 'succeeded' ? (
          <Badge tone="success" dot>
            Ready
          </Badge>
        ) : (
          <Badge tone="error" dot>
            Failed
          </Badge>
        )}
      </div>
      {running ? <Meter value={job.progress ?? 0} max={1} label="Export progress" /> : null}
      {job.state === 'failed' ? (
        <p className="text-caption text-error">{job.error ?? 'The export stopped. Try again.'}</p>
      ) : null}
      {job.state === 'succeeded' && job.download_url ? (
        <div className="flex flex-wrap items-center gap-3">
          <a
            href={`${API_BASE}${job.download_url.replace(/^\/admin\/v1/, '')}`}
            download
            className="inline-flex items-center gap-1.5 text-ui font-medium text-accent hover:underline"
          >
            <Icon name="download" size={14} />
            Download ZIP
          </a>
          <span className="text-caption text-muted">
            {job.expires_at ? `Link works until ${formatDateTime(job.expires_at, timeZone)}. ` : ''}Only you
            can download it.
          </span>
          <Button size="sm" variant="ghost" onClick={clearJob}>
            Dismiss
          </Button>
        </div>
      ) : null}
    </div>
  );
}

/** One action inside a card: a title, what it does, its button, and its state below. */
function Block({
  title,
  description,
  action,
  children,
}: {
  title: string;
  description: string;
  action: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2 border-t border-line pt-4 first:border-t-0 first:pt-0">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-[1_1_16rem]">
          <h4 className="text-ui font-semibold text-fg">{title}</h4>
          <p className="mt-0.5 text-ui text-muted">{description}</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">{action}</div>
      </div>
      {children}
    </div>
  );
}

function ExportBlock({ target }: { target: string | null }) {
  const startExport = useDanger((s) => s.startExport);
  const job = useDanger((s) => s.job);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const running = job !== null && (job.state === 'queued' || job.state === 'running');
  return (
    <Block
      title="Export"
      description="A ZIP of its configuration (without secrets), audit entries, spend lines and request log records. Prompts and responses are included only if you can read them."
      action={
        <Gate operation="exportOrganization">
          <Button
            icon="download"
            busy={busy}
            disabled={!target || running}
            disabledReason={running ? 'An export is still running' : 'Choose an organization first'}
            onClick={() => {
              if (!target) return;
              setBusy(true);
              setError(null);
              startExport(target)
                .then(() => notify('info', 'Export started', target))
                .catch(setError)
                .finally(() => setBusy(false));
            }}
          >
            Start export
          </Button>
        </Gate>
      }
    >
      <InlineError error={error} />
      <ExportStatus />
    </Block>
  );
}

const GRACES = [
  { value: '1d', label: '1 day' },
  { value: '7d', label: '7 days' },
  { value: '30d', label: '30 days' },
  { value: '90d', label: '90 days' },
];

function DeletionBlock({ target }: { target: string | null }) {
  const organization = useDanger((s) => s.target);
  const loadTarget = useDanger((s) => s.loadTarget);
  const loadPending = useDanger((s) => s.loadPending);
  const timeZone = usePreferences((s) => s.timeZone);
  const [dialog, setDialog] = useState<'schedule' | 'cancel' | null>(null);
  const [grace, setGrace] = useState('30d');
  useEffect(() => {
    if (target) void loadTarget(target);
  }, [target, loadTarget]);
  const current = organization.data?.name === target ? organization.data : null;
  const deletion = current?.metadata.deletion;
  const isDefault = target === 'default';
  const days = deletion ? daysUntil(deletion.delete_at) : null;
  return (
    <Block
      title="Delete"
      description="It's turned off at once, then deleted with its teams, projects and keys when the waiting period ends. You can cancel until then."
      action={
        deletion?.state === 'scheduled' ? (
          <Gate operation="cancelOrganizationDeletion">
            <Button onClick={() => setDialog('cancel')}>Cancel deletion</Button>
          </Gate>
        ) : (
          <Gate operation="scheduleOrganizationDeletion">
            <Button
              variant="danger-solid"
              disabled={!current || isDefault}
              disabledReason={
                isDefault ? "The default organization can't be deleted" : 'Choose an organization first'
              }
              onClick={() => setDialog('schedule')}
            >
              Schedule deletion
            </Button>
          </Gate>
        )
      }
    >
      {organization.error && organization.data?.name !== target ? (
        <ErrorState error={organization.error} subject="The organization" size="sm" />
      ) : null}
      {deletion?.state === 'scheduled' ? (
        <p className="flex flex-wrap items-center gap-2 text-ui text-fg" role="status">
          <Badge tone="error" dot>
            Deletion scheduled
          </Badge>
          <span>
            <span className="mono">{target}</span> is turned off and will be deleted on{' '}
            {formatDateTime(deletion.delete_at, timeZone)}
            {days !== null && days > 0 ? ` (in ${days} ${days === 1 ? 'day' : 'days'})` : ''}. Scheduled by{' '}
            <span className="mono">{deletion.scheduled_by}</span>.
          </span>
        </p>
      ) : deletion?.state === 'failed' ? (
        <p className="flex flex-wrap items-center gap-2 text-ui text-fg" role="status">
          <Badge tone="warn" dot>
            Deletion failed
          </Badge>
          {deletion.error ?? 'The organization could not be removed.'} It stays turned off; schedule it again
          or turn it back on in Teams &amp; users.
        </p>
      ) : (
        <ul className="list-disc pl-5 text-ui text-muted">
          <li>Its keys stop working at once; people with roles only there lose access.</li>
          <li>
            When it&apos;s deleted, its request log, spend and stored responses are erased. The audit log is
            kept.
          </li>
          <li>Export it first if you need a copy.</li>
        </ul>
      )}
      <SecondPersonNote action="organization_delete" />
      <ConfirmDialog
        open={dialog === 'schedule'}
        onClose={() => setDialog(null)}
        title={`Delete organization ${target ?? ''}?`}
        objectName={target ?? undefined}
        confirmLabel="Schedule deletion"
        typeToConfirm={target ?? undefined}
        withReason
        effect="The organization is turned off now and deleted with everything in it when the waiting period ends."
        onConfirm={async (reason) => {
          if (!target) return;
          try {
            const r = await settingsApi.scheduleDeletion(target, grace, reason);
            notify(
              'success',
              'Deletion scheduled',
              `${target} will be deleted on ${formatDateTime(r.delete_at, timeZone)}.`,
            );
          } finally {
            void loadTarget(target);
            void loadPending();
          }
        }}
      >
        <Field label="Delete it after">
          <Select value={grace} onChange={setGrace} options={GRACES} />
        </Field>
      </ConfirmDialog>
      <ConfirmDialog
        open={dialog === 'cancel'}
        onClose={() => setDialog(null)}
        title={`Cancel the deletion of ${target ?? ''}?`}
        tone="primary"
        confirmLabel="Cancel deletion"
        withReason
        effect="The organization is kept and turned back on as it was before."
        onConfirm={async (reason) => {
          if (!target) return;
          await settingsApi.cancelDeletion(target, reason);
          notify('success', 'Deletion cancelled', target);
          await loadTarget(target);
        }}
      />
    </Block>
  );
}

/** Export and scheduled deletion of an organization: the scope's, or one picked here. */
function OrganizationCard() {
  const org = scopeOrganization(useScope((s) => s.scope));
  const [picked, setPicked] = useState<string | null>(null);
  const target = org ?? picked;
  return (
    <Card
      danger
      title={org ? `Export or delete ${org}` : 'Export or delete an organization'}
      description="To offboard a tenant: take a copy of everything it has, then remove it after a waiting period."
    >
      <div className="flex flex-col gap-4">
        {org ? null : <OrganizationPicker value={picked} onChange={setPicked} />}
        <ExportBlock target={target} />
        <DeletionBlock target={target} />
      </div>
    </Card>
  );
}

const TTLS = ['5m', '15m', '30m', '1h', '2h', '6h', '12h', '24h'];

function ttlLabel(v: string): string {
  const m = /^(\d+)(m|h)$/.exec(v);
  if (!m) return v;
  const n = Number(m[1]);
  return m[2] === 'm' ? `${n} minutes` : n === 1 ? '1 hour' : `${n} hours`;
}

function TwoPersonForm({ initial }: { initial: { actions: DualAction[]; ttl: string } }) {
  const saveGovernance = useDanger((s) => s.saveGovernance);
  const canEdit = useCan('patchGovernanceSettings');
  const [values, setValues] = useState(initial);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const dirty =
    values.ttl !== initial.ttl || [...values.actions].sort().join() !== [...initial.actions].sort().join();
  const ttls = TTLS.includes(values.ttl) ? TTLS : [...TTLS, values.ttl];
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
        saveGovernance({ dual_control: { actions: values.actions, approval_ttl: values.ttl } })
          .then(() => notify('success', 'Two-person rule saved'))
          .catch(setError)
          .finally(() => setBusy(false));
      }}
    >
      <InlineError error={error} />
      <fieldset className="grid grid-cols-2 gap-x-6 gap-y-3 max-sm:grid-cols-1">
        <legend className="mb-2 text-ui font-medium text-fg">Actions that need a second person</legend>
        {DUAL_ACTIONS.map((a) => (
          <Checkbox
            key={a.value}
            label={a.label}
            description={a.description}
            checked={values.actions.includes(a.value)}
            disabled={!canEdit}
            onChange={(on) =>
              setValues({
                ...values,
                actions: on ? [...values.actions, a.value] : values.actions.filter((x) => x !== a.value),
              })
            }
          />
        ))}
      </fieldset>
      <Field label="A request expires after" hint="If nobody approves it by then, the action doesn't run.">
        <Select
          value={values.ttl}
          disabled={!canEdit}
          onChange={(ttl) => setValues({ ...values, ttl })}
          options={ttls.map((v) => ({ value: v, label: ttlLabel(v) }))}
        />
      </Field>
      <SaveBar dirty={dirty} busy={busy} canEdit={canEdit} onDiscard={() => setValues(initial)} />
    </form>
  );
}

function TwoPersonCard() {
  const governance = useDanger((s) => s.governance);
  const dual = governance.data?.spec.dual_control;
  return (
    <Card
      title="Two-person rule"
      description="The actions here wait for a second person's approval, who needs the same permission. The request shows in Change requests."
    >
      {governance.error ? (
        <ErrorState error={governance.error} subject="The two-person rule" size="sm" />
      ) : null}
      {governance.status === 'loading' && !governance.data ? <LoadingRows rows={3} /> : null}
      {governance.data ? (
        <TwoPersonForm
          key={governance.data.metadata.etag}
          initial={{ actions: dual?.actions ?? [], ttl: dual?.approval_ttl ?? '1h' }}
        />
      ) : null}
    </Card>
  );
}

/** Danger zone: destructive operations with their confirmations, held actions, the two-person rule and windows. */
export function DangerSection() {
  const load = useDanger((s) => s.load);
  const loadStatus = useDanger((s) => s.loadStatus);
  const loadPending = useDanger((s) => s.loadPending);
  const pending = useDanger((s) => s.pending);
  useEffect(() => {
    void load();
  }, [load]);
  usePolling(() => {
    void loadStatus();
    void loadPending();
  });
  return (
    <>
      {pending.data && pending.data.length > 0 ? <WaitingCard /> : null}
      <PauseCard />
      <DryRunCard />
      <PurgeCard />
      <RotateCard />
      <OrganizationCard />
      <TwoPersonCard />
      <MaintenanceCard />
      {pending.error ? null : (
        <p className="text-caption text-muted">
          Held actions and their approvals are listed in{' '}
          <Link href="/change-requests/" className="text-accent hover:underline">
            Change requests
          </Link>
          .
        </p>
      )}
    </>
  );
}
