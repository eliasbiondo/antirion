'use client';

import { useEffect, useState } from 'react';

import { Button, IconButton } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { DatePicker } from '@/components/ui/date-picker';
import { Combobox, Field, Input, type SelectOption } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { CancelButton, Drawer } from '@/components/ui/overlay';
import { EmptyState, ErrorState, InlineError, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { ModelMark } from '@/components/ui/marks';
import { plural } from '@/lib/format';
import { notify } from '@/stores/ui';

import type { TierRow } from '@/features/models/tiers';

import { budgetsApi } from '../api';
import { useBudgets } from '../store';
import {
  type OwnTiers,
  ownTiers,
  type TierPriceRow,
  tierRowsOf,
  tierSettingsPatch,
  type UpstreamChoice,
  upstreamChoices,
} from '../tier-prices';
import { useEdited } from '@/lib/hooks';

const DAY = 86_400_000;

/** `[from, to)` of whole UTC days: from the start of `fromDay` to the end of `toDay`. */
export function recostRange(fromDay: string, toDay: string): { from: string; to: string } {
  const from = new Date(`${fromDay}T00:00:00Z`);
  const to = new Date(new Date(`${toDay}T00:00:00Z`).getTime() + DAY);
  const iso = (d: Date) => d.toISOString().replace(/\.\d{3}Z$/, 'Z');
  return { from: iso(from), to: iso(to) };
}

/**
 * Re-cost spend: prices every request admitted in the chosen days again at today's
 * prices, appending one correction per request whose cost changed; budgets of the period in
 * progress take the difference. Gateway-wide, so it needs to adjust budgets at the gateway.
 */
export function RecostDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const load = useBudgets((s) => s.load);
  const today = new Date().toISOString().slice(0, 10);
  const [fromDay, setFromDay] = useState(() => new Date(Date.now() - 7 * DAY).toISOString().slice(0, 10));
  const [toDay, setToDay] = useState(today);
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title="Re-cost spend?"
      tone="primary"
      confirmLabel="Re-cost spend"
      withReason={false}
      effect={
        <>
          Every request in these days is priced again at today&apos;s prices, service tiers and batch
          discounts included. A request whose cost changed gets a correction; budgets of the current period
          take the difference. Running it again with the same prices changes nothing.
        </>
      }
      onConfirm={async () => {
        if (fromDay > toDay) throw new Error('The first day comes before the last.');
        const { from, to } = recostRange(fromDay, toDay);
        const lines = await budgetsApi.recost(from, to);
        notify(
          'success',
          'Spend re-costed',
          lines === 0 ? 'No request changed cost.' : `${plural(lines, 'correction')} added.`,
        );
        await load({ quiet: true });
      }}
    >
      <div className="grid grid-cols-2 gap-3 max-sm:grid-cols-1">
        <Field label="From">
          <DatePicker aria-label="First day" value={fromDay} onChange={setFromDay} max={today} />
        </Field>
        <Field label="To">
          <DatePicker aria-label="Last day" value={toDay} onChange={setToDay} max={today} />
        </Field>
      </div>
    </ConfirmDialog>
  );
}

/**
 * Prices that depend on the request: multipliers of an upstream model's token rates by the
 * service tier the provider served (`flex`, `priority`) and its batch discount, on the list prices
 * of the gateway-wide price settings (the entries the Models price grid edits); the tiers deployments
 * set themselves are listed read-only.
 */
