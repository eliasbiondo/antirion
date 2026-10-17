'use client';

import { forwardRef, useId, type ButtonHTMLAttributes, type ReactNode } from 'react';

import { cn } from '@/lib/cn';

import { Icon, type IconName } from './icon';
import { Tooltip } from './tooltip';

/**
 * Button weights, loudest first:
 * - `primary`: filled accent. At most one per page header, dialog or drawer footer: the action the
 *   view is for ("Create key", "Save changes").
 * - `secondary` (the default): outlined. Every other action.
 * - `ghost`: text only. Cancel, toolbar and inline actions.
 * - `danger`: a quiet red outline for a destructive action on a page (it opens a confirmation).
 *   Never the loudest button of a view.
 * - `danger-solid`: filled red, only as the confirm button of a confirmation dialog.
 * - `inverse`: the sign-in pages' foreground-filled button.
 */
export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger' | 'danger-solid' | 'inverse';
export type ButtonSize = 'sm' | 'md';

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'title'> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: IconName;
  /** An icon after the label (a chevron for a menu, an arrow for a link-like action). */
  iconAfter?: IconName;
  busy?: boolean;
  /** Shown as the tooltip and to assistive technology when the button is disabled. */
  disabledReason?: string | undefined;
  /** A short hint on hover and keyboard focus (the console's tooltip, never `title`). */
  tooltip?: string | undefined;
  children?: ReactNode;
}

const VARIANTS: Record<ButtonVariant, string> = {
  primary:
    'bg-accent text-accent-fg border-accent font-semibold shadow-[inset_0_1px_0_rgb(255_255_255/0.12)] hover:brightness-110',
  secondary: 'bg-raised text-fg border-line-strong hover:bg-hover hover:border-[var(--ant-fg-dim)]',
  ghost: 'bg-transparent text-muted border-transparent hover:bg-hover hover:text-fg',
  danger:
    'bg-raised text-error border-[color-mix(in_oklab,var(--ant-error)_55%,var(--ant-border))] hover:bg-[color-mix(in_oklab,var(--ant-error)_10%,transparent)]',
  'danger-solid': 'bg-error text-white border-error font-semibold hover:brightness-110',
  inverse: 'bg-fg text-raised border-fg hover:opacity-90',
};

const SIZES: Record<ButtonSize, string> = {
  sm: 'h-7 px-2.5 text-caption gap-1.5',
  md: 'h-8 px-3 text-ui gap-2',
};

const ICON_SIZE: Record<ButtonSize, number> = { sm: 14, md: 16 };

const BASE =
  'inline-flex shrink-0 items-center justify-center rounded-control border font-medium whitespace-nowrap transition-[background-color,border-color,color,filter]';

/** The classes of a button, for a link that looks like one (a Next `Link` to a create page). */
export function buttonClass(variant: ButtonVariant = 'secondary', size: ButtonSize = 'md'): string {
  return cn(BASE, VARIANTS[variant], SIZES[size]);
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  {
    variant = 'secondary',
    size = 'md',
    icon,
    iconAfter,
    busy,
    disabled,
    disabledReason,
    className,
    children,
    type,
    tooltip,
    ...rest
  },
  ref,
) {
  const reasonId = useId();
  const isDisabled = disabled || busy;
  const reason = isDisabled && disabledReason ? disabledReason : undefined;
  const button = (
    <button
      ref={ref}
      type={type ?? 'button'}
      disabled={isDisabled}
      aria-busy={busy || undefined}
      aria-describedby={reason ? reasonId : rest['aria-describedby']}
      className={cn(
        BASE,
        'disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:brightness-100',
        VARIANTS[variant],
        SIZES[size],
        className,
      )}
      {...rest}
    >
      {busy ? (
        <Spinner size={ICON_SIZE[size] - 2} />
      ) : icon ? (
        <Icon name={icon} size={ICON_SIZE[size]} className="-ml-0.5 shrink-0" />
      ) : null}
      {children}
      {iconAfter ? (
        <Icon name={iconAfter} size={ICON_SIZE[size]} className="-mr-1 shrink-0 opacity-70" />
      ) : null}
    </button>
  );
  // A button that can be disabled with a reason keeps one wrapper, so it doesn't remount (and
  // lose focus) when it turns busy; a disabled button gets no pointer events, the wrapper does.
  if (disabledReason === undefined) return tooltip ? <Tooltip content={tooltip}>{button}</Tooltip> : button;
  return (
    <Tooltip content={reason ?? tooltip} describe={!reason}>
      <span className={cn('inline-flex [&>button:disabled]:pointer-events-none', wrapperWidth(className))}>
        {button}
        {reason ? (
          <span id={reasonId} hidden>
            {reason}
          </span>
        ) : null}
      </span>
    </Tooltip>
  );
});

/** The wrapper of a full-width button is full width too. */
function wrapperWidth(className: string | undefined): string | undefined {
  if (!className) return undefined;
  return className
    .split(/\s+/)
    .filter((c) => /^(w-full|flex-1|grow|ml-auto|print:hidden)$/.test(c))
    .join(' ');
}

export interface IconButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'title'> {
  icon: IconName;
  /** The accessible name, also shown as the tooltip. */
  label: string;
  size?: number;
  /** Turns the label tooltip off (when the label is already on screen next to it). */
  noTooltip?: boolean;
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { icon, label, size = 16, className, type, noTooltip, ...rest },
  ref,
) {
  const button = (
    <button
      ref={ref}
      type={type ?? 'button'}
      aria-label={label}
      className={cn(
        'inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-control border border-transparent text-muted transition-colors',
        'hover:bg-hover hover:text-fg disabled:cursor-not-allowed disabled:opacity-50',
        className,
      )}
      {...rest}
    >
      <Icon name={icon} size={size} />
    </button>
  );
  if (noTooltip) return button;
  return (
    <Tooltip content={label} describe={false}>
      {button}
    </Tooltip>
  );
});

export function Spinner({ size = 12 }: { size?: number }) {
  return (
    <svg className="animate-spin" width={size} height={size} viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="3" />
      <path
        d="M21 12a9 9 0 0 0-9-9"
        fill="none"
        stroke="currentColor"
        strokeWidth="3"
        strokeLinecap="round"
      />
    </svg>
  );
}
