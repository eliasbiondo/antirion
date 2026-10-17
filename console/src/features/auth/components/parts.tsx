'use client';

import { forwardRef, useState, type ButtonHTMLAttributes, type ReactNode } from 'react';

import { Input, type InputProps } from '@/components/ui/form';
import { Icon, type IconName } from '@/components/ui/icon';
import { cn } from '@/lib/cn';

/** One message for a failed step, in plain words, announced to assistive technology. */
export function ErrorStrip({ message }: { message: string | null }) {
  if (!message) return null;
  return (
    <p
      role="alert"
      className="flex items-start gap-2 rounded-sm border border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-error)_8%,transparent)] px-3 py-2.5 text-ui leading-snug text-fg"
    >
      <span className="mt-px shrink-0 text-error">
        <Icon name="alert" size={14} />
      </span>
      <span>{message}</span>
    </p>
  );
}

/** A quiet note: a tip, or what happens next. */
export function Note({ children, icon = 'info' }: { children: ReactNode; icon?: IconName }) {
  return (
    <div className="flex items-start gap-2 rounded-sm border border-line bg-sunken px-3 py-2.5 text-caption leading-relaxed text-muted">
      <span className="mt-px shrink-0 text-dim">
        <Icon name={icon} size={13} />
      </span>
      <div>{children}</div>
    </div>
  );
}

/** The card's heading: what this step is for, and where it sits in a multi-step flow. */
export function StepHeader({
  title,
  subtitle,
  step,
}: {
  title: string;
  subtitle?: ReactNode;
  step?: { current: number; total: number } | null;
}) {
  return (
    <div className="mb-6 flex flex-col gap-1.5">
      {step ? (
        <p className="text-caption font-medium text-accent">
          Step {step.current} of {step.total}
        </p>
      ) : null}
      <h1 className="text-heading leading-tight font-semibold tracking-tight text-fg">{title}</h1>
      {subtitle ? <div className="text-body leading-relaxed text-muted">{subtitle}</div> : null}
    </div>
  );
}

/** A text-only action inside the card ("Forgot password?", "Use an admin token"). */
export const TextButton = forwardRef<
  HTMLButtonElement,
  ButtonHTMLAttributes<HTMLButtonElement> & { tone?: 'accent' | 'muted'; icon?: IconName }
>(function TextButton({ tone = 'muted', icon, className, children, type, ...rest }, ref) {
  return (
    <button
      ref={ref}
      type={type ?? 'button'}
      className={cn(
        'inline-flex items-center gap-1.5 rounded-xs text-ui underline-offset-4 hover:underline disabled:opacity-50',
        tone === 'accent' ? 'text-accent' : 'text-muted hover:text-fg',
        className,
      )}
      {...rest}
    >
      {icon ? <Icon name={icon} size={13} /> : null}
      {children}
    </button>
  );
});

/** A password or token field with a show and hide toggle; paste and password managers work as usual. */
export const SecretInput = forwardRef<
  HTMLInputElement,
  Omit<InputProps, 'type'> & { what?: string; compact?: boolean }
>(function SecretInput({ what = 'password', compact, className, ...rest }, ref) {
  const [show, setShow] = useState(false);
  return (
    <div className="relative">
      <Input
        ref={ref}
        type={show ? 'text' : 'password'}
        spellCheck={false}
        autoCapitalize="off"
        className={cn('pr-10', !compact && 'h-10 text-base', className)}
        {...rest}
      />
      <button
        type="button"
        aria-label={show ? `Hide ${what}` : `Show ${what}`}
        aria-pressed={show}
        onClick={() => setShow(!show)}
        className="absolute top-1/2 right-1.5 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-xs text-muted hover:bg-hover hover:text-fg"
      >
        <Icon name={show ? 'eyeOff' : 'eye'} size={15} />
      </button>
    </div>
  );
});

/** "123 456", "123-456" and "123456" all mean the same code. */
export function digitsOf(value: string): string {
  return value.replace(/[\s-]/g, '');
}

/** The one field a second-factor code goes in: numeric keyboard, one-time-code autofill, paste. */
export const CodeInput = forwardRef<
  HTMLInputElement,
  Omit<InputProps, 'type'> & { recovery?: boolean; compact?: boolean }
>(function CodeInput({ recovery, compact, className, ...rest }, ref) {
  return (
    <Input
      ref={ref}
      type="text"
      mono
      inputMode={recovery ? 'text' : 'numeric'}
      autoComplete="one-time-code"
      spellCheck={false}
      autoCapitalize="off"
      maxLength={recovery ? 64 : 9}
      className={cn(!compact && 'h-11 text-center text-title tracking-[0.3em]', className)}
      {...rest}
    />
  );
});

/** The secondary ways in, under a rule at the bottom of the card. */
export function MoreOptions({ children }: { children: ReactNode }) {
  return <div className="mt-6 flex flex-col gap-3 border-t border-line pt-5">{children}</div>;
}
