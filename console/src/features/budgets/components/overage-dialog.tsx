'use client';

import { useState } from 'react';

import { ConfirmDialog } from '@/components/ui/confirm';
import { DatePicker } from '@/components/ui/date-picker';
import { Field, Input } from '@/components/ui/form';
import type { BudgetCounter } from '@/lib/api/types';
import { formatMoney } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { formatDateTime } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { useBudgets } from '../store';

const DAY = 86_400_000;

/**
 * When a grant ending on `day` (end of that UTC day) expires: never later than the period's end, which
 * a budget with a period end also bounds it to; a budget without one (lifetime, rolling) is unbounded.
 */
export function grantExpiry(day: string, periodEnd: string | null | undefined): string {
  const end = new Date(`${day}T23:59:59Z`).getTime();
  const bound = periodEnd ? new Date(periodEnd).getTime() : Infinity;
  return new Date(Math.min(end, bound)).toISOString().replace(/\.\d{3}Z$/, 'Z');
}

/**
 * "Approve overage": lets a budget go over by an amount until a time no later than its period's
 * end (a grant). The reason is required; it goes to the audit log and the budget's events.
 */
export function OverageDialog({
  counter,
  label,
  open,
  onClose,
}: {
  counter: BudgetCounter;
  /** What the budget is called in the dialog ("support-ai · monthly"). */
  label: string;
  open: boolean;
  onClose: () => void;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const grant = useBudgets((s) => s.grant);
  const periodEnd = counter.resets_at;
  const now = useNow(60_000);
  const defaultDay = (periodEnd ? new Date(new Date(periodEnd).getTime() - 1) : new Date(now + 7 * DAY))
    .toISOString()
    .slice(0, 10);
  const over = Math.max(0, Number(counter.spent) - Number(counter.amount));
  const [amount, setAmount] = useState(() => (over > 0 ? (Math.ceil(over * 100) / 100).toFixed(2) : ''));
  const [day, setDay] = useState(defaultDay);
  const [reason, setReason] = useState('');
  const today = new Date(now).toISOString().slice(0, 10);
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title="Approve overage?"
      tone="primary"
      confirmLabel="Approve overage"
      objectName={label}
      withReason={false}
      effect={
        <>
          Requests this budget refuses are accepted again until the extra amount is spent or the approval
          expires.{' '}
          {periodEnd ? (
            <span className="text-muted">
              It ends with the period, {formatDateTime(periodEnd, timeZone)}, at the latest.
            </span>
          ) : null}
        </>
      }
      onConfirm={async () => {
        const trimmed = amount.trim();
        if (!/^\d+(\.\d{1,6})?$/.test(trimmed) || Number(trimmed) <= 0)
          throw new Error('Enter the extra amount in dollars, like 50');
        if (!reason.trim()) throw new Error('Say why going over is approved; it goes to the audit log.');
        const expires = grantExpiry(day, periodEnd);
        if (new Date(expires).getTime() <= Date.now()) throw new Error('Choose a day from today on');
        await grant(counter, trimmed, expires, reason.trim());
        notify('success', 'Overage approved', `${label}: ${formatMoney(trimmed)} more`);
      }}
    >
      <div className="grid grid-cols-2 gap-3 max-sm:grid-cols-1">
        <Field label="Extra amount (USD)" required>
          <Input
            mono
            inputMode="decimal"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            placeholder="e.g. 50"
          />
        </Field>
        <Field label="Until" hint={periodEnd ? 'At most the end of the period.' : undefined}>
          <DatePicker
            aria-label="Approved until"
            value={day}
            onChange={setDay}
            min={today}
            max={periodEnd ? defaultDay : undefined}
          />
        </Field>
      </div>
      <Field label="Reason" required>
        <Input
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          placeholder="e.g. Launch week traffic"
          maxLength={500}
        />
      </Field>
    </ConfirmDialog>
  );
}
