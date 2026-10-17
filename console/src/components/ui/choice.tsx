'use client';

import { useId, useRef, type KeyboardEvent, type ReactNode } from 'react';

import { cn } from '@/lib/cn';

import { Icon } from './icon';
import { Tooltip } from './tooltip';

const BOX_FOCUS = 'focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent';

export interface CheckboxProps {
  checked: boolean | 'mixed';
  onChange: (checked: boolean) => void;
  /** The visible label; without one, give an `aria-label`. */
  label?: ReactNode;
  /** A muted line under the label, read as the box's description. */
  description?: ReactNode;
  disabled?: boolean;
  id?: string;
  'aria-label'?: string;
  'aria-describedby'?: string;
  className?: string;
}

/**
 * A checkbox drawn by the console (a box, a tick, a dash for "some"), a `role="checkbox"` button:
 * Space toggles, the label toggles it too, and a screen reader hears its state and description.
 */
export function Checkbox({
  checked,
  onChange,
  label,
  description,
  disabled,
  id,
  className,
  ...aria
}: CheckboxProps) {
  const auto = useId();
  const boxId = id ?? `cb${auto}`;
  const descId = description ? `${boxId}-desc` : undefined;
  const on = checked === true;
  const mixed = checked === 'mixed';
  const box = (
    <button
      id={boxId}
      type="button"
      role="checkbox"
      aria-checked={mixed ? 'mixed' : on}
      aria-label={aria['aria-label']}
      aria-describedby={[descId, aria['aria-describedby']].filter(Boolean).join(' ') || undefined}
      disabled={disabled}
      onClick={() => onChange(!on)}
      onKeyDown={(e) => {
        // A checkbox toggles on Space only; Enter would submit the form.
        if (e.key === 'Enter') e.preventDefault();
      }}
      className={cn(
        'mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-xs border transition-colors',
        on || mixed
          ? 'border-accent bg-accent text-accent-fg'
          : 'border-line-strong bg-canvas hover:border-[var(--ant-fg-dim)]',
        'disabled:cursor-not-allowed disabled:opacity-50',
        BOX_FOCUS,
      )}
    >
      {on ? <Icon name="check" size={12} /> : null}
      {mixed ? <Icon name="minus" size={12} /> : null}
    </button>
  );
  if (!label && !description) return box;
  return (
    <div className={cn('flex items-start gap-2', className)}>
      {box}
      <div className="min-w-0">
        {label ? (
          <label
            htmlFor={boxId}
            className={cn(
              'block text-ui text-fg',
              disabled ? 'cursor-not-allowed opacity-60' : 'cursor-pointer',
            )}
          >
            {label}
          </label>
        ) : null}
        {description ? (
          <p id={descId} className="text-caption text-muted">
            {description}
          </p>
        ) : null}
      </div>
    </div>
  );
}

export interface RadioOption<T extends string> {
  value: T;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
}

export interface RadioGroupProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  options: RadioOption<T>[];
  /** The group's accessible name (or `aria-labelledby`). */
  label?: string;
  'aria-labelledby'?: string;
  /** `list`: a circle and a label per row. `cards`: bordered tiles, for a few choices with a line each. */
  variant?: 'list' | 'cards';
  className?: string;
}

/**
 * Radio buttons drawn by the console, in one tab stop: the arrows move the choice (and wrap),
 * Space chooses the focused one, and the checked one is where Tab lands.
 */
