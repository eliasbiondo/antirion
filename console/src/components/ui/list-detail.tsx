'use client';

import { useEffect, useRef, type KeyboardEvent, type ReactNode } from 'react';

import { cn } from '@/lib/cn';
import { useMediaQuery } from '@/lib/hooks';

import type { Tone } from './badge';
import { Drawer } from './overlay';

/** Narrow screens (under 1024px): the list alone, the selection opens over it as a drawer. */
export interface NarrowDetail {
  /** Whether a selection was made on purpose (a row was chosen), so the drawer opens. */
  open: boolean;
  onClose: () => void;
  /** The drawer's title (the selected item's name). */
  title: string;
}

/**
 * The mockups' master-detail split: a selectable list on the left, the selection on the right. It
 * grows to the page's height, so a detail taller than the list never leaves a border across it.
 * The detail stays in view and scrolls on its own while the page scrolls (`sticky`, on unless a
 * screen turns it off), sized to the view above the keyboard-hints strip pinned under the page. With
 * `narrow`, screens under 1024px show the list alone and the detail in a full-width drawer.
 */
export function MasterDetail({
  list,
  detail,
  className,
  sticky = true,
  narrow,
}: {
  list: ReactNode;
  detail: ReactNode;
  className?: string;
  sticky?: boolean;
  narrow?: NarrowDetail | undefined;
}) {
  const small = useMediaQuery('(max-width: 1023px)');
  if (narrow && small)
    return (
      <div className={cn('flex-auto bg-canvas', className)}>
        {list}
        <Drawer open={narrow.open} onClose={narrow.onClose} title={narrow.title} width="lg">
          <div className="-mx-5 -my-4">{detail}</div>
        </Drawer>
      </div>
    );
  return (
    <div
      className={cn('grid min-h-[520px] flex-auto grid-cols-[minmax(340px,480px)_minmax(0,1fr)]', className)}
    >
      <div className="min-w-0 border-r border-line bg-canvas">{list}</div>
      <div className="min-w-0 bg-raised">
        {sticky ? <div className="sticky top-0 max-h-(--pane-h) overflow-y-auto">{detail}</div> : detail}
      </div>
    </div>
  );
}

export interface SelectableListProps<T> {
  items: T[];
  selectedKey: string | null;
  keyOf: (item: T) => string;
  onSelect: (key: string) => void;
  renderItem: (item: T, selected: boolean) => ReactNode;
  toneOf?: (item: T) => Tone;
  label: string;
}

const BAR: Record<Tone, string> = {
  neutral: 'bg-line-strong',
  success: 'bg-success',
  warn: 'bg-warn',
  error: 'bg-error',
  info: 'bg-info',
  accent: 'bg-accent',
};

/** Keys that move a list's selection: the arrows, and j / k. */
const MOVE_KEYS: Record<string, 1 | -1> = { ArrowDown: 1, j: 1, ArrowUp: -1, k: -1 };

/**
 * Whether a key pressed with focus outside the list should move it: not in a field, a dialog, a
 * menu or another listbox, and not taken by a shortcut (`g k`) first.
 */
