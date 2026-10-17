'use client';

import { Button, IconButton } from '@/components/ui/button';
import { Field, Input, NumberInput, Select } from '@/components/ui/form';

import { type PriorityMode, remainderText, type ShareRow } from '../priority';

/** A limit's priority shares by key class, and whether a class may borrow what others leave unused. */
export function PriorityShares({
  rows,
  mode,
  onRows,
  onMode,
  errors,
}: {
  rows: ShareRow[];
  mode: PriorityMode;
  onRows: (rows: ShareRow[]) => void;
  onMode: (mode: PriorityMode) => void;
  errors: Record<string, string>;
}) {
  const update = (i: number, patch: Partial<ShareRow>) =>
    onRows(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <fieldset className="flex flex-col gap-2 border-t border-line pt-4">
      <legend className="sr-only">Priority shares</legend>
      <p className="text-ui font-medium text-fg">Priority shares</p>
      <p className="-mt-1 text-caption text-muted">
        Keep part of the limit for keys of a priority class, set on each API key (for example prod and batch).
        Each class has its own counter; the rest of the limit is shared by keys of any other class.
      </p>
      {rows.map((r, i) => (
        <div key={i} className="flex flex-col gap-1">
          <div className="grid grid-cols-[minmax(0,1fr)_110px_auto_auto] items-center gap-2">
            <Input
              mono
              aria-label={`Class ${i + 1}`}
              placeholder="prod"
              value={r.cls}
              onChange={(e) => update(i, { cls: e.target.value })}
            />
            <NumberInput
              aria-label={`Share of class ${i + 1}, percent`}
              min={0}
              max={100}
              value={r.percent}
              onChange={(percent) => update(i, { percent })}
            />
            <span className="text-ui text-muted">%</span>
            <IconButton
              icon="x"
              label={`Remove class ${i + 1}`}
              onClick={() => onRows(rows.filter((_, j) => j !== i))}
            />
          </div>
          {errors[`shares.${i}`] ? <p className="text-caption text-error">{errors[`shares.${i}`]}</p> : null}
        </div>
      ))}
      {errors.shares ? <p className="text-caption text-error">{errors.shares}</p> : null}
      <Button
        size="sm"
        icon="plus"
        className="self-start"
        onClick={() => onRows([...rows, { cls: '', percent: '' }])}
      >
        Add class
      </Button>
      {rows.length > 0 ? (
        <>
          <p className="text-caption text-muted">{remainderText(rows)}.</p>
          <Field
            label="When a class has used its share"
            hint="Borrowing never takes capacity another class has not used of its own share."
          >
            <Select
              value={mode}
              onChange={(v) => onMode(v === 'borrow' ? 'borrow' : 'strict')}
              options={[
                { value: 'strict', label: 'Its requests are refused (strict)' },
                { value: 'borrow', label: 'It may use the unclaimed rest (borrow)' },
              ]}
            />
          </Field>
        </>
      ) : null}
    </fieldset>
  );
}
