'use client';

import type { ReactNode } from 'react';

import { Button, IconButton } from '@/components/ui/button';
import { Input, MultiSelect } from '@/components/ui/form';
import { cn } from '@/lib/cn';

import type { HeaderRow } from '../fields';

/** A titled block of a drawer, as in the request drawer. */
export function Section({
  title,
  aside,
  children,
}: {
  title: ReactNode;
  aside?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section>
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <h3 className="text-ui font-semibold text-fg">{title}</h3>
        {aside ? <span className="text-caption text-muted">{aside}</span> : null}
      </div>
      {children}
    </section>
  );
}

/** Labels left in sans, values right, mono where they are codes (the request drawer's list). */
export function Facts({ items }: { items: { label: string; value: ReactNode; mono?: boolean }[] }) {
  return (
    <dl className="grid grid-cols-[minmax(120px,170px)_minmax(0,1fr)] gap-x-4 rounded-control border border-line max-sm:grid-cols-1">
      {items.map((item, i) => (
        <div key={item.label} className={cn('contents', i > 0 && '[&>*]:border-t [&>*]:border-line')}>
          <dt className="px-3 py-2 text-caption text-muted max-sm:pb-0">{item.label}</dt>
          <dd
            className={cn(
              'min-w-0 px-3 py-2 text-ui break-words text-fg max-sm:border-t-0 max-sm:pt-0.5',
              item.mono !== false && 'mono tabular-nums',
            )}
          >
            {item.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** A group of form fields under a small heading, for long editors. */
export function FormGroup({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: ReactNode;
  children: ReactNode;
}) {
  return (
    <fieldset className="flex min-w-0 flex-col gap-4 border-t border-line pt-4">
      <legend className="float-left mb-1 w-full text-ui font-semibold text-fg">{title}</legend>
      {hint ? <p className="-mt-3 text-caption text-muted">{hint}</p> : null}
      {children}
    </fieldset>
  );
}

/**
 * A header map whose values are written, never read back: a stored value shows "Stored, hidden"
 * and is kept unless a new one is typed.
 */
export function HeaderRowsEditor({
  label,
  rows,
  onChange,
  error,
  hint,
}: {
  label: string;
  rows: HeaderRow[];
  onChange: (rows: HeaderRow[]) => void;
  error?: string | undefined;
  hint?: ReactNode;
}) {
  const update = (i: number, patch: Partial<HeaderRow>) =>
    onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div role="group" aria-label={label} className="flex flex-col gap-2">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-ui font-medium text-fg">
          {label} <span className="font-normal text-muted">(optional)</span>
        </span>
        <Button
          size="sm"
          variant="ghost"
          icon="plus"
          onClick={() => onChange([...rows, { name: '', value: '', stored: false }])}
        >
          Add header
        </Button>
      </div>
      {rows.length === 0 ? <p className="text-caption text-muted">None.</p> : null}
      {rows.map((row, i) => (
        <div key={i} className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)_auto] items-center gap-2">
          <Input
            mono
            aria-label={`Header ${i + 1} name`}
            placeholder="authorization"
            value={row.name}
            onChange={(e) => update(i, { name: e.target.value })}
          />
          <Input
            mono
            type="password"
            autoComplete="off"
            aria-label={`Header ${i + 1} value`}
            placeholder={row.stored ? 'Stored, hidden. Type to replace' : 'Value or ${env:NAME}'}
            value={row.value}
            onChange={(e) => update(i, { value: e.target.value })}
          />
          <IconButton
            icon="trash"
            label={`Remove header ${row.name || i + 1}`}
            onClick={() => onChange(rows.filter((_, j) => j !== i))}
          />
        </div>
      ))}
      {error ? (
        <p role="alert" className="text-caption text-error">
          {error}
        </p>
      ) : hint ? (
        <p className="text-caption text-muted">{hint}</p>
      ) : null}
    </div>
  );
}

/** A list of free values (globs, names, URLs) as chips: type one and press Enter. */
export function ListInput({
  value,
  onChange,
  noun,
  placeholder,
  options = [],
  ...aria
}: {
  value: string[];
  onChange: (value: string[]) => void;
  noun: string;
  placeholder?: string;
  options?: string[];
  'aria-label'?: string;
  'aria-invalid'?: boolean;
  'aria-describedby'?: string;
  id?: string;
}) {
  const all = [...new Set([...options, ...value])];
  return (
    <MultiSelect
      mono
      creatable
      noun={noun}
      placeholder={placeholder ?? 'Type and press Enter'}
      value={value}
      onChange={onChange}
      options={all.map((v) => ({ value: v, label: v }))}
      {...aria}
    />
  );
}

const CALLOUT_TONE = {
  info: 'border-[color-mix(in_oklab,var(--ant-info)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-info)_8%,transparent)]',
  warn: 'border-[color-mix(in_oklab,var(--ant-warn)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-warn)_8%,transparent)]',
  error:
    'border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-error)_8%,transparent)]',
} as const;

/** A short note in a drawer: what a state means and what to do about it. */
export function Callout({
  tone,
  title,
  children,
}: {
  tone: keyof typeof CALLOUT_TONE;
  title: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div role="status" className={cn('rounded-control border px-3 py-2.5', CALLOUT_TONE[tone])}>
      <p className="text-ui font-semibold text-fg">{title}</p>
      {children ? <div className="mt-0.5 text-ui text-muted">{children}</div> : null}
    </div>
  );
}
