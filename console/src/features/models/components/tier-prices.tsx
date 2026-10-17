'use client';

import { useState } from 'react';

import { Button, IconButton } from '@/components/ui/button';
import { Field, Input, Select } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { CancelButton, Dialog } from '@/components/ui/overlay';
import { InlineError } from '@/components/ui/states';
import { isChangeControl } from '@/lib/api/client';
import { notify } from '@/stores/ui';

import { modelsApi } from '../api';
import { useDeploymentMap } from '../hooks';
import { useCatalog } from '../store';
import { priceListKey, type TierRow, tierErrors, tierPatch, tiersOf, tiersText } from '../tiers';
import { useEdited } from '@/lib/hooks';

/**
 * Prices that depend on the request, in the Models price grid: the service-tier multipliers
 * and the batch multiplier of the Model's price, editable on the list price of a deployment's upstream
 * model (the same settings Budgets edits).
 */
export function TierPrices({ leaves }: { leaves: string[] }) {
  const prices = useCatalog((s) => s.prices);
  const canEdit = useCan('patchPricingSettings');
  const [open, setOpen] = useState(false);
  const priced = leaves.filter((l) => prices[l]?.price);
  const shown = priced.find((l) => {
    const t = tiersOf(prices[l]?.price);
    return t.tiers.length > 0 || t.batch !== '';
  });
  if (priced.length === 0) return null;
  const { tiers, batch } = tiersOf(shown ? prices[shown]?.price : null);
  return (
    <div className="mt-3 flex flex-wrap items-baseline gap-x-5 gap-y-1 text-ui">
      <div className="flex gap-1.5">
        <span className="text-muted">Service tiers</span>
        <span className={tiers.length > 0 ? 'mono text-fg' : 'text-muted'}>
          {tiers.length > 0 ? tiersText(tiers) : 'base rates in every tier'}
        </span>
      </div>
      <div className="flex gap-1.5">
        <span className="text-muted">Batches</span>
        <span className={batch ? 'mono text-fg' : 'text-muted'}>{batch ? `×${batch}` : 'base rates'}</span>
      </div>
      {shown && priced.length > 1 ? (
        <span className="text-caption text-muted">
          of <span className="mono">{shown}</span>
        </span>
      ) : null}
      {canEdit ? (
        <Button size="sm" variant="ghost" icon="edit" onClick={() => setOpen(true)}>
          Edit tier prices
        </Button>
      ) : null}
      {open ? (
        <TierPriceDialog leaves={priced} initial={shown ?? priced[0] ?? ''} onClose={() => setOpen(false)} />
      ) : null}
    </div>
  );
}

function TierPriceDialog({
  leaves,
  initial,
  onClose,
}: {
  leaves: string[];
  initial: string;
  onClose: () => void;
}) {
  const prices = useCatalog((s) => s.prices);
  const loadPrices = useCatalog((s) => s.loadPrices);
  const deployments = useDeploymentMap();
  const [deployment, setDeployment] = useState(initial);
  const start = tiersOf(prices[deployment]?.price);
  const [rows, setRows] = useState<TierRow[]>(start.tiers);
  const [batch, setBatch] = useState(start.batch);
  const [before, setBefore] = useState<TierRow[]>(start.tiers);
  const [invalid, setInvalid] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited({ rows, batch }, deployment);
  const key = priceListKey(prices[deployment]);
  const own = tiersOf(deployments.get(deployment)?.spec.price);
  const ownSet = own.tiers.length > 0 || own.batch !== '';

  function pick(d: string) {
    const t = tiersOf(prices[d]?.price);
    setDeployment(d);
    setRows(t.tiers);
    setBatch(t.batch);
    setBefore(t.tiers);
  }

  async function save() {
    const bad = tierErrors(rows, batch);
    setInvalid(bad);
    if (bad || !key) return;
    setBusy(true);
    setError(null);
    try {
      const settings = await modelsApi.pricing();
      await modelsApi.patchPricing({ [key]: tierPatch(before, rows, batch) }, settings.metadata.etag);
      // Every deployment of that upstream model is priced anew: forget their prices, then read them again.
      useCatalog.setState((st) => ({
        prices: Object.fromEntries(Object.entries(st.prices).filter(([, p]) => priceListKey(p) !== key)),
      }));
      await loadPrices(leaves);
      notify('success', 'Tier prices saved', `${key}: new requests are priced with them.`);
      onClose();
    } catch (e) {
      if (isChangeControl(e)) onClose();
      else setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      dirty={edited}
      open
      onClose={onClose}
      title="Prices by service tier"
      description="Multipliers of the token prices, by the tier the provider served the request in, and for Batches API usage."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} disabled={!key} onClick={() => void save()}>
            Save tier prices
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <InlineError error={error} />
        {leaves.length > 1 ? (
          <Field label="Deployment">
            <Select
              value={deployment}
              onChange={pick}
              options={leaves.map((d) => ({ value: d, label: d }))}
            />
          </Field>
        ) : null}
        <p className="text-ui text-muted">
          {key ? (
            <>
              Set on the list price of <span className="mono text-fg">{key}</span>, so every deployment of
              that upstream model uses them. A tier the list doesn’t name is priced at the base rates.
            </>
          ) : (
            'This deployment has no list price to set tiers on.'
          )}
        </p>
        {ownSet ? (
          <p className="text-caption text-warn">
            {deployment} sets {own.tiers.length > 0 ? tiersText(own.tiers) : 'a batch multiplier'} itself,
            which wins over these. Change that on the deployment.
          </p>
        ) : null}
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-2 text-ui font-medium text-fg">Service tiers</legend>
          {rows.map((r, i) => (
            <div key={i} className="grid grid-cols-[minmax(0,1fr)_120px_auto] items-center gap-2">
              <Input
                mono
                aria-label={`Tier ${i + 1}`}
                placeholder="flex"
                value={r.tier}
                onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, tier: e.target.value } : x)))}
              />
              <Input
                mono
                inputMode="decimal"
                aria-label={`Multiplier of tier ${i + 1}`}
                placeholder="0.5"
                value={r.multiplier}
                onChange={(e) =>
                  setRows(rows.map((x, j) => (j === i ? { ...x, multiplier: e.target.value } : x)))
                }
              />
              <IconButton
                icon="x"
                label={`Remove tier ${i + 1}`}
                onClick={() => setRows(rows.filter((_, j) => j !== i))}
              />
            </div>
          ))}
          <Button
            size="sm"
            icon="plus"
            className="self-start"
            onClick={() =>
              setRows([...rows, { tier: rows.length === 0 ? 'flex' : 'priority', multiplier: '' }])
            }
          >
            Add tier
          </Button>
        </fieldset>
        <Field
          label="Batches"
          optional
          error={invalid ?? undefined}
          hint="Multiplies the token prices of Batches API usage, after the tier’s; empty for none."
        >
          <Input
            mono
            inputMode="decimal"
            className="w-32"
            placeholder="0.5"
            value={batch}
            onChange={(e) => setBatch(e.target.value)}
          />
        </Field>
        <p className="text-caption text-muted">
          Requests already made keep their cost; re-cost spend in Budgets to apply a change to them.
        </p>
      </div>
    </Dialog>
  );
}
