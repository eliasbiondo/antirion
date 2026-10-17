'use client';

import {
  cloneElement,
  forwardRef,
  isValidElement,
  useId,
  useRef,
  type InputHTMLAttributes,
  type ReactElement,
  type ReactNode,
  type TextareaHTMLAttributes,
} from 'react';

import { cn } from '@/lib/cn';

import { Button, type ButtonProps } from './button';
import { Icon, type IconName } from './icon';

const CONTROL =
  'w-full rounded-control border border-line bg-canvas px-2.5 text-ui text-fg placeholder:text-dim ' +
  'hover:border-line-strong focus:border-accent focus:outline-none focus-visible:outline-none ' +
  'focus:ring-2 focus:ring-accent-dim disabled:cursor-not-allowed disabled:opacity-60 ' +
  'aria-[invalid=true]:border-error';

interface ControlA11y {
  id?: string;
  'aria-describedby'?: string;
  'aria-invalid'?: boolean;
  'aria-required'?: boolean;
}

export interface FieldProps {
  label: ReactNode;
  hint?: ReactNode;
  error?: string | undefined;
  required?: boolean;
  /** Marks the field "(optional)" after its label; use it on every field a form does not need. */
  optional?: boolean;
  /** Text shown on the label row's right, such as a counter or a link. */
  aside?: ReactNode;
  className?: string;
  /** For a control wrapped in other elements: the id of the input inside, which must carry it. */
  controlId?: string;
  children: ReactElement<ControlA11y>;
}

/** A label, the control, a hint and an error, wired with `id` and `aria-describedby`. */
export function Field({
  label,
  hint,
  error,
  required,
  optional,
  aside,
  className,
  controlId: wrappedId,
  children,
}: FieldProps) {
  const id = useId();
  const controlId = wrappedId ?? children.props.id ?? `f${id}`;
  const hintId = hint ? `${controlId}-hint` : undefined;
  const errorId = error ? `${controlId}-error` : undefined;
  const describedBy = [hintId, errorId].filter(Boolean).join(' ') || undefined;
  const control = wrappedId
    ? children
    : isValidElement(children)
      ? cloneElement(children, {
          id: controlId,
          'aria-describedby': describedBy,
          'aria-invalid': error ? true : undefined,
          'aria-required': required || undefined,
        })
      : children;
  return (
    <div className={cn('flex flex-col gap-1.5', className)}>
      <div className="flex items-baseline justify-between gap-2">
        <label htmlFor={controlId} className="text-ui font-medium text-fg">
          {label}
          {required ? (
            <span className="text-error" aria-hidden="true">
              {' '}
              *
            </span>
          ) : null}
          {optional && !required ? <span className="font-normal text-dim"> (optional)</span> : null}
        </label>
        {aside}
      </div>
      {control}
      {hint && !error ? (
        <p id={hintId} className="text-caption text-muted">
          {hint}
        </p>
      ) : null}
      {error ? (
        <p id={errorId} role="alert" className="flex items-start gap-1.5 text-caption text-error">
          <Icon name="alertCircle" size={14} className="mt-px shrink-0" />
          {error}
        </p>
      ) : null}
    </div>
  );
}

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  icon?: IconName;
  mono?: boolean;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { icon, mono, className, ...rest },
  ref,
) {
  if (!icon) return <input ref={ref} className={cn(CONTROL, 'h-8', mono && 'mono', className)} {...rest} />;
  return (
    <div className="relative">
      <span className="pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-dim">
        <Icon name={icon} size={14} />
      </span>
      <input ref={ref} className={cn(CONTROL, 'h-8 pl-8', mono && 'mono', className)} {...rest} />
    </div>
  );
});

export interface NumberInputProps extends Omit<
  InputHTMLAttributes<HTMLInputElement>,
  'value' | 'onChange' | 'type' | 'min' | 'max' | 'step'
> {
  /** The text in the field ('' when empty), as a form keeps it. */
  value: string;
  onChange: (value: string) => void;
  min?: number;
  max?: number;
  /** What the arrow keys and the − / + buttons add or take away (Shift and Page Up/Down: ten times). */
  step?: number;
  /** − and + buttons beside the field. */
  steppers?: boolean;
  mono?: boolean;
}

/**
 * A number field without the browser's spinners: a text field with a numeric keyboard and the
 * spin button's keys (arrows, Page Up/Down, Home/End to the limits), and optional − / + buttons.
 */
