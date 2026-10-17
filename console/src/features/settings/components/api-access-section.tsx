'use client';

import { useEffect, useMemo, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Combobox, Field, Input, MultiSelect, NumberInput, Select, Textarea } from '@/components/ui/form';
import { permissionOf, useCan } from '@/components/ui/gate';
import { noPermissionReason } from '@/lib/permissions';
import { MenuButton } from '@/components/ui/menu';
import { CancelButton, Dialog } from '@/components/ui/overlay';
import { EmptyState, ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { modelOption } from '@/components/ui/marks';
import { formatCount, plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import type { FieldErrors } from '@/lib/validation';
import { scopeOrganization, useScope } from '@/stores/scope';
import { notify } from '@/stores/ui';

import { type LimitSimulationResult, settingsApi } from '../api';
import { type PriorityMode, sharesErrors, sharesFrom, sharesText, sharesTo } from '../priority';
import { detailErrors, DURATION_RE, NAME_RE, same, splitList } from '../form-errors';
import { useOrgSettings } from '../store';
import { Card, SaveBar } from './parts';
import { PriorityShares } from './priority-shares';
import { useEdited } from '@/lib/hooks';

const CIDR_RE = /^(\d{1,3}(\.\d{1,3}){3}(\/\d{1,2})?|[0-9a-fA-F:]+:[0-9a-fA-F:.]*(\/\d{1,3})?)$/;

/** The scope's organization (the default one at gateway scope), loaded for the API access cards. */
function useOrganization() {
  const scope = useScope((s) => s.scope);
  const org = scopeOrganization(scope) ?? 'default';
  const organization = useOrgSettings((s) => s.organization);
  const load = useOrgSettings((s) => s.loadOrganization);
  const loadSettings = useOrgSettings((s) => s.loadSettings);
  useEffect(() => {
    void load(org);
  }, [org, load]);
  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);
  return { org, organization, ready: organization.data?.name === org ? organization.data : null };
}

/* ---------------------------------------------------------------- IP allow-list */

function AllowListForm({ org, initial, sets }: { org: string; initial: string[]; sets: string[] }) {
  const save = useOrgSettings((s) => s.saveOrganization);
  const canEdit = useCan('patchOrganization');
  const split = (list: string[]) => ({
    ranges: list.filter((v) => !v.startsWith('set:')),
    sets: list.filter((v) => v.startsWith('set:')).map((v) => v.slice(4)),
  });
  const start = split(initial);
  const [ranges, setRanges] = useState(start.ranges);
  const [named, setNamed] = useState(start.sets);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const next = [...ranges, ...named.map((n) => `set:${n}`)];
  const dirty = !same(next, initial);
  async function submit() {
    const bad = ranges.find((r) => !CIDR_RE.test(r));
    if (bad) {
      setErrors({ ranges: `"${bad}" isn't an address or range such as 10.0.0.0/8` });
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      await save(org, { allowed_ips: next });
      notify('success', next.length ? 'IP allow-list saved' : 'IP allow-list removed', org);
    } catch (e) {
      setError(e);
      setErrors(detailErrors(e, { [`organizations.${org}.allowed_ips`]: 'ranges' }));
    } finally {
      setBusy(false);
    }
  }
  return (
    <form
      noValidate
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <InlineError error={error} />
      <p className="text-ui text-fg">
        {next.length === 0
          ? 'Off. Apps can call from any address with a valid key.'
          : `On. Requests from other addresses are refused, whatever the key (${plural(next.length, 'entry', 'entries')}).`}
      </p>
      <Field
        label="Addresses and ranges"
        error={errors.ranges}
        hint="IPv4 or IPv6, a single address or a CIDR range such as 10.0.0.0/8. Press Enter after each."
      >
        <MultiSelect
          creatable
          noun="range"
          placeholder="10.0.0.0/8"
          disabled={!canEdit}
          value={ranges}
          options={ranges.map((r) => ({ value: r, label: r }))}
          onChange={(v) => {
            setRanges(v.map((r) => r.trim()));
            setErrors({});
          }}
        />
      </Field>
      <Field
        label="IP sets"
        hint={
          sets.length ? 'A change to a set applies everywhere it is used.' : 'No IP sets yet; add one below.'
        }
      >
        <MultiSelect
          noun="set"
          disabled={!canEdit || sets.length === 0}
          value={named}
          options={sets.map((s) => ({ value: s, label: s }))}
          onChange={setNamed}
        />
      </Field>
      <SaveBar
        dirty={dirty}
        busy={busy}
        canEdit={canEdit}
        onDiscard={() => {
          setRanges(start.ranges);
          setNamed(start.sets);
        }}
      />
    </form>
  );
}

function IpSetDialog({
  open,
  name,
  ranges,
  taken,
  onClose,
}: {
  open: boolean;
  name: string | null;
  ranges: string[];
  taken: string[];
  onClose: () => void;
}) {
  const saveAuth = useOrgSettings((s) => s.saveAuth);
  const [values, setValues] = useState({ name: name ?? '', ranges: ranges.join('\n') });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited(values);
  async function submit() {
    const list = splitList(values.ranges);
    const e: FieldErrors = {};
    if (!name) {
      if (!NAME_RE.test(values.name)) e.name = 'Use letters, digits, . _ and -; start with a letter or digit';
      else if (taken.includes(values.name)) e.name = 'A set already has this name';
    }
    const bad = list.find((r) => !CIDR_RE.test(r));
    if (list.length === 0) e.ranges = 'Enter at least one address or range';
    else if (bad) e.ranges = `"${bad}" isn't an address or range such as 10.0.0.0/8`;
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    try {
      await saveAuth({ ip_sets: { [values.name]: list } });
      notify('success', name ? 'IP set saved' : 'IP set added', values.name);
      onClose();
    } catch (err) {
      setError(err);
      setErrors(detailErrors(err, { [`auth.ip_sets.${values.name}`]: 'ranges' }));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      dirty={edited}
      open={open}
      onClose={onClose}
      title={name ? `IP set ${name}` : 'New IP set'}
      description="A named list of ranges that organizations, teams, projects and keys can allow by name."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            {name ? 'Save set' : 'Add set'}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <InlineError error={error} />
        {name ? null : (
          <Field label="Name" required error={errors.name}>
            <Input
              mono
              data-autofocus
              value={values.name}
              placeholder="office"
              onChange={(e) => setValues({ ...values, name: e.target.value })}
            />
          </Field>
        )}
        <Field label="Ranges" required error={errors.ranges} hint="One per line, or separated by commas.">
          <Textarea
            mono
            rows={6}
            value={values.ranges}
            placeholder={'203.0.113.0/24\n2001:db8::/32'}
            onChange={(e) => setValues({ ...values, ranges: e.target.value })}
          />
        </Field>
      </div>
    </Dialog>
  );
}

function IpSets() {
  const auth = useOrgSettings((s) => s.auth);
  const saveAuth = useOrgSettings((s) => s.saveAuth);
  const canEdit = useCan('patchAuthSettings');
  const sets = auth.data?.spec.ip_sets ?? {};
  const [editing, setEditing] = useState<{ name: string | null; key: number } | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const reason = canEdit ? undefined : "You don't have permission to change IP sets.";
  const entries = Object.entries(sets);
  return (
    <div className="flex flex-col gap-2 border-t border-line pt-4">
      <div className="flex items-center justify-between gap-2">
        <h4 className="text-ui font-semibold text-fg">Named IP sets</h4>
        <Button
          size="sm"
          icon="plus"
          disabled={!canEdit}
          disabledReason={reason}
          onClick={() => setEditing({ name: null, key: Date.now() })}
        >
          New IP set
        </Button>
      </div>
      {entries.length === 0 ? (
        <p className="text-caption text-muted">
          Define an office or VPN range once and allow it by name here and on teams, projects and keys.
        </p>
      ) : (
        <Table label="IP sets">
          <thead>
            <tr>
              <Th>Set</Th>
              <Th>Ranges</Th>
              <Th>
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {entries.map(([name, ranges]) => (
              <Tr key={name}>
                <Td mono>{name}</Td>
                <Td mono muted className="max-w-[24rem] truncate">
                  {ranges.join(', ')}
                </Td>
                <Td>
                  <span className="flex justify-end">
                    <MenuButton
                      label={`More for ${name}`}
                      iconOnly
                      variant="ghost"
                      size="sm"
                      items={[
                        {
                          label: 'Edit',
                          icon: 'edit',
                          disabled: !canEdit,
                          disabledReason: reason,
                          onSelect: () => setEditing({ name, key: Date.now() }),
                        },
                        {
                          label: 'Remove',
                          icon: 'trash',
                          danger: true,
                          disabled: !canEdit,
                          disabledReason: reason,
                          onSelect: () => setRemoving(name),
                        },
                      ]}
                    />
                  </span>
                </Td>
              </Tr>
            ))}
          </tbody>
        </Table>
      )}
      {editing ? (
        <IpSetDialog
          key={editing.key}
          open
          name={editing.name}
          ranges={editing.name ? (sets[editing.name] ?? []) : []}
          taken={Object.keys(sets)}
          onClose={() => setEditing(null)}
        />
      ) : null}
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title={`Remove IP set ${removing ?? ''}?`}
        objectName={removing ?? undefined}
        confirmLabel="Remove set"
        effect="Allow-lists that name it must drop it first; the gateway refuses to remove a set still in use."
        onConfirm={async (r) => {
          if (!removing) return;
          await saveAuth({ ip_sets: { [removing]: null } }, r);
          notify('success', 'IP set removed', removing);
        }}
      />
    </div>
  );
}

function AllowListCard() {
  const { org, organization, ready } = useOrganization();
  const auth = useOrgSettings((s) => s.auth);
  const sets = Object.keys(auth.data?.spec.ip_sets ?? {});
  return (
    <Card
      title="IP allow-list"
      description={`Addresses the apps of the ${org} organization may call the gateway from.`}
    >
      {organization.error ? (
        <ErrorState error={organization.error} subject="The organization" size="sm" />
      ) : null}
      {!ready && !organization.error ? <LoadingRows rows={2} /> : null}
      {ready ? (
        <div className="flex flex-col gap-4">
          <AllowListForm
            key={`${ready.metadata.etag}${sets.join(',')}`}
            org={org}
            initial={ready.spec.allowed_ips ?? []}
            sets={sets}
          />
          <IpSets />
        </div>
      ) : null}
    </Card>
  );
}

/* ---------------------------------------------------------------- Default limits */

type LimitSpec = {
  unit: 'requests' | 'tokens' | 'concurrency';
  limit: number;
  per?: string;
  partition_by?: string[];
  enforce?: boolean;
  /** Key class → share of the limit, and whether classes may borrow. */
  priority_shares?: Record<string, number> | null;
  priority_mode?: PriorityMode | null;
};

const UNIT_WORD: Record<LimitSpec['unit'], string> = {
  requests: 'Requests',
  tokens: 'Tokens',
  concurrency: 'Requests at once',
};

const PER = [
  { value: '1s', label: 'per second' },
  { value: '1m', label: 'per minute' },
  { value: '1h', label: 'per hour' },
  { value: '1d', label: 'per day' },
];

const FOR = [
  { value: '', label: 'The whole organization' },
  { value: 'key', label: 'Each key' },
  { value: 'user', label: 'Each person' },
  { value: 'team', label: 'Each team' },
  { value: 'project', label: 'Each project' },
  { value: 'model', label: 'Each Model' },
];

function perWords(per: string | undefined): string {
  return PER.find((p) => p.value === per)?.label ?? (per ? `per ${per}` : '');
}

function forWords(partition: string[] | undefined): string {
  if (!partition?.length) return 'The whole organization';
  return partition.map((p) => FOR.find((f) => f.value === p)?.label ?? p).join(', ');
}

function LimitDialog({
  org,
  name,
  spec,
  taken,
  onClose,
}: {
  org: string;
  name: string | null;
  spec: LimitSpec | undefined;
  taken: string[];
  onClose: () => void;
}) {
  const save = useOrgSettings((s) => s.saveOrganization);
  const partition = spec?.partition_by ?? [];
  const simple = partition.length <= 1;
  const [values, setValues] = useState({
    name: name ?? '',
    unit: spec?.unit ?? 'requests',
    limit: spec ? String(spec.limit) : '',
    per: spec?.per ?? '1m',
    for: simple ? (partition[0] ?? '') : '',
  });
  const [shares, setShares] = useState(() => sharesFrom(spec?.priority_shares));
  const [mode, setMode] = useState<PriorityMode>(spec?.priority_mode === 'borrow' ? 'borrow' : 'strict');
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited({ values, shares, mode });
  const set = <K extends keyof typeof values>(k: K, v: (typeof values)[K]) =>
    setValues((x) => ({ ...x, [k]: v }));
  async function submit() {
    const e: FieldErrors = { ...sharesErrors(shares) };
    if (!name) {
      if (!/^[a-z0-9_]{1,64}$/.test(values.name)) e.name = 'Use lowercase letters, digits and _';
      else if (['locked', 'request_overridable'].includes(values.name)) e.name = 'This name is reserved';
      else if (taken.includes(values.name)) e.name = 'A limit already has this name';
    }
    if (!/^\d+$/.test(values.limit)) e.limit = 'Enter a whole number; 0 refuses every request';
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    try {
      const body: Record<string, unknown> = {
        unit: values.unit,
        limit: Number(values.limit),
        per: values.unit === 'concurrency' ? null : values.per,
        ...(simple ? { partition_by: values.for ? [values.for] : [] } : {}),
        priority_shares: sharesTo(shares),
        priority_mode: shares.length > 0 && mode === 'borrow' ? 'borrow' : null,
      };
      if (values.unit !== 'tokens' && spec?.unit === 'tokens')
        Object.assign(body, { tokens: null, weights: null });
      if (values.unit !== 'requests' && spec?.unit === 'requests') Object.assign(body, { count: null });
      await save(org, { policies: { limits: { [values.name]: body } } } as never);
      notify('success', name ? 'Limit saved' : 'Limit added', values.name);
      onClose();
    } catch (err) {
      setError(err);
      setErrors(
        detailErrors(err, {
          [`organizations.${org}.policies.limits.${values.name}.limit`]: 'limit',
          [`organizations.${org}.policies.limits.${values.name}.per`]: 'per',
          [`organizations.${org}.policies.limits.${values.name}`]: 'name',
          [`organizations.${org}.policies.limits.${values.name}.priority_shares`]: 'shares',
        }),
      );
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      dirty={edited}
      open
      onClose={onClose}
      title={name ? `Limit ${name}` : 'New default limit'}
      description="Counted for every request of this organization; teams, projects and keys can set tighter ones."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            {name ? 'Save limit' : 'Add limit'}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <InlineError error={error} />
        {name ? null : (
          <Field label="Name" required error={errors.name} hint="Shown in rate-limit headers and refusals.">
            <Input
              mono
              data-autofocus
              value={values.name}
              placeholder="org_rpm"
              onChange={(e) => set('name', e.target.value)}
            />
          </Field>
        )}
        <div className="grid grid-cols-3 gap-3 max-sm:grid-cols-1">
          <Field label="Counts">
            <Select
              value={values.unit}
              onChange={(v) => set('unit', v as LimitSpec['unit'])}
              options={(Object.keys(UNIT_WORD) as LimitSpec['unit'][]).map((u) => ({
                value: u,
                label: UNIT_WORD[u],
              }))}
            />
          </Field>
          <Field label="At most" required error={errors.limit}>
            <NumberInput value={values.limit} min={0} onChange={(v) => set('limit', v)} />
          </Field>
          {values.unit === 'concurrency' ? null : (
            <Field label="Window" error={errors.per}>
              <Select value={values.per} onChange={(v) => set('per', v)} options={PER} />
            </Field>
          )}
        </div>
        {simple ? (
          <Field label="Counted for">
            <Select value={values.for} onChange={(v) => set('for', v)} options={FOR} />
          </Field>
        ) : (
          <p className="text-caption text-muted">
            Counted for {forWords(partition)}; change that in the configuration.
          </p>
        )}
        <PriorityShares rows={shares} mode={mode} onRows={setShares} onMode={setMode} errors={errors} />
      </div>
    </Dialog>
  );
}

function LimitsCard() {
  const { org, organization, ready } = useOrganization();
  const save = useOrgSettings((s) => s.saveOrganization);
  const canEdit = useCan('patchOrganization');
  const [editing, setEditing] = useState<{ name: string | null; key: number } | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const limits = Object.entries(
    (ready?.spec.policies as { limits?: Record<string, unknown> } | undefined)?.limits ?? {},
  ).filter(([k]) => k !== 'locked' && k !== 'request_overridable') as [string, LimitSpec][];
  const reason = canEdit ? undefined : "You don't have permission to change limits.";
  return (
    <Card
      title="Default rate limits"
      description={`Ceilings for every app of the ${org} organization. Teams, projects and keys can set tighter ones.`}
      actions={
        <Button
          size="sm"
          icon="plus"
          disabled={!canEdit || !ready}
          disabledReason={reason}
          onClick={() => setEditing({ name: null, key: Date.now() })}
        >
          New limit
        </Button>
      }
    >
      {organization.error ? (
        <ErrorState error={organization.error} subject="The organization" size="sm" />
      ) : null}
      {ready && limits.length === 0 ? (
        <EmptyState
          size="sm"
          title="No default limits"
          description="Keys, teams, projects and the gateway still limit requests."
        />
      ) : null}
      {limits.length > 0 ? (
        <Table label="Default rate limits">
          <thead>
            <tr>
              <Th>Limit</Th>
              <Th numeric>At most</Th>
              <Th>Counted for</Th>
              <Th>
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {limits.map(([name, l]) => (
              <Tr key={name}>
                <Td>
                  <span className="mono">{name}</span>
                  {l.enforce === false ? (
                    <Badge tone="info" className="ml-2">
                      Measured only
                    </Badge>
                  ) : null}
                </Td>
                <Td numeric>
                  {formatCount(l.limit)} {UNIT_WORD[l.unit].toLowerCase()} {perWords(l.per)}
                </Td>
                <Td muted>
                  {forWords(l.partition_by)}
                  {l.priority_shares && Object.keys(l.priority_shares).length > 0 ? (
                    <span className="block text-caption">
                      Shares: {sharesText(l.priority_shares, l.priority_mode)}
                    </span>
                  ) : null}
                </Td>
                <Td>
                  <span className="flex justify-end">
                    <MenuButton
                      label={`More for ${name}`}
                      iconOnly
                      variant="ghost"
                      size="sm"
                      items={[
                        {
                          label: 'Edit',
                          icon: 'edit',
                          disabled: !canEdit,
                          disabledReason: reason,
                          onSelect: () => setEditing({ name, key: Date.now() }),
                        },
                        {
                          label: 'Remove',
                          icon: 'trash',
                          danger: true,
                          disabled: !canEdit,
                          disabledReason: reason,
                          onSelect: () => setRemoving(name),
                        },
                      ]}
                    />
                  </span>
                </Td>
              </Tr>
            ))}
          </tbody>
        </Table>
      ) : null}
      {editing && ready ? (
        <LimitDialog
          key={editing.key}
          org={org}
          name={editing.name}
          spec={limits.find(([n]) => n === editing.name)?.[1]}
          taken={limits.map(([n]) => n)}
          onClose={() => setEditing(null)}
        />
      ) : null}
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title={`Remove limit ${removing ?? ''}?`}
        objectName={removing ?? undefined}
        confirmLabel="Remove limit"
        effect="Apps of this organization are no longer held to it; tighter limits elsewhere still apply."
        onConfirm={async (r) => {
          if (!removing) return;
          await save(org, { policies: { limits: { [removing]: null } } } as never, r);
          notify('success', 'Limit removed', removing);
        }}
      />
    </Card>
  );
}

/* ---------------------------------------------------------------- Key lifetimes */

const LIFETIMES = [
  { value: '', label: 'No limit' },
  { value: '30d', label: '30 days' },
  { value: '90d', label: '90 days' },
  { value: '180d', label: '180 days' },
  { value: '365d', label: '1 year' },
];

function KeyLifetimeForm({
  org,
  initial,
}: {
  org: string;
  initial: { max: string; def: string; reminders: string };
}) {
  const save = useOrgSettings((s) => s.saveOrganization);
  const canEdit = useCan('patchOrganization');
  const [values, setValues] = useState(initial);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const dirty = !same(values, initial);
  const options = (v: string, base: typeof LIFETIMES) =>
    base.some((o) => o.value === v) ? base : [...base, { value: v, label: v }];
  async function submit() {
    const e: FieldErrors = {};
    const reminders = splitList(values.reminders);
    if (reminders.some((d) => !/^\d{1,3}$/.test(d) || Number(d) < 1 || Number(d) > 365))
      e.reminders = 'Enter days from 1 to 365, separated by commas';
    else if (reminders.length > 8) e.reminders = 'Use at most 8 reminders';
    if (values.max && !DURATION_RE.test(values.max)) e.max = 'Enter a duration such as 90d';
    if (values.def && !DURATION_RE.test(values.def)) e.def = 'Enter a duration such as 30d';
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    try {
      await save(org, {
        max_key_lifetime: (values.max || null) as string | undefined,
        key_policy: {
          default_lifetime: (values.def || null) as string | undefined,
          reminder_days: (reminders.length ? reminders.map(Number) : null) as number[] | undefined,
        },
      });
      notify('success', 'Key lifetimes saved', org);
    } catch (err) {
      setError(err);
      setErrors(
        detailErrors(err, {
          [`organizations.${org}.max_key_lifetime`]: 'max',
          [`organizations.${org}.key_policy.default_lifetime`]: 'def',
          [`organizations.${org}.key_policy.reminder_days`]: 'reminders',
        }),
      );
    } finally {
      setBusy(false);
    }
  }
  return (
    <form
      noValidate
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <InlineError error={error} />
      <div className="grid grid-cols-3 gap-4 max-md:grid-cols-1">
        <Field label="Longest a key may live" error={errors.max} hint="Keys can't be created for longer.">
          <Select
            value={values.max}
            disabled={!canEdit}
            onChange={(v) => setValues({ ...values, max: v })}
            options={options(values.max, LIFETIMES)}
          />
        </Field>
        <Field
          label="New keys expire after"
          error={errors.def}
          hint="When the person creating one doesn't choose."
        >
          <Select
            value={values.def}
            disabled={!canEdit}
            onChange={(v) => setValues({ ...values, def: v })}
            options={options(values.def, [{ value: '', label: "Don't expire" }, ...LIFETIMES.slice(1)])}
          />
        </Field>
        <Field label="Remind owners, days before" error={errors.reminders} hint="For example 14, 7, 1.">
          <Input
            mono
            value={values.reminders}
            disabled={!canEdit}
            placeholder="14, 7, 3, 1"
            onChange={(e) => setValues({ ...values, reminders: e.target.value })}
          />
        </Field>
      </div>
      <SaveBar dirty={dirty} busy={busy} canEdit={canEdit} onDiscard={() => setValues(initial)} />
    </form>
  );
}

function KeyLifetimesCard() {
  const { org, ready } = useOrganization();
  return (
    <Card
      title="Key lifetimes"
      description="How long new API keys live, and when their owners hear about it."
    >
      {ready ? (
        <KeyLifetimeForm
          key={ready.metadata.etag}
          org={org}
          initial={{
            max: ready.spec.max_key_lifetime ?? '',
            def: ready.spec.key_policy?.default_lifetime ?? '',
            reminders: (ready.spec.key_policy?.reminder_days ?? [14, 7, 3, 1]).join(', '),
          }}
        />
      ) : (
        <LoadingRows rows={2} />
      )}
    </Card>
  );
}

/* ---------------------------------------------------------------- Simulation */

function SimulationResult({ result }: { result: LimitSimulationResult }) {
  const blocking = [
    ...result.limits.filter((l) => !l.admitted && l.enforced !== false).map((l) => `${l.level} ${l.name}`),
    ...result.budgets
      .filter((b) => !b.admitted && b.mode !== 'soft')
      .map((b) => `${b.level} budget ${b.name}`),
  ];
  return (
    <div className="flex flex-col gap-3">
      <p
        role="status"
        className={
          result.admitted
            ? 'rounded-control border border-line bg-sunken px-3.5 py-2.5 text-ui text-fg'
            : 'rounded-control border border-[color-mix(in_oklab,var(--ant-error)_40%,var(--ant-border))] px-3.5 py-2.5 text-ui text-fg'
        }
      >
        {result.admitted
          ? `${plural(result.count, 'such request', 'such requests')} sent now would all be let through.`
          : `${plural(result.count, 'such request', 'such requests')} sent now would be refused by ${blocking.join(', ') || 'a limit'}.`}{' '}
        <span className="text-muted">Nothing was sent and no counter changed.</span>
      </p>
      {result.limits.length === 0 && result.budgets.length === 0 ? (
        <p className="text-caption text-muted">No limit or budget applies to this key and Model.</p>
      ) : (
        <Table label="Limits and budgets that apply">
          <thead>
            <tr>
              <Th>Applies at</Th>
              <Th>Limit</Th>
              <Th numeric>This would take</Th>
              <Th numeric>Left</Th>
              <Th>Result</Th>
            </tr>
          </thead>
          <tbody>
            {result.limits.map((l, i) => (
              <Tr key={`l${i}`}>
                <Td>
                  {l.level} <span className="mono text-dim">{l.owner}</span>
                </Td>
                <Td>
                  <span className="mono">{l.name}</span>{' '}
                  <span className="text-muted">
                    {formatCount(l.limit)} {l.unit}
                    {l.per_s
                      ? ` per ${l.per_s >= 86400 ? `${l.per_s / 86400} d` : l.per_s >= 3600 ? `${l.per_s / 3600} h` : l.per_s >= 60 ? `${l.per_s / 60} min` : `${l.per_s} s`}`
                      : ' at once'}
                  </span>
                </Td>
                <Td numeric>{formatCount(l.requested)}</Td>
                <Td numeric>
                  {formatCount(l.remaining)}
                  {l.reset_at ? (
                    <span className="block text-caption text-dim">resets {formatRelative(l.reset_at)}</span>
                  ) : null}
                </Td>
                <Td>
                  {l.admitted ? (
                    <Badge tone="success">Fits</Badge>
                  ) : l.enforced === false ? (
                    <Badge tone="info">Over, measured only</Badge>
                  ) : (
                    <Badge tone="error">Refused</Badge>
                  )}
                </Td>
              </Tr>
            ))}
            {result.budgets.map((b, i) => (
              <Tr key={`b${i}`}>
                <Td>
                  {b.level} <span className="mono text-dim">{b.owner}</span>
                </Td>
                <Td>
                  <span className="mono">{b.name}</span>{' '}
                  <span className="text-muted">
                    budget of {b.unit === 'usd' ? `$${b.amount}` : `${b.amount} tokens`}
                  </span>
                </Td>
                <Td numeric>{b.unit === 'usd' ? `$${b.requested}` : b.requested}</Td>
                <Td numeric>{b.unit === 'usd' ? `$${b.remaining}` : b.remaining}</Td>
                <Td>
                  {b.admitted ? (
                    <Badge tone="success">Fits</Badge>
                  ) : b.mode === 'soft' ? (
                    <Badge tone="warn">Over, warns only</Badge>
                  ) : (
                    <Badge tone="error">Refused</Badge>
                  )}
                </Td>
              </Tr>
            ))}
          </tbody>
        </Table>
      )}
    </div>
  );
}

function SimulationCard() {
  const canSimulate = useCan('simulateLimits');
  const [keys, setKeys] = useState<{ value: string; label: string; description?: string }[]>([]);
  const [models, setModels] = useState<{ value: string; label: string }[]>([]);
  const [values, setValues] = useState({ key: '', model: '', input: '1000', output: '500', count: '1' });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<LimitSimulationResult | null>(null);
  useEffect(() => {
    settingsApi.keys().then(
      (ks) =>
        setKeys(
          ks.map((k) => ({
            value: k.name,
            label: k.name,
            description: [k.spec.project, k.spec.owner].filter(Boolean).join(' · ') || undefined,
          })),
        ),
      () => setKeys([]),
    );
    settingsApi.models().then(
      (ms) => setModels(ms.map((m) => modelOption(m))),
      () => setModels([]),
    );
  }, []);
  const set = (k: keyof typeof values, v: string) => {
    setValues((x) => ({ ...x, [k]: v }));
    setResult(null);
  };
  async function run() {
    const e: FieldErrors = {};
    if (!values.key) e.key = 'Choose a key';
    if (!values.model) e.model = 'Choose a Model';
    if (!/^[1-9]\d*$/.test(values.count)) e.count = 'Enter 1 or more';
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    setError(null);
    try {
      setResult(
        await settingsApi.simulate({
          key: values.key,
          model: values.model,
          input_tokens: Number(values.input || 0),
          output_tokens: Number(values.output || 0),
          count: Number(values.count),
        }),
      );
    } catch (err) {
      setError(err);
      setResult(null);
    } finally {
      setBusy(false);
    }
  }
  const keyOptions = useMemo(() => keys, [keys]);
  return (
    <Card
      title="Try a request against the limits"
      description="See which limits and budgets a request with this key and Model would meet right now, without sending it."
    >
      <form
        noValidate
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void run();
        }}
      >
        <InlineError error={error} />
        <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
          <Field label="Key" required error={errors.key}>
            <Combobox value={values.key} options={keyOptions} onChange={(v) => set('key', v)} />
          </Field>
          <Field label="Model" required error={errors.model}>
            <Combobox value={values.model} options={models} onChange={(v) => set('model', v)} />
          </Field>
        </div>
        <div className="grid grid-cols-3 gap-4 max-sm:grid-cols-1">
          <Field label="Input tokens">
            <NumberInput value={values.input} min={0} step={100} onChange={(v) => set('input', v)} />
          </Field>
          <Field label="Output tokens">
            <NumberInput value={values.output} min={0} step={100} onChange={(v) => set('output', v)} />
          </Field>
          <Field label="Requests at once" error={errors.count} hint="Would this many, sent now, all pass?">
            <NumberInput value={values.count} min={1} onChange={(v) => set('count', v)} />
          </Field>
        </div>
        <div>
          <Button
            type="submit"
            icon="play"
            busy={busy}
            disabled={!canSimulate}
            disabledReason={canSimulate ? undefined : noPermissionReason(permissionOf('simulateLimits'))}
          >
            Check limits
          </Button>
        </div>
        {result ? <SimulationResult result={result} /> : null}
      </form>
    </Card>
  );
}

/** The organization's API access: IP allow-list and sets, default limits, key lifetimes and a limit check. */
export function ApiAccessSettings() {
  return (
    <>
      <AllowListCard />
      <LimitsCard />
      <KeyLifetimesCard />
      <SimulationCard />
    </>
  );
}