export function ServiceTierDrawer({ open, onClose }: { open: boolean; onClose: () => void }) {
  const canWrite = useCan('patchPricingSettings');
  const [rows, setRows] = useState<TierPriceRow[] | null>(null);
  const [before, setBefore] = useState<Record<string, TierRow[]>>({});
  const [choices, setChoices] = useState<UpstreamChoice[]>([]);
  const [etag, setEtag] = useState<string | undefined>();
  const [deployed, setDeployed] = useState<OwnTiers[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [rowError, setRowError] = useState<string | null>(null);
  const edited = useEdited(rows, rows === null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    Promise.all([budgetsApi.pricing(), budgetsApi.deploymentPrices(), budgetsApi.deployments()])
      .then(([settings, prices, deployments]) => {
        if (cancelled) return;
        const read = tierRowsOf(settings.spec.models);
        setRows(read.rows);
        setBefore(read.before);
        setChoices(upstreamChoices(prices));
        setEtag(settings.metadata.etag);
        setDeployed(ownTiers(deployments));
      })
      .catch((e: unknown) => !cancelled && setError(e));
    return () => {
      cancelled = true;
    };
  }, [open]);

  const set = (i: number, patch: Partial<TierPriceRow>) =>
    setRows((list) => (list ?? []).map((r, j) => (j === i ? { ...r, ...patch } : r)));

  const options = (current: string): SelectOption[] => {
    const known = choices.map((c) => ({
      value: c.key,
      label: c.key,
      description: `${c.deployments.length === 1 ? 'Deployment' : 'Deployments'} ${c.deployments.join(', ')}`,
      icon: <ModelMark model={c.upstream} size="md" />,
    }));
    return current && !choices.some((c) => c.key === current)
      ? [{ value: current, label: current, description: 'No deployment serves it' }, ...known]
      : known;
  };

  async function save() {
    if (!rows) return;
    const result = tierSettingsPatch(rows, before);
    if ('error' in result) {
      setRowError(result.error);
      return;
    }
    setRowError(null);
    setBusy(true);
    setSaveError(null);
    try {
      await budgetsApi.patchPricing(result.patch, etag);
      notify('success', 'Prices saved', 'New requests are priced with these multipliers.');
      onClose();
    } catch (e) {
      setSaveError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Drawer
      dirty={edited}
      open={open}
      onClose={onClose}
      title="Prices by service tier"
      subtitle="Multipliers of an upstream model's token prices by the tier the provider served, and its batch discount"
      width="lg"
      footer={
        <>
          <CancelButton />
          <Button
            variant="primary"
            busy={busy}
            disabled={!rows || !canWrite}
            disabledReason={canWrite ? undefined : "You don't have permission to change prices."}
            onClick={() => void save()}
          >
            Save prices
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-5">
        {error ? <ErrorState error={error} subject="Price settings" size="sm" /> : null}
        <InlineError error={saveError} />
        {rowError ? <p className="text-ui text-error">{rowError}</p> : null}
        {!rows && !error ? <LoadingRows rows={3} /> : null}
        {rows ? (
          <section className="flex flex-col gap-3">
            <p className="text-ui text-muted">
              Tiers are set on the list price of an upstream model, so every deployment of it uses them; the
              Models price grid edits the same prices. Write each tier as name=multiplier, such as flex=0.5,
              priority=2. A request served in a tier the list doesn&apos;t name is priced at the base rates.
              Re-cost spend to apply a change to requests already made.
            </p>
            {rows.length === 0 ? <EmptyState size="sm" title="No upstream model has tier prices" /> : null}
            {rows.map((r, i) => (
              <div
                key={i}
                role="group"
                aria-label={r.key || 'New upstream model'}
                className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)_96px_auto] items-end gap-3 max-sm:grid-cols-1"
              >
                <Field label="Upstream model">
                  <Combobox
                    value={r.key}
                    onChange={(v) => set(i, { key: v })}
                    placeholder="Select an upstream model"
                    options={options(r.key)}
                  />
                </Field>
                <Field label="Tier multipliers">
                  <Input mono value={r.tiers} onChange={(e) => set(i, { tiers: e.target.value })} />
                </Field>
                <Field label="Batch" optional>
                  <Input
                    mono
                    value={r.batch}
                    onChange={(e) => set(i, { batch: e.target.value })}
                    placeholder="1"
                  />
                </Field>
                <IconButton
                  icon="trash"
                  label={`Remove ${r.key || 'this row'}`}
                  onClick={() => setRows((list) => (list ?? []).filter((_, j) => j !== i))}
                />
              </div>
            ))}
            <Button
              variant="ghost"
              icon="plus"
              className="w-fit"
              onClick={() =>
                setRows((list) => [...(list ?? []), { key: '', tiers: 'flex=0.5, priority=2', batch: '' }])
              }
            >
              Add an upstream model
            </Button>
          </section>
        ) : null}
        {deployed.length > 0 ? (
          <section className="flex flex-col gap-2">
            <h3 className="text-body font-semibold text-fg">Set on deployments</h3>
            <p className="text-caption text-muted">
              A deployment&apos;s own tiers win over the ones above. Change them on the deployment.
            </p>
            <Table label="Tier prices set on deployments">
              <thead>
                <tr>
                  <Th>Deployment</Th>
                  <Th>Tier multipliers</Th>
                  <Th>Batch</Th>
                </tr>
              </thead>
              <tbody>
                {deployed.map((d) => (
                  <Tr key={d.deployment}>
                    <Td mono>{d.deployment}</Td>
                    <Td mono>{d.tiers || '—'}</Td>
                    <Td mono>{d.batch || '—'}</Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          </section>
        ) : null}
      </div>
    </Drawer>
  );
}
