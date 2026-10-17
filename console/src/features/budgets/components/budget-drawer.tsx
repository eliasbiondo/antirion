'use client';

import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/choice';
import { joinValues, splitValues } from '@/components/ui/select';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Combobox, Field, Input, MultiSelect, Segmented, Select } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Icon } from '@/components/ui/icon';
import { DefinitionGrid } from '@/components/ui/layout';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { CopyLinkButton } from '@/components/ui/misc';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { modelOption } from '@/components/ui/marks';
import { cn } from '@/lib/cn';
import { useNow } from '@/lib/hooks';
import { formatMoney, formatPercent, plural } from '@/lib/format';
import { formatDate, formatRelative } from '@/lib/time';
import { serverFieldErrors, validate, type FieldErrors } from '@/lib/validation';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import {
  budgetSchema,
  type BudgetFormValues,
  type Enforcement,
  EMPTY_BUDGET,
  fromDeclaration,
  ROLLING_WINDOWS,
  toDeclarations,
  toOverride,
} from '../form';
import { useBudgets } from '../store';
import {
  type BudgetRow,
  isCounted,
  LEVEL_LABEL,
  partitionLabel,
  partitionNoun,
  runsOutWords,
} from '../types';
import { OverageDialog } from './overage-dialog';

const ENFORCEMENT = [
  { value: 'soft', label: 'Only notify' },
  { value: 'hard', label: 'Block new requests' },
  { value: 'approval', label: 'Block, and ask the notified person to approve going over' },
  { value: 'downgrade', label: 'Switch to a cheaper Model or tier' },
];

const PERIODS = [
  { value: 'daily', label: 'Every day' },
  { value: 'weekly', label: 'Every week' },
  { value: 'monthly', label: 'Every month' },
  { value: 'quarterly', label: 'Every quarter' },
  { value: 'yearly', label: 'Every year' },
  { value: 'lifetime', label: 'Never (one total)' },
  { value: 'rolling', label: 'Never: a rolling window' },
];

const WINDOW_LABEL: Record<string, string> = {
  '1h': 'Last hour',
  '24h': 'Last 24 hours',
  '7d': 'Last 7 days',
  '30d': 'Last 30 days',
};

const LEVELS = [
  { value: 'organization', label: 'Organization' },
  { value: 'team', label: 'Team' },
  { value: 'project', label: 'Project' },
  { value: 'key', label: 'Key' },
] as const;

const DAY = 86_400_000;

function amountText(v: unknown): string {
  return typeof v === 'string' || typeof v === 'number' ? String(v) : '';
}

/** The parent's amount, what its other budgets take, and what is left for this one. */
function Allocation({ row, parent, amount }: { row: BudgetRow; parent: BudgetRow; amount: number }) {
  const siblings = parent.children.filter(
    (c) => c.id !== row.id && c.kind === 'budget' && c.counter.unit === row.counter.unit,
  );
  const others = siblings.reduce((a, c) => a + c.amount, 0);
  const available = parent.amount - others;
  const over = Number.isFinite(amount) && amount > available;
  const level = (LEVEL_LABEL[parent.level] ?? parent.level).toLowerCase();
  const noun = `${(LEVEL_LABEL[row.level] ?? row.level).toLowerCase()}${siblings.length === 1 ? '' : 's'}`;
  return (
    <section aria-label="Allocation in the parent budget" className="flex flex-col gap-2">
      <h3 className="text-ui font-medium text-fg">Within the {level} budget</h3>
      <dl className="mono grid grid-cols-[1fr_auto] gap-x-4 gap-y-1 border-y border-line py-2 text-ui tabular-nums">
        <dt className="text-muted">
          {parent.owner} · {parent.name}
        </dt>
        <dd className="text-right text-fg">{formatMoney(parent.amount)}</dd>
        <dt className="text-muted">
          Other {noun} ({siblings.length})
        </dt>
        <dd className="text-right text-fg">{formatMoney(others)}</dd>
        <dt className="text-muted">Left for this one</dt>
        <dd className={cn('text-right font-semibold', available < 0 || over ? 'text-warn' : 'text-success')}>
          {formatMoney(available)}
        </dd>
      </dl>
      {over ? (
        <p className="text-caption text-warn">
          At {formatMoney(amount)}, the budgets inside {parent.owner} add up to {formatMoney(others + amount)}
          , {formatMoney(others + amount - parent.amount)} more than its amount. Every budget is still checked
          on its own; the {level} budget can refuse first.
        </p>
      ) : null}
    </section>
  );
}