export function movesPageList(e: globalThis.KeyboardEvent, list: Element | null): boolean {
  if (!(e.key in MOVE_KEYS) || e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return false;
  if (document.querySelector('[aria-modal="true"]')) return false;
  const active = document.activeElement;
  if (!active || active === document.body) return true;
  if (list?.contains(active)) return false;
  if (
    active.closest(
      'input, textarea, select, [role="dialog"], [role="listbox"], [role="menu"], [role="tablist"], [contenteditable="true"]',
    )
  )
    return false;
  return active.tagName === 'A' || active.tagName === 'BUTTON';
}

/**
 * A listbox of rows: arrow keys (and j / k) move the selection, Home and End jump, Enter or a click
 * selects. The first list on the page also answers ↑, ↓, j and k pressed anywhere else on it (from
 * page load, while focus is not in a field). Each row has a coloured status bar on its left edge.
 */
export function SelectableList<T>({
  items,
  selectedKey,
  keyOf,
  onSelect,
  renderItem,
  toneOf,
  label,
}: SelectableListProps<T>) {
  const refs = useRef(new Map<string, HTMLLIElement>());
  const listRef = useRef<HTMLUListElement>(null);
  const move = (step: number) => {
    const keys = items.map(keyOf);
    const i = selectedKey ? keys.indexOf(selectedKey) : -1;
    const next = i < 0 ? 0 : Math.max(0, Math.min(keys.length - 1, i + step));
    const key = keys[next];
    if (key === undefined) return false;
    if (key !== selectedKey) onSelect(key);
    refs.current.get(key)?.focus();
    return true;
  };
  const moveRef = useRef(move);
  useEffect(() => {
    moveRef.current = move;
  });
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const list = listRef.current;
      if (!list || document.querySelector('ul[role="listbox"][data-page-list]') !== list) return;
      if (!movesPageList(e, list)) return;
      if (moveRef.current(MOVE_KEYS[e.key] ?? 0)) e.preventDefault();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  // When the selection changes from outside (a link, the URL), focus follows it, so no other row
  // keeps a focused, selected-looking background.
  useEffect(() => {
    if (!selectedKey) return;
    const active = document.activeElement;
    for (const [key, el] of refs.current)
      if (el === active && key !== selectedKey) {
        refs.current.get(selectedKey)?.focus({ preventScroll: true });
        return;
      }
  }, [selectedKey]);
  function onKey(e: KeyboardEvent<HTMLUListElement>) {
    const keys = items.map(keyOf);
    const i = selectedKey ? keys.indexOf(selectedKey) : -1;
    let next = -1;
    if (e.key === 'ArrowDown' || e.key === 'j') next = Math.min(keys.length - 1, i + 1);
    else if (e.key === 'ArrowUp' || e.key === 'k') next = Math.max(0, i - 1);
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = keys.length - 1;
    if (next < 0) return;
    e.preventDefault();
    const key = keys[next];
    if (key === undefined) return;
    onSelect(key);
    refs.current.get(key)?.focus();
  }
  return (
    <ul
      ref={listRef}
      role="listbox"
      aria-label={label}
      onKeyDown={onKey}
      data-page-list
      className="flex flex-col"
    >
      {items.map((item, index) => {
        const key = keyOf(item);
        const selected = key === selectedKey;
        const tone = toneOf?.(item) ?? 'neutral';
        return (
          <li
            key={key}
            ref={(el) => {
              if (el) refs.current.set(key, el);
              else refs.current.delete(key);
            }}
            role="option"
            aria-selected={selected}
            tabIndex={selected || (selectedKey === null && index === 0) ? 0 : -1}
            onClick={() => onSelect(key)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                onSelect(key);
              }
            }}
            className={cn(
              'relative cursor-pointer border-b border-line py-3 pr-4 pl-5 transition-colors outline-none',
              selected ? 'bg-accent-dim' : 'bg-raised hover:bg-hover focus-visible:bg-hover',
            )}
          >
            <span aria-hidden="true" className={cn('absolute top-0 bottom-0 left-0 w-[3px]', BAR[tone])} />
            {selected ? (
              <span aria-hidden="true" className="absolute top-0 right-0 bottom-0 w-0.5 bg-accent" />
            ) : null}
            {renderItem(item, selected)}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * The detail pane's header: eyebrow facts, title, and an actions bar under it. It stays pinned to
 * the top of the pane while the detail scrolls, so the selection's name and actions never leave view.
 */
export function DetailHeader({
  eyebrow,
  title,
  subtitle,
  actions,
  aside,
}: {
  eyebrow?: ReactNode;
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  aside?: ReactNode;
}) {
  return (
    <div className="sticky top-0 z-[5] border-b border-line bg-raised">
      <div className="flex items-start gap-4 px-7 pt-6 pb-4">
        <div className="min-w-0 flex-1">
          {eyebrow ? (
            <div className="mono flex flex-wrap items-center gap-2 text-caption text-muted">{eyebrow}</div>
          ) : null}
          <h2 className="mt-2 truncate text-[22px] font-semibold tracking-tight text-fg">{title}</h2>
          {subtitle ? <div className="mono mt-1.5 text-small text-muted">{subtitle}</div> : null}
        </div>
        {aside}
      </div>
      {actions ? (
        <div className="flex flex-wrap items-center gap-2 border-t border-line px-7 py-3">{actions}</div>
      ) : null}
    </div>
  );
}

export function DetailSection({
  title,
  aside,
  children,
}: {
  title: ReactNode;
  aside?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="border-b border-line px-7 py-5">
      <div className="mb-3 flex items-center justify-between gap-3">
        <h3 className="eyebrow">{title}</h3>
        {aside}
      </div>
      {children}
    </section>
  );
}
