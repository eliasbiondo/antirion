import type { ReactNode } from 'react';

import { cn } from '@/lib/cn';
import { Tooltip } from './tooltip';

export type Tone = 'neutral' | 'success' | 'warn' | 'error' | 'info' | 'accent';

const TONES: Record<Tone, string> = {
  neutral: 'text-muted border-line bg-sunken',
  success:
    'text-success border-[color-mix(in_oklab,var(--ant-success)_35%,transparent)] bg-[color-mix(in_oklab,var(--ant-success)_9%,transparent)]',
  warn: 'text-warn border-[color-mix(in_oklab,var(--ant-warn)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-warn)_10%,transparent)]',
  error:
    'text-error border-[color-mix(in_oklab,var(--ant-error)_40%,transparent)] bg-[color-mix(in_oklab,var(--ant-error)_9%,transparent)]',
  info: 'text-info border-[color-mix(in_oklab,var(--ant-info)_35%,transparent)] bg-[color-mix(in_oklab,var(--ant-info)_9%,transparent)]',
  accent:
    'text-accent border-[color-mix(in_oklab,var(--ant-accent)_35%,transparent)] bg-[color-mix(in_oklab,var(--ant-accent)_10%,transparent)]',
};

export interface BadgeProps {
  tone?: Tone;
  /**
   * A status word ("active", "service"): shown in sentence case ("Active"). It was an uppercase
   * pill; the name stays for callers.
   */
  caps?: boolean;
  /** A code-like value (a status code, an id, a mode name) in the monospace face. */
  mono?: boolean;
  /** A leading dot in the tone's colour, so the status is not told by colour alone. */
  dot?: boolean;
  children: ReactNode;
  className?: string;
  title?: string;
}

function sentence(children: ReactNode): ReactNode {
  return typeof children === 'string' && children.length > 0
    ? children.charAt(0).toUpperCase() + children.slice(1)
    : children;
}

export function Badge({ tone = 'neutral', caps, mono, dot, children, className, title }: BadgeProps) {
  const badge = (
    <span
      className={cn(
        'inline-flex h-5 items-center gap-1.5 rounded-xs border px-1.5 text-label leading-4 font-medium whitespace-nowrap',
        mono && 'mono font-normal',
        TONES[tone],
        className,
      )}
    >
      {dot ? <Dot tone={tone} /> : null}
      {caps ? sentence(children) : children}
    </span>
  );
  return title ? <Tooltip content={title}>{badge}</Tooltip> : badge;
}

const DOTS: Record<Tone, string> = {
  neutral: 'bg-dim',
  success: 'bg-success',
  warn: 'bg-warn',
  error: 'bg-error',
  info: 'bg-info',
  accent: 'bg-accent',
};

export function Dot({ tone = 'neutral', className }: { tone?: Tone; className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cn('inline-block h-1.5 w-1.5 shrink-0 rounded-full', DOTS[tone], className)}
    />
  );
}

/** A status code as the log shows it: green 2xx, amber 4xx, red 5xx. */
export function StatusCode({ code }: { code: number }) {
  const tone: Tone =
    code >= 500 ? 'error' : code >= 400 ? 'warn' : code >= 200 && code < 300 ? 'success' : 'neutral';
  return (
    <Badge tone={tone} mono>
      {code}
    </Badge>
  );
}

/** A small swatch for a provider or team colour, given as a CSS class. */
export function Swatch({ className }: { className: string }) {
  return <span aria-hidden="true" className={cn('inline-block h-2 w-2 shrink-0 rounded-[2px]', className)} />;
}