/** What a budget's children take of it, from the gateway's count. */
function ChildrenAllocated({ row, amount }: { row: BudgetRow; amount: number }) {
  const allocated = row.counter.children_allocated ? Number(row.counter.children_allocated) : null;
  if (allocated === null) return null;
  const over = Number.isFinite(amount) && allocated > amount;
  return (
    <p
      className={cn(
        'rounded-control border px-3 py-2 text-ui',
        over
          ? 'border-[color-mix(in_oklab,var(--ant-warn)_40%,transparent)] text-fg'
          : 'border-line bg-sunken text-muted',
      )}
    >
      The budgets inside take {formatMoney(allocated)}
      {Number.isFinite(amount) && amount > 0
        ? ` (${formatPercent(allocated / amount, 0)} of ${formatMoney(amount)})`
        : ''}
      .{over ? ' That is more than this budget; each is still checked on its own.' : ''}
    </p>
  );
}

export interface BudgetDrawerProps {
  row: BudgetRow | null;
  /** The budget the row sits in, for its allocation. */
  parent?: BudgetRow | undefined;
  /** Opens with this enforcement chosen (the banner's "Downgrade"). */
  enforcement?: Enforcement | undefined;
  onClose: () => void;
}

/** Edits a budget (or declares a new one) on its owner, with the period, the what-if and the counter actions. */
export function BudgetDrawer({ row, parent, enforcement, onClose }: BudgetDrawerProps) {
  const timeZone = usePreferences((s) => s.timeZone);
  const now = useNow(60_000);
  const teams = useBudgets((s) => s.teams.data) ?? [];
  const projects = useBudgets((s) => s.projects.data) ?? [];
  const keys = useBudgets((s) => s.keys.data) ?? [];
  const models = useBudgets((s) => s.models.data) ?? [];
  const users = useBudgets((s) => s.users.data) ?? [];
  const save = useBudgets((s) => s.saveDeclaration);
  const reset = useBudgets((s) => s.reset);
  const adjust = useBudgets((s) => s.adjust);
  const readDeclaration = useBudgets((s) => s.readDeclaration);
  const [values, setValues] = useState<BudgetFormValues>(EMPTY_BUDGET);
  // The values as loaded, to tell unsaved edits apart.
  const [pristine, setPristine] = useState<BudgetFormValues>(EMPTY_BUDGET);
  const [decl, setDecl] = useState<Record<string, unknown> | undefined>(undefined);
  const [loaded, setLoaded] = useState(row === null);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [dialog, setDialog] = useState<'reset' | 'adjust' | 'delete' | 'approve' | null>(null);
  const [adjustBy, setAdjustBy] = useState('');
  const [advanced, setAdvanced] = useState(false);
  const [override, setOverride] = useState<{ own: boolean; amount: string }>({ own: false, amount: '' });
  const set = <K extends keyof BudgetFormValues>(key: K, value: BudgetFormValues[K]) => {
    setValues((v) => ({ ...v, [key]: value }));
    if (errors[key]) setErrors(({ [key]: _, ...rest }) => rest);
  };
  const canAdjust = useCan('adjustBudgetCounter');
  const canReset = useCan('resetBudgetCounter');
  const canGrant = useCan('grantBudgetOverage');
  const editable = row === null || ['organization', 'team', 'project', 'key'].includes(row.level);
  const partition = row?.kind === 'partition' ? row.partition : null;
  const counted = row !== null && isCounted(row);

  useEffect(() => {
    if (!row) return;
    let cancelled = false;
    readDeclaration(row.level, row.owner)
      .then((d) => {
        if (cancelled) return;
        const found = d?.budgets[row.name] as Record<string, unknown> | undefined;
        const cap = d?.budgets[`${row.name}_cap`] as Record<string, unknown> | undefined;
        const v = fromDeclaration(row.level as BudgetFormValues['level'], row.owner, row.name, found, cap);
        setValues(enforcement ? { ...v, enforcement } : v);
        setPristine(enforcement ? { ...v, enforcement } : v);
        setDecl(found);
        if (row.partition) {
          const overrides = (found?.overrides ?? {}) as Record<string, string | number>;
          const own = row.partition.value in overrides;
          setOverride({
            own,
            amount: own ? String(overrides[row.partition.value]) : amountText(found?.amount),
          });
        }
        setLoaded(true);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setError(e);
        setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [row, readDeclaration, enforcement]);

  const newAmount = Number(partition ? override.amount : values.amount);
  const whatIf =
    row && counted && !row.rolling && Number.isFinite(newAmount) && newAmount > 0
      ? {
          was: row.amount,
          headroom: newAmount - row.projected,
          crossesCap: row.projected > newAmount,
          crossesAlert: values.alerts
            .split(',')
            .map((p) => Number(p.trim()) / 100)
            .filter((r) => r > 0 && r < 1)
            .some((r) => row.projected > newAmount * r),
        }
      : null;

  async function submitOverride() {
    if (!row || !partition) return;
    const amount = override.amount.trim();
    if (override.own && !/^\d+(\.\d{1,6})?$/.test(amount)) {
      setErrors({ override: 'Enter an amount in dollars, like 25' });
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await save(row.level, row.owner, toOverride(row.name, partition.value, override.own ? amount : null));
      notify('success', 'Budget saved', `${partitionLabel(partition)} · ${row.name}`);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  async function submit() {
    if (partition) return submitOverride();
    const result = validate(budgetSchema, values);
    if (!result.ok) {
      setErrors(result.errors);
      if (result.errors.name) setAdvanced(true);
      return;
    }
    setErrors({});
    setBusy(true);
    setError(null);
    try {
      await save(values.level, values.owner, toDeclarations(values));
      notify('success', 'Budget saved', `${values.owner} · ${values.name}`);
      onClose();
    } catch (e) {
      setError(e);
      setErrors(serverFieldErrors(e, `${values.level}s.${values.owner}.policies.budgets.${values.name}`));
    } finally {
      setBusy(false);
    }
  }

  const owners =
    values.level === 'team'
      ? teams.map((t) => t.name)
      : values.level === 'project'
        ? projects.map((p) => p.name)
        : values.level === 'organization'
          ? [...new Set(teams.map((t) => t.spec.organization))]
          : keys.map((k) => k.name);

  const levelLabel = LEVELS.find((l) => l.value === values.level)?.label ?? 'Owner';
  const amountError = errors.amount ? 'Enter an amount in dollars, like 500' : undefined;
  const counterActions: MenuItem[] = [];
  if (row && counted && canGrant)
    counterActions.push({ label: 'Approve overage', icon: 'check', onSelect: () => setDialog('approve') });
  if (row && counted && canAdjust)
    counterActions.push({ label: 'Adjust amount spent', icon: 'edit', onSelect: () => setDialog('adjust') });
  if (row && counted && canReset)
    counterActions.push({ label: 'Reset this period', icon: 'refresh', onSelect: () => setDialog('reset') });
  if (row && editable && !partition)
    counterActions.push({
      label: 'Delete budget',
      icon: 'trash',
      danger: true,
      onSelect: () => setDialog('delete'),
    });

  const start = row ? new Date(row.counter.period_start).getTime() : NaN;
  const elapsedDays = Math.max(1, Math.ceil((now - start) / DAY));
  const title = row
    ? partition
      ? `${partitionLabel(partition)} · ${row.name}`
      : `${row.owner} · ${row.name}`
    : 'Create budget';
  const partitionedBy = Array.isArray(decl?.partition_by) ? (decl.partition_by as string[]) : [];

  return (
    <Drawer
      open
      onClose={onClose}
      dirty={editable && JSON.stringify(values) !== JSON.stringify(pristine)}
      headerActions={row ? <CopyLinkButton /> : undefined}
      title={title}
      subtitle={
        row
          ? partition
            ? `One ${partitionNoun(partition.attr, 1)}'s share of ${LEVEL_LABEL[row.level]?.toLowerCase() ?? row.level} ${row.owner}'s budget. Changes apply right away and are recorded in the audit log.`
            : `${LEVEL_LABEL[row.level] ?? row.level} budget. Changes apply right away and are recorded in the audit log.`
          : undefined
      }
      width="lg"
      footer={
        <>
          {counterActions.length > 0 ? (
            <span className="mr-auto">
              <MenuButton items={counterActions} label="More" align="left" />
            </span>
          ) : null}
          <CancelButton />
          <Button
            variant="primary"
            busy={busy}
            disabled={!loaded || !editable}
            disabledReason={
              !editable ? 'Gateway and model budgets are changed in the configuration' : undefined
            }
            onClick={() => void submit()}
          >
            {row ? 'Save changes' : 'Create budget'}
          </Button>
        </>
      }
    >
      <form
        noValidate
        className="flex flex-col gap-5"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <InlineError error={error} />
        {row && counted ? (
          <>
            <DefinitionGrid
              columns={4}
              items={[
                { label: 'Spent', value: formatMoney(row.spent) },
                { label: 'Held for requests in flight', value: formatMoney(row.reserved) },
                { label: 'Projected', value: row.rolling ? 'No forecast' : formatMoney(row.projected) },
                {
                  label: row.rolling ? 'Counting since' : 'Resets',
                  value: row.rolling
                    ? formatDate(row.counter.period_start, timeZone)
                    : row.counter.resets_at
                      ? formatDate(row.counter.resets_at, timeZone)
                      : 'Never',
                },
                {
                  label: 'Burn, last 7 days',
                  value: row.ratePerDay !== null ? `${formatMoney(row.ratePerDay)} a day` : '—',
                },
                {
                  label: 'Daily mean this period',
                  value: `${formatMoney(row.spent / elapsedDays)} a day`,
                },
                {
                  label: 'Runs out',
                  value: runsOutWords(row, (iso) => formatDate(iso, timeZone)),
                },
                {
                  label: 'Approved overage',
                  value:
                    row.effective > row.amount
                      ? `+${formatMoney(row.effective - row.amount)} · ${plural(row.counter.grants?.length ?? 0, 'approval')}`
                      : 'None',
                },
              ]}
            />
          </>
        ) : null}
        {row?.kind === 'budget' &&
        parent?.kind === 'budget' &&
        parent.counter.unit === row.counter.unit &&
        parent.counter.resets_at === row.counter.resets_at ? (
          <Allocation row={row} parent={parent} amount={Number(values.amount)} />
        ) : null}
        {row?.kind === 'budget' ? <ChildrenAllocated row={row} amount={Number(values.amount)} /> : null}
        {partition ? (
          <section className="flex flex-col gap-3">
            <Segmented<'shared' | 'own'>
              label="Amount for this one"
              value={override.own ? 'own' : 'shared'}
              onChange={(v) => setOverride({ ...override, own: v === 'own' })}
              options={[
                { value: 'shared', label: `The budget's amount (${formatMoney(amountText(decl?.amount))})` },
                { value: 'own', label: 'An amount of its own' },
              ]}
              className="w-fit max-w-full"
            />
            {override.own ? (
              <Field label="Amount (USD)" error={errors.override} required>
                <Input
                  mono
                  inputMode="decimal"
                  value={override.amount}
                  onChange={(e) => setOverride({ ...override, amount: e.target.value })}
                  data-autofocus
                />
              </Field>
            ) : null}
            <p className="text-caption text-muted">
              The rest of the budget (period, alerts, what happens when it&apos;s reached) is the same for
              every {partitionNoun(partition.attr, 1)}; edit it on {row?.owner} · {row?.name}.
            </p>
          </section>
        ) : (
          <>
            <div className="flex flex-col gap-1.5">
              <span className="text-ui font-medium text-fg">Applies to</span>
              {row ? (
                <p className="text-ui text-muted">
                  {levelLabel} <span className="mono text-fg">{values.owner}</span>
                  {partitionedBy.length > 0
                    ? ` · each ${partitionedBy.map((a) => partitionNoun(a, 1)).join(' and ')} with its own amount`
                    : ''}
                </p>
              ) : (
                <Segmented<BudgetFormValues['level']>
                  label="Applies to"
                  value={values.level}
                  onChange={(level) => setValues({ ...values, level, owner: '' })}
                  options={LEVELS.map((l) => ({ value: l.value, label: l.label }))}
                  className="w-fit"
                />
              )}
            </div>
            {row ? null : (
              <Field
                label={levelLabel}
                error={errors.owner ? `Choose a ${levelLabel.toLowerCase()}` : undefined}
                required
              >
                <Combobox
                  value={values.owner}
                  onChange={(v) => set('owner', v)}
                  placeholder={`Select ${values.level === 'organization' ? 'an' : 'a'} ${levelLabel.toLowerCase()}`}
                  options={owners.map((o) => ({ value: o, label: o }))}
                />
              </Field>
            )}
            <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
              <Field label="Amount (USD)" error={amountError} required>
                <Input
                  mono
                  inputMode="decimal"
                  value={values.amount}
                  onChange={(e) => set('amount', e.target.value)}
                  placeholder="e.g. 500"
                  data-autofocus
                />
              </Field>
              <Field label="Resets">
                <Select
                  value={values.period}
                  onChange={(v) => set('period', v as BudgetFormValues['period'])}
                  options={PERIODS}
                />
              </Field>
            </div>
            {values.period === 'rolling' ? (
              <Field
                label="Counts spend over the"
                error={errors.window}
                hint="Spend older than the window stops counting, a little at a time. A rolling budget has no forecast."
              >
                <Select
                  value={values.window}
                  onChange={(v) => set('window', v)}
                  options={[
                    ...ROLLING_WINDOWS.map((w) => ({ value: w, label: WINDOW_LABEL[w] ?? w })),
                    ...((ROLLING_WINDOWS as readonly string[]).includes(values.window)
                      ? []
                      : [{ value: values.window, label: `Last ${values.window}` }]),
                  ]}
                />
              </Field>
            ) : null}
            <Field
              label="Alert at"
              error={errors.alerts}
              hint="Percentages of the amount; type another to add it."
            >
              <MultiSelect
                mono
                creatable
                noun="alert"
                placeholder="No alerts"
                options={['50', '75', '80', '90', '100'].map((p) => ({ value: p, label: `${p}%` }))}
                value={splitValues(values.alerts)}
                onChange={(v) => set('alerts', joinValues(v))}
              />
            </Field>
            <Field label="When the budget is reached">
              <Select
                value={values.enforcement}
                onChange={(v) => set('enforcement', v as BudgetFormValues['enforcement'])}
                options={ENFORCEMENT}
              />
            </Field>
            {values.enforcement === 'soft' ? (
              <Field
                label="Hard limit, % of the amount"
                optional
                error={errors.hardCap}
                hint="New requests are refused once spend passes this percentage."
              >
                <Input
                  mono
                  value={values.hardCap}
                  onChange={(e) => set('hardCap', e.target.value)}
                  placeholder="e.g. 110"
                />
              </Field>
            ) : null}
            {values.enforcement === 'downgrade' ? (
              <div className="flex flex-col gap-4 rounded-control border border-line bg-sunken p-4">
                <div className="grid grid-cols-2 gap-4 max-sm:grid-cols-1">
                  <Field label="Switch at, % of the amount" error={errors.downgradeAt}>
                    <Input
                      mono
                      value={values.downgradeAt}
                      onChange={(e) => set('downgradeAt', e.target.value)}
                    />
                  </Field>
                  <div className="flex flex-col gap-1.5">
                    <span className="text-ui font-medium text-fg">Switch to</span>
                    <Segmented<'model' | 'tier'>
                      label="Switch to"
                      value={values.downgradeTo}
                      onChange={(v) => set('downgradeTo', v)}
                      options={[
                        { value: 'model', label: 'A Model' },
                        { value: 'tier', label: 'A tier' },
                      ]}
                      className="w-fit"
                    />
                  </div>
                </div>
                {values.downgradeTo === 'model' ? (
                  <Field
                    label="Model"
                    error={errors.downgradeModel}
                    hint="Requests this budget covers go to this Model instead, past the percentage."
                  >
                    <Combobox
                      value={values.downgradeModel}
                      onChange={(v) => set('downgradeModel', v)}
                      placeholder="Select a Model"
                      options={models.map((m) => modelOption(m))}
                    />
                  </Field>
                ) : (
                  <Field label="Tier" hint="The first enabled Model of the tier the key may use.">
                    <Select
                      value={values.downgradeTier}
                      onChange={(v) => set('downgradeTier', v)}
                      options={['fast', 'balanced', 'specialized'].map((t) => ({
                        value: t,
                        label: t.charAt(0).toUpperCase() + t.slice(1),
                      }))}
                    />
                  </Field>
                )}
              </div>
            ) : null}
            <Field label="Notify" optional hint="This person gets the alerts and can approve going over.">
              <Combobox
                value={values.budgetOwner}
                onChange={(v) => set('budgetOwner', v)}
                emptyOption="No one"
                options={users.map((u) => ({ value: u.name, label: u.spec.display_name ?? u.name }))}
              />
            </Field>
            <Checkbox
              checked={values.cacheBypass}
              onChange={(v) => set('cacheBypass', v)}
              label="Answer from the cache when the budget is spent"
              description="A request this budget refuses is still served when a cached answer exists, without charging it. Other budgets and limits still apply."
            />
          </>
        )}
        {whatIf ? (
          <p
            className={cn(
              'rounded-control border px-3 py-2 text-ui',
              whatIf.headroom < 0
                ? 'border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] text-fg'
                : 'border-line bg-sunken text-muted',
            )}
          >
            At {formatMoney(newAmount)} (was {formatMoney(whatIf.was)}), the projected{' '}
            {formatMoney(row?.projected)} is {formatPercent((row?.projected ?? 0) / newAmount, 0)} of the
            budget
            {whatIf.headroom < 0
              ? `, ${formatMoney(-whatIf.headroom)} over.`
              : `, leaving ${formatMoney(whatIf.headroom)}.`}
            {whatIf.crossesCap ? '' : whatIf.crossesAlert ? ' It will cross an alert threshold.' : ''}
          </p>
        ) : null}
        {row ? null : (
          <div className="flex flex-col gap-3 border-t border-line pt-4">
            <button
              type="button"
              aria-expanded={advanced}
              onClick={() => setAdvanced(!advanced)}
              className="-mx-1 flex items-center gap-2 rounded-xs px-1 text-left"
            >
              <Icon
                name="chevronRight"
                size={14}
                className={cn('text-muted transition-transform', advanced && 'rotate-90')}
              />
              <span className="text-ui font-semibold text-fg">Advanced</span>
              <span className="text-caption text-muted">Budget name</span>
            </button>
            {advanced ? (
              <Field
                label="Name"
                error={errors.name ? 'Use lowercase letters, numbers and _' : undefined}
                hint="Tells budgets on the same owner apart."
              >
                <Input mono value={values.name} onChange={(e) => set('name', e.target.value)} />
              </Field>
            ) : null}
          </div>
        )}
        {row?.counter.grants && row.counter.grants.length > 0 ? (
          <ul className="flex flex-col gap-1 text-caption text-muted">
            {row.counter.grants.map((g) => (
              <li key={g.id}>
                +{formatMoney(g.amount)} approved by {g.granted_by}, expires {formatRelative(g.expires_at)}:{' '}
                {g.reason}
              </li>
            ))}
          </ul>
        ) : null}
        <button type="submit" hidden aria-hidden="true" tabIndex={-1} />
      </form>
      {row ? (
        <>
          <OverageDialog
            counter={row.counter}
            label={title}
            open={dialog === 'approve'}
            onClose={() => setDialog(null)}
          />
          <ConfirmDialog
            open={dialog === 'reset'}
            onClose={() => setDialog(null)}
            title="Reset this period?"
            confirmLabel="Reset period"
            objectName={title}
            withReason
            effect="Spent goes back to zero for this period. Requests refused because of this budget are accepted again right away."
            onConfirm={async (reason) => {
              await reset(row.counter, reason ?? 'reset from the console');
              notify('success', 'Budget period reset', row.owner);
            }}
          />
          <ConfirmDialog
            open={dialog === 'adjust'}
            onClose={() => setDialog(null)}
            title="Adjust amount spent"
            tone="primary"
            confirmLabel="Adjust"
            objectName={title}
            withReason
            effect="Add to or subtract from what this budget has spent. Use a negative amount for a credit."
            onConfirm={async (reason) => {
              const trimmed = adjustBy.trim();
              if (!/^-?\d+(\.\d{1,6})?$/.test(trimmed)) {
                throw new Error('Enter an amount in dollars, like -25.00 or 10');
              }
              await adjust(row.counter, trimmed, reason ?? 'adjusted from the console');
              notify('success', 'Amount spent adjusted', row.owner);
            }}
          >
            <Field label="Amount (USD)">
              <Input
                mono
                value={adjustBy}
                onChange={(e) => setAdjustBy(e.target.value)}
                placeholder="e.g. -25.00"
              />
            </Field>
          </ConfirmDialog>
          <ConfirmDialog
            open={dialog === 'delete'}
            onClose={() => setDialog(null)}
            title={`Delete budget ${row.name}?`}
            confirmLabel="Delete budget"
            objectName={`${row.owner} · ${row.name}`}
            withReason
            effect="Spend stops being checked against this budget. Its history stays in the audit log."
            onConfirm={async (reason) => {
              await save(row.level, row.owner, { [row.name]: null, [`${row.name}_cap`]: null }, reason);
              notify('success', 'Budget deleted', row.owner);
              onClose();
            }}
          />
        </>
      ) : null}
    </Drawer>
  );
}