export function RadioGroup<T extends string>({
  value,
  onChange,
  options,
  label,
  variant = 'list',
  className,
  ...aria
}: RadioGroupProps<T>) {
  const auto = useId();
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const checkedIndex = options.findIndex((o) => o.value === value);
  const tabStop = checkedIndex >= 0 ? checkedIndex : options.findIndex((o) => !o.disabled);

  const move = (from: number, step: 1 | -1) => {
    for (let n = 1; n <= options.length; n++) {
      const i = (from + step * n + options.length * n) % options.length;
      const o = options[i];
      if (o && !o.disabled) {
        onChange(o.value);
        refs.current[i]?.focus();
        return;
      }
    }
  };

  const onKey = (e: KeyboardEvent<HTMLButtonElement>, i: number) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowRight') {
      e.preventDefault();
      move(i, 1);
    } else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') {
      e.preventDefault();
      move(i, -1);
    } else if (e.key === 'Enter') e.preventDefault();
  };

  return (
    <div
      role="radiogroup"
      aria-label={label}
      aria-labelledby={aria['aria-labelledby']}
      className={cn(variant === 'cards' ? 'grid gap-2 sm:grid-cols-2' : 'flex flex-col gap-2', className)}
    >
      {options.map((o, i) => {
        const on = o.value === value;
        const id = `r${auto}-${i}`;
        const descId = o.description ? `${id}-desc` : undefined;
        const dot = (
          <span
            aria-hidden="true"
            className={cn(
              'mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center rounded-full border transition-colors',
              on ? 'border-accent' : 'border-line-strong bg-canvas',
            )}
          >
            {on ? <span className="h-2 w-2 rounded-full bg-accent" /> : null}
          </span>
        );
        if (variant === 'cards')
          return (
            <button
              key={o.value}
              ref={(el) => {
                refs.current[i] = el;
              }}
              id={id}
              type="button"
              role="radio"
              aria-checked={on}
              aria-labelledby={`${id}-label`}
              aria-describedby={descId}
              tabIndex={i === tabStop ? 0 : -1}
              disabled={o.disabled}
              onClick={() => onChange(o.value)}
              onKeyDown={(e) => onKey(e, i)}
              className={cn(
                'flex items-start gap-2.5 rounded-sm border px-3 py-2.5 text-left transition-colors',
                on ? 'border-accent bg-accent-dim' : 'border-line hover:bg-hover',
                'disabled:cursor-not-allowed disabled:opacity-50',
                BOX_FOCUS,
              )}
            >
              {dot}
              <span className="min-w-0">
                <span id={`${id}-label`} className="block text-body font-semibold text-fg">
                  {o.label}
                </span>
                {o.description ? (
                  <span id={descId} className="block text-caption text-muted">
                    {o.description}
                  </span>
                ) : null}
              </span>
            </button>
          );
        return (
          <div key={o.value} className="flex items-start gap-2">
            <button
              ref={(el) => {
                refs.current[i] = el;
              }}
              id={id}
              type="button"
              role="radio"
              aria-checked={on}
              aria-describedby={descId}
              tabIndex={i === tabStop ? 0 : -1}
              disabled={o.disabled}
              onClick={() => onChange(o.value)}
              onKeyDown={(e) => onKey(e, i)}
              className={cn('rounded-full disabled:cursor-not-allowed disabled:opacity-50', BOX_FOCUS)}
            >
              {dot}
            </button>
            <div className="min-w-0">
              <label htmlFor={id} className="block cursor-pointer text-ui text-fg">
                {o.label}
              </label>
              {o.description ? (
                <p id={descId} className="text-caption text-muted">
                  {o.description}
                </p>
              ) : null}
            </div>
          </div>
        );
      })}
    </div>
  );
}

export interface SwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: string;
  disabled?: boolean;
  disabledReason?: string | undefined;
  /** Hides the visible label; it stays the accessible name. */
  hideLabel?: boolean;
}

/** An on/off switch (`role="switch"`): Space or a click flips it; a disabled one says why. */
export function Switch({ checked, onChange, label, disabled, disabledReason, hideLabel }: SwitchProps) {
  const reasonId = useId();
  const reason = disabled && disabledReason ? disabledReason : undefined;
  const button = (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={hideLabel ? label : undefined}
      aria-describedby={reason ? reasonId : undefined}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') e.preventDefault();
      }}
      className={cn(
        'group inline-flex items-center gap-2 rounded-full disabled:cursor-not-allowed disabled:opacity-50',
        BOX_FOCUS,
      )}
    >
      <span
        className={cn(
          'relative inline-block h-4 w-7 rounded-full border transition-colors',
          checked ? 'border-success bg-success' : 'border-line-strong bg-sunken',
        )}
      >
        <span
          className={cn(
            'absolute top-0.5 h-2.5 w-2.5 rounded-full bg-white shadow transition-[left]',
            checked ? 'left-3.5' : 'left-0.5',
          )}
        />
      </span>
      {hideLabel ? null : <span className="text-ui text-fg">{label}</span>}
    </button>
  );
  if (disabledReason === undefined) return button;
  return (
    <Tooltip content={reason} describe={false}>
      <span className="inline-flex [&>button:disabled]:pointer-events-none">
        {button}
        {reason ? (
          <span id={reasonId} hidden>
            {reason}
          </span>
        ) : null}
      </span>
    </Tooltip>
  );
}