export const NumberInput = forwardRef<HTMLInputElement, NumberInputProps>(function NumberInput(
  { value, onChange, min, max, step = 1, steppers = false, mono = true, className, disabled, ...rest },
  ref,
) {
  const n = value.trim() === '' ? null : Number(value);
  const current = n !== null && Number.isFinite(n) ? n : null;
  const clamp = (v: number) => Math.min(max ?? Infinity, Math.max(min ?? -Infinity, v));
  const nudge = (by: number) => {
    const base = current ?? min ?? 0;
    onChange(String(clamp(Math.round((base + by) / step) * step)));
  };
  const field = (
    <input
      ref={ref}
      type="text"
      inputMode={step % 1 === 0 && (min ?? 0) >= 0 ? 'numeric' : 'decimal'}
      role="spinbutton"
      aria-valuenow={current ?? undefined}
      aria-valuemin={min}
      aria-valuemax={max}
      autoComplete="off"
      disabled={disabled}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => {
        const big = step * 10;
        const keys: Record<string, () => void> = {
          ArrowUp: () => nudge(e.shiftKey ? big : step),
          ArrowDown: () => nudge(-(e.shiftKey ? big : step)),
          PageUp: () => nudge(big),
          PageDown: () => nudge(-big),
          ...(min !== undefined ? { Home: () => onChange(String(min)) } : {}),
          ...(max !== undefined ? { End: () => onChange(String(max)) } : {}),
        };
        const run = keys[e.key];
        if (run) {
          e.preventDefault();
          run();
        }
      }}
      className={cn(CONTROL, 'h-8', mono && 'mono', steppers && 'rounded-r-none', className)}
      {...rest}
    />
  );
  if (!steppers) return field;
  const name = rest['aria-label'] ?? 'value';
  const stepButton =
    'flex h-8 w-7 shrink-0 items-center justify-center border border-l-0 border-line bg-raised text-muted hover:bg-hover hover:text-fg disabled:cursor-not-allowed disabled:opacity-50';
  return (
    <div className="flex">
      {field}
      <button
        type="button"
        tabIndex={-1}
        aria-label={`Decrease ${name}`}
        disabled={disabled || (min !== undefined && current !== null && current <= min)}
        onClick={() => nudge(-step)}
        className={stepButton}
      >
        <Icon name="minus" size={14} />
      </button>
      <button
        type="button"
        tabIndex={-1}
        aria-label={`Increase ${name}`}
        disabled={disabled || (max !== undefined && current !== null && current >= max)}
        onClick={() => nudge(step)}
        className={cn(stepButton, 'rounded-r-control')}
      >
        <Icon name="plus" size={14} />
      </button>
    </div>
  );
});

export const Textarea = forwardRef<
  HTMLTextAreaElement,
  TextareaHTMLAttributes<HTMLTextAreaElement> & { mono?: boolean }
>(function Textarea({ className, mono, ...rest }, ref) {
  return <textarea ref={ref} className={cn(CONTROL, 'min-h-20 py-2', mono && 'mono', className)} {...rest} />;
});

export { Combobox, MultiSelect, Select, type SelectOption } from './select';

export {
  Checkbox,
  RadioGroup,
  Switch,
  type CheckboxProps,
  type RadioOption,
  type SwitchProps,
} from './choice';

export interface SegmentedOption<T extends string> {
  value: T;
  label: ReactNode;
  count?: number | undefined;
  icon?: IconName;
}

export interface SegmentedProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  options: SegmentedOption<T>[];
  label: string;
  className?: string;
}

/** The mockups' segmented filter (All 12 · Service 7 · User 4), a radio group. */
export function Segmented<T extends string>({
  value,
  onChange,
  options,
  label,
  className,
}: SegmentedProps<T>) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={cn(
        'inline-flex items-stretch overflow-hidden rounded-control border border-line bg-canvas',
        className,
      )}
      onKeyDown={(e) => {
        if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
        e.preventDefault();
        const i = options.findIndex((o) => o.value === value);
        const next = options[(i + (e.key === 'ArrowRight' ? 1 : options.length - 1)) % options.length];
        if (next) onChange(next.value);
      }}
    >
      {options.map((o, i) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={on ? 0 : -1}
            onClick={() => onChange(o.value)}
            className={cn(
              'inline-flex h-7 items-center gap-1.5 px-3 text-ui whitespace-nowrap transition-colors',
              i > 0 && 'border-l border-line',
              on
                ? 'bg-raised font-semibold text-fg shadow-[inset_0_-2px_0_var(--ant-accent)]'
                : 'text-muted hover:bg-hover hover:text-fg',
            )}
          >
            {o.icon ? <Icon name={o.icon} size={14} /> : null}
            {o.label}
            {o.count !== undefined ? (
              <span className="text-caption text-dim tabular-nums">{o.count}</span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}

export interface ChipToggleProps {
  pressed: boolean;
  onChange: (pressed: boolean) => void;
  children: ReactNode;
  tone?: 'success' | 'info' | 'warn' | 'error' | 'muted';
  count?: number | undefined;
}

const DOT: Record<NonNullable<ChipToggleProps['tone']>, string> = {
  success: 'bg-success',
  info: 'bg-info',
  warn: 'bg-warn',
  error: 'bg-error',
  muted: 'bg-dim',
};

/** A filter chip with a coloured dot, toggled on and off (state chips in the mockups). */
export function ChipToggle({ pressed, onChange, children, tone, count }: ChipToggleProps) {
  return (
    <button
      type="button"
      aria-pressed={pressed}
      onClick={() => onChange(!pressed)}
      className={cn(
        'inline-flex h-6 items-center gap-1.5 rounded-full border px-2.5 text-caption transition-colors',
        pressed
          ? 'border-fg bg-fg text-raised'
          : 'border-line bg-canvas text-fg hover:border-line-strong hover:bg-hover',
      )}
    >
      {tone ? <span className={cn('h-1.5 w-1.5 rounded-full', DOT[tone])} aria-hidden="true" /> : null}
      {children}
      {count !== undefined ? <span className="tabular-nums opacity-70">{count}</span> : null}
    </button>
  );
}

export interface FileButtonProps extends Omit<ButtonProps, 'onClick' | 'onChange' | 'type'> {
  /** File types the picker offers, as in `accept` (`image/png,image/jpeg`). */
  accept?: string;
  /** Called with the chosen file; choosing the same file again calls it again. */
  onFile: (file: File) => void;
}

/** A button that opens the system's file picker; the browser's own file field never shows. */
export function FileButton({ accept, onFile, children, ...rest }: FileButtonProps) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <Button {...rest} type="button" onClick={() => input.current?.click()}>
        {children}
      </Button>
      <input
        ref={input}
        type="file"
        accept={accept}
        tabIndex={-1}
        aria-hidden="true"
        className="hidden"
        onChange={(e) => {
          const file = e.target.files?.[0];
          e.target.value = '';
          if (file) onFile(file);
        }}
      />
    </>
  );
}
