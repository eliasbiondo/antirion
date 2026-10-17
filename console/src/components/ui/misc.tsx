'use client';

import { useState, type ReactNode } from 'react';

import { cn } from '@/lib/cn';
import { hueOf, initials } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { useUi } from '@/stores/ui';

import { Button, IconButton } from './button';
import { Icon } from './icon';

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="mono inline-flex h-5 min-w-5 items-center justify-center rounded-xs border border-line bg-sunken px-1 text-label leading-none text-muted">
      {children}
    </kbd>
  );
}

/** Copies a non-secret value (a link, a name, a key prefix) and says so. */
export function CopyButton({
  value,
  label,
  children,
}: {
  value: string;
  label: string;
  children?: ReactNode;
}) {
  const [done, setDone] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setDone(true);
      setTimeout(() => setDone(false), 1500);
    } catch {
      setDone(false);
    }
  }
  if (!children)
    return (
      <IconButton
        icon={done ? 'check' : 'copy'}
        label={done ? 'Copied' : label}
        onClick={() => void copy()}
        size={14}
      />
    );
  return (
    <Button size="sm" icon={done ? 'check' : 'copy'} onClick={() => void copy()} aria-label={label}>
      {done ? 'Copied' : children}
    </Button>
  );
}

/** "Copy link" for the current view: every entity has a stable URL. */
export function CopyLinkButton() {
  const [done, setDone] = useState(false);
  return (
    <Button
      size="sm"
      icon={done ? 'check' : 'link'}
      onClick={() => {
        void navigator.clipboard.writeText(window.location.href).then(() => {
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        });
      }}
    >
      {done ? 'Copied' : 'Copy link'}
    </Button>
  );
}

export interface AvatarProps {
  name: string;
  size?: 'sm' | 'md' | 'lg';
  /** A hex colour for teams that have one; otherwise a hue derived from the name. */
  color?: string | undefined;
  /** A photo (an image URL, such as a `data:` URL) shown instead of the initials. */
  photo?: string | null | undefined;
}

const AVATAR_SIZE = { sm: 'h-7 w-7 text-caption', md: 'h-9 w-9 text-label', lg: 'h-14 w-14 text-title' };
const HUES = [
  'bg-[oklch(0.62_0.13_190)]',
  'bg-[oklch(0.62_0.13_30)]',
  'bg-[oklch(0.6_0.13_150)]',
  'bg-[oklch(0.6_0.14_285)]',
  'bg-[oklch(0.62_0.13_330)]',
  'bg-[oklch(0.62_0.12_80)]',
  'bg-[oklch(0.58_0.14_250)]',
  'bg-[oklch(0.6_0.12_120)]',
];

export function Avatar({ name, size = 'md', color, photo }: AvatarProps) {
  const hue = HUES[hueOf(name) % HUES.length] ?? HUES[0];
  if (photo)
    return (
      // eslint-disable-next-line @next/next/no-img-element -- a small data URL; nothing to optimise
      <img
        src={photo}
        alt=""
        aria-hidden="true"
        className={cn('inline-block shrink-0 rounded-full object-cover', AVATAR_SIZE[size])}
      />
    );
  return (
    <span
      aria-hidden="true"
      ref={(el) => {
        if (el && color) el.style.backgroundColor = color;
      }}
      className={cn(
        'inline-flex shrink-0 items-center justify-center rounded-full font-semibold text-white',
        AVATAR_SIZE[size],
        !color && hue,
      )}
    >
      {initials(name)}
    </span>
  );
}

/** A small square in a team's or provider's colour, set through CSSOM so no inline style is emitted. */
export function ColorChip({ color, className }: { color: string | undefined; className?: string }) {
  return (
    <span
      aria-hidden="true"
      ref={(el) => {
        if (el) el.style.backgroundColor = color ?? 'var(--ant-fg-dim)';
      }}
      className={cn('inline-block h-2 w-2 shrink-0 rounded-[2px]', className)}
    />
  );
}

/** Toasts at the bottom right: what happened, one line of detail, and at most one action. */
export function Toaster() {
  const toasts = useUi((s) => s.toasts);
  const dismiss = useUi((s) => s.dismiss);
  const tone = { info: 'text-info', success: 'text-success', warn: 'text-warn', error: 'text-error' };
  const icon = { info: 'info', success: 'checkCircle', warn: 'alert', error: 'alertCircle' } as const;
  return (
    <div
      aria-live="polite"
      className="pointer-events-none fixed bottom-4 left-4 z-[60] flex w-[380px] max-w-[calc(100vw-2rem)] flex-col gap-2"
    >
      {toasts.map((t) => (
        <div
          key={t.id}
          role={t.tone === 'error' ? 'alert' : 'status'}
          className="pointer-events-auto flex animate-fade-in items-start gap-3 rounded-container border border-line-strong bg-raised py-3 pr-2 pl-3.5 shadow-pop"
        >
          <span className={cn('mt-0.5', tone[t.tone])}>
            <Icon name={icon[t.tone]} size={16} />
          </span>
          <div className="min-w-0 flex-1">
            <p className="text-ui font-semibold text-fg">{t.title}</p>
            {t.detail ? <p className="mt-0.5 text-caption break-words text-muted">{t.detail}</p> : null}
          </div>
          {t.action ? (
            <Button
              size="sm"
              variant="ghost"
              className="text-fg"
              onClick={() => {
                t.action?.onClick();
                dismiss(t.id);
              }}
            >
              {t.action.label}
            </Button>
          ) : null}
          <IconButton icon="x" label="Dismiss" size={14} onClick={() => dismiss(t.id)} className="h-6 w-6" />
        </div>
      ))}
    </div>
  );
}

/** "Updated 12 s ago", shown on every live view next to its refresh control. */
export function UpdatedAt({ at }: { at: number | null }) {
  const now = useNow(5000);
  if (!at) return null;
  const s = Math.max(0, Math.round((now - at) / 1000));
  const text = s < 5 ? 'just now' : s < 60 ? `${s} s ago` : `${Math.round(s / 60)} min ago`;
  return <span className="text-caption whitespace-nowrap text-dim tabular-nums">Updated {text}</span>;
}
