'use client';

import {
  forwardRef,
  useEffect,
  useId,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';

import { cn } from '@/lib/cn';

import { useFloating } from './floating';
import { Icon } from './icon';
import { useLayer } from './layers';
import { EmptyPill } from './states';

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
  /** A second, muted line under the label. */
  description?: string;
  /** A small mark before the label (a provider's logo), decorative. */
  icon?: ReactNode;
}

interface ControlAria {
  id?: string;
  'aria-label'?: string;
  'aria-describedby'?: string;
  'aria-invalid'?: boolean;
  'aria-required'?: boolean;
}

interface BaseProps extends ControlAria {
  options: SelectOption[];
  disabled?: boolean;
  className?: string;
  /** `sm` for toolbars and table rows (28px), `md` for forms (32px). */
  size?: 'sm' | 'md';
  mono?: boolean;
  name?: string;
  /** Takes the first focus when its dialog or drawer opens. */
  'data-autofocus'?: boolean;
}

const TRIGGER =
  'flex w-full min-w-0 items-center justify-between gap-2 rounded-control border border-line bg-canvas px-2.5 text-left text-ui text-fg ' +
  'hover:border-line-strong focus:border-accent focus:outline-none focus-visible:outline-none focus:ring-2 focus:ring-accent-dim ' +
  'disabled:cursor-not-allowed disabled:opacity-60 aria-expanded:border-accent aria-[invalid=true]:border-error';

const LIST =
  'fixed top-0 left-0 z-[60] flex max-h-[min(320px,var(--floating-max-height,320px))] min-w-[160px] max-w-[min(480px,calc(100vw-16px))] animate-fade-in flex-col overflow-hidden rounded-container border border-line-strong bg-raised shadow-pop';

const OPTION =
  'flex min-h-8 cursor-default items-center gap-2 rounded-sm px-2.5 py-1.5 text-ui text-fg select-none aria-disabled:cursor-not-allowed aria-disabled:opacity-50';

/** Where a listbox mounts: in the modal dialog that holds the trigger, else the body. */
function targetFor(el: HTMLElement | null): HTMLElement {
  return el?.closest<HTMLElement>('[aria-modal="true"]') ?? document.body;
}

/** The trigger's visible label, as the listbox's name. */
function labelOf(el: HTMLElement | null, fallback: string | undefined): string | undefined {
  if (fallback) return fallback;
  const labels = (el as HTMLButtonElement | null)?.labels;
  const text = labels?.[0]?.textContent
    .replace(/\s*\*\s*$/, '')
    .replace(/\(optional\)$/, '')
    .trim();
  return text ?? undefined;
}

function enabledIndex(options: SelectOption[], from: number, step: 1 | -1): number {
  for (let i = from; i >= 0 && i < options.length; i += step) if (!options[i]?.disabled) return i;
  return -1;
}

function first(options: SelectOption[]): number {
  return enabledIndex(options, 0, 1);
}

function last(options: SelectOption[]): number {
  return enabledIndex(options, options.length - 1, -1);
}

/** Moves the active option for a navigation key; null when the key is not one. */
function navigate(key: string, active: number, options: SelectOption[]): number | null {
  if (key === 'ArrowDown') {
    const next = enabledIndex(options, active + 1, 1);
    return next === -1 ? active : next;
  }
  if (key === 'ArrowUp') {
    const prev = active <= 0 ? -1 : enabledIndex(options, active - 1, -1);
    return prev === -1 ? Math.max(active, first(options)) : prev;
  }
  if (key === 'Home') return first(options);
  if (key === 'End') return last(options);
  if (key === 'PageDown') {
    const next = enabledIndex(options, Math.min(options.length - 1, active + 10), 1);
    return next === -1 ? last(options) : next;
  }
  if (key === 'PageUp') {
    const prev = enabledIndex(options, Math.max(0, active - 10), -1);
    return prev === -1 ? first(options) : prev;
  }
  return null;
}

/**
 * Type-ahead: letters typed within half a second find the next option starting with them; the
 * same letter again cycles through the options that start with it.
 */
function useTypeahead() {
  const buffer = useRef('');
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  return {
    typing: () => buffer.current.length > 0,
    find(char: string, active: number, options: SelectOption[]): number {
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => {
        buffer.current = '';
      }, 500);
      buffer.current += char.toLowerCase();
      const term = buffer.current;
      const cycling = term.length > 1 && term.split('').every((c) => c === term[0]);
      const search = cycling ? (term[0] ?? '') : term;
      const start = cycling || term.length === 1 ? active + 1 : Math.max(active, 0);
      for (let n = 0; n < options.length; n++) {
        const i = (start + n) % options.length;
        const o = options[i];
        if (o && !o.disabled && o.label.toLowerCase().startsWith(search)) return i;
      }
      return active;
    },
  };
}

function scrollIntoView(list: HTMLElement | null, id: string) {
  const el = list?.querySelector<HTMLElement>(`[id="${id}"]`);
  el?.scrollIntoView({ block: 'nearest' });
}

function Check({ on }: { on: boolean }) {
  return (
    <span className={cn('w-3.5 shrink-0 text-accent', on ? 'visible' : 'invisible')} aria-hidden="true">
      <Icon name="check" size={14} />
    </span>
  );
}

function OptionRow({
  id,
  option,
  selected,
  active,
  multi,
  onPick,
  onHover,
}: {
  id: string;
  option: SelectOption;
  selected: boolean;
  active: boolean;
  multi?: boolean;
  onPick: () => void;
  onHover: () => void;
}) {
  return (
    <div
      id={id}
      role="option"
      aria-selected={selected}
      aria-disabled={option.disabled || undefined}
      data-active={active || undefined}
      onMouseDown={(e) => e.preventDefault()}
      onMouseMove={onHover}
      onClick={() => {
        if (!option.disabled) onPick();
      }}
      className={cn(
        OPTION,
        active && 'bg-hover',
        selected && !multi && 'font-medium',
        option.value === '' && 'text-muted',
      )}
    >
      {multi ? (
        <span
          aria-hidden="true"
          className={cn(
            'flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-xs border',
            selected ? 'border-accent bg-accent text-accent-fg' : 'border-line-strong bg-canvas',
          )}
        >
          {selected ? <Icon name="check" size={11} /> : null}
        </span>
      ) : (
        <Check on={selected} />
      )}
      {option.icon ? (
        <span aria-hidden="true" className="flex shrink-0 text-muted">
          {option.icon}
        </span>
      ) : null}
      <span className="min-w-0 flex-1">
        <span className="block truncate">{option.label}</span>
        {option.description ? (
          <span className="block truncate text-caption text-muted">{option.description}</span>
        ) : null}
      </span>
    </div>
  );
}

/** Closes on a press outside the trigger and the list, while this list is the top overlay. */
function useOutsidePress(
  open: boolean,
  isTop: () => boolean,
  parts: { current: HTMLElement | null }[],
  close: () => void,
) {
  const closeRef = useRef(close);
  useEffect(() => {
    closeRef.current = close;
  });
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (parts.some((p) => p.current?.contains(t)) || !isTop()) return;
      closeRef.current();
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open, isTop, parts]);
}

export interface SelectProps extends BaseProps {
  value: string;
  onChange: (value: string) => void;
  /**
   * The trigger's text while nothing is chosen ("Select a provider"). It is a prompt, not an option:
   * the list starts at the first real value and nothing is ticked.
   */
  placeholder?: string;
  /**
   * An option with the empty value at the top, for when "nothing" is a choice of its own ("All
   * teams", "None"); it is ticked while the value is empty.
   */
  emptyOption?: string;
  /** A search field over the options, for long lists (models, providers, teams, people). */
  searchable?: boolean;
  /** The search field's placeholder. */
  searchLabel?: string;
}

export interface SelectHandle {
  focus: () => void;
}

/**
 * The console's select, instead of the browser's: a button that opens a listbox under it (above
 * near the bottom of the screen). The WAI-ARIA select-only combobox: the trigger keeps focus and
 * points at the active option; arrows, Home, End, Page Up and Down move; letters jump; Enter,
 * Space or Tab choose; Escape closes without a change. With `searchable` the list gets a search
 * field that filters as you type (the editable combobox pattern), for long lists.
 */
export const Select = forwardRef<SelectHandle, SelectProps>(function Select(
  {
    options: given,
    value,
    onChange,
    placeholder,
    emptyOption,
    searchable = false,
    searchLabel,
    disabled,
    className,
    size = 'md',
    mono,
    name,
    id,
    ...aria
  },
  ref,
) {
  const autoId = useId();
  const triggerId = id ?? `select${autoId}`;
  const listId = `${triggerId}-list`;
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const [target, setTarget] = useState<HTMLElement | null>(null);
  const [active, setActive] = useState(-1);
  const [query, setQuery] = useState('');
  const [listLabel, setListLabel] = useState<string | undefined>(undefined);
  const open = target !== null;
  const isTop = useLayer(open);
  const typeahead = useTypeahead();
  useImperativeHandle(ref, () => ({ focus: () => trigger.current?.focus() }), []);

  const all = useMemo<SelectOption[]>(
    () => (emptyOption !== undefined ? [{ value: '', label: emptyOption }, ...given] : given),
    [given, emptyOption],
  );
  const options = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!searchable || !q) return all;
    return all.filter(
      (o) => o.value !== '' && (o.label.toLowerCase().includes(q) || o.value.toLowerCase().includes(q)),
    );
  }, [all, query, searchable]);
  const selectedIndex = all.findIndex((o) => o.value === value);
  const selected = selectedIndex >= 0 ? all[selectedIndex] : undefined;
  const optionId = (i: number) => `${listId}-${i}`;

  useFloating(trigger, list, open, { matchWidth: true });

  const closeList = (focusTrigger: boolean) => {
    setTarget(null);
    setQuery('');
    if (focusTrigger) trigger.current?.focus();
  };
  useOutsidePress(open, isTop, [trigger, list], () => closeList(false));

  useEffect(() => {
    if (open && active >= 0) scrollIntoView(list.current, optionId(active));
    // optionId only depends on listId.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, active]);

  useEffect(() => {
    if (open && searchable) search.current?.focus();
  }, [open, searchable]);

  const openList = (at?: number) => {
    if (disabled || open) return;
    setListLabel(labelOf(trigger.current, aria['aria-label']));
    setActive(at ?? (selectedIndex >= 0 ? selectedIndex : first(all)));
    setTarget(targetFor(trigger.current));
  };

  const choose = (i: number, focusTrigger = true) => {
    const o = options[i];
    if (o && !o.disabled && o.value !== value) onChange(o.value);
    closeList(focusTrigger);
  };

  const onListKey = (e: KeyboardEvent<HTMLElement>, fromSearch: boolean) => {
    const moved = navigate(e.key, active, options);
    if (moved !== null && !(fromSearch && (e.key === 'Home' || e.key === 'End'))) {
      e.preventDefault();
      if (e.altKey && e.key === 'ArrowUp') {
        choose(active);
        return;
      }
      setActive(moved);
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      choose(active);
    } else if (e.key === ' ' && !fromSearch) {
      e.preventDefault();
      if (typeahead.typing()) setActive(typeahead.find(' ', active, options));
      else choose(active);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      closeList(true);
    } else if (e.key === 'Tab') {
      if (fromSearch) {
        // Leave from the trigger, so Tab continues to the control after it.
        trigger.current?.focus();
        closeList(false);
      } else choose(active, false);
    } else if (!fromSearch && e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      setActive(typeahead.find(e.key, active, options));
    }
  };

  const onTriggerKey = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (open) {
      if (!searchable) onListKey(e, false);
      return;
    }
    if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(e.key)) {
      e.preventDefault();
      openList();
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      openList(e.key === 'Home' ? first(all) : last(all));
    } else if (!searchable && e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      openList(typeahead.find(e.key, selectedIndex, all));
    }
  };

  const shown = selected?.label ?? placeholder ?? emptyOption ?? '';
  const muted = !selected || selected.value === '';

  return (
    <>
      <button
        ref={trigger}
        id={triggerId}
        type="button"
        role="combobox"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-activedescendant={open && !searchable && active >= 0 ? optionId(active) : undefined}
        data-autofocus={aria['data-autofocus'] || undefined}
        aria-label={aria['aria-label']}
        aria-describedby={aria['aria-describedby']}
        aria-invalid={aria['aria-invalid']}
        aria-required={aria['aria-required']}
        disabled={disabled}
        onClick={() => (open ? closeList(true) : openList())}
        onKeyDown={onTriggerKey}
        onBlur={(e) => {
          // Focus left for something else (not the list or its search field): close.
          const to = e.relatedTarget as Node | null;
          if (open && !searchable && !list.current?.contains(to)) closeList(false);
        }}
        className={cn(TRIGGER, size === 'sm' ? 'h-7 text-caption' : 'h-8', className)}
      >
        <span className="flex min-w-0 items-center gap-2">
          {selected?.icon ? (
            <span aria-hidden="true" className="flex shrink-0 text-muted">
              {selected.icon}
            </span>
          ) : null}
          <span className={cn('min-w-0 truncate', mono && 'mono', muted && 'text-dim')}>{shown}</span>
        </span>
        <Icon name="chevronDown" size={14} className="shrink-0 text-dim" />
      </button>
      {name !== undefined ? <input type="hidden" name={name} value={value} /> : null}
      {target
        ? createPortal(
            <div ref={list} className={LIST} data-select-popup>
              {searchable ? (
                <div className="flex shrink-0 items-center gap-2 border-b border-line px-2.5">
                  <Icon name="search" size={14} className="text-dim" />
                  <input
                    ref={search}
                    type="text"
                    role="combobox"
                    aria-expanded="true"
                    aria-controls={listId}
                    aria-autocomplete="list"
                    aria-activedescendant={active >= 0 && options[active] ? optionId(active) : undefined}
                    aria-label={searchLabel ?? `Search ${listLabel ?? 'options'}`}
                    placeholder={searchLabel ?? 'Search…'}
                    autoComplete="off"
                    spellCheck={false}
                    value={query}
                    onChange={(e) => {
                      setQuery(e.target.value);
                      setActive(0);
                    }}
                    onKeyDown={(e) => onListKey(e, true)}
                    className="h-9 min-w-0 flex-1 bg-transparent text-ui text-fg outline-none placeholder:text-dim"
                  />
                </div>
              ) : null}
              <div
                id={listId}
                role="listbox"
                aria-label={listLabel}
                tabIndex={-1}
                onKeyDown={(e) => onListKey(e, false)}
                onBlur={(e) => {
                  const to = e.relatedTarget as Node | null;
                  if (!list.current?.contains(to) && to !== trigger.current) closeList(false);
                }}
                className="min-h-0 overflow-y-auto overscroll-contain p-1"
              >
                {options.length === 0 ? (
                  <div className="flex justify-center px-2.5 py-3">
                    <EmptyPill>No matches</EmptyPill>
                  </div>
                ) : null}
                {options.map((o, i) => (
                  <OptionRow
                    key={o.value === '' ? '__placeholder' : o.value}
                    id={optionId(i)}
                    option={o}
                    selected={o.value === value}
                    active={i === active}
                    onPick={() => choose(i)}
                    onHover={() => {
                      if (i !== active) setActive(i);
                    }}
                  />
                ))}
              </div>
            </div>,
            target,
          )
        : null}
    </>
  );
});

/** A searchable select, for long lists (models, providers, teams, people). */
export const Combobox = forwardRef<SelectHandle, Omit<SelectProps, 'searchable'>>(
  function Combobox(props, ref) {
    return <Select ref={ref} {...props} searchable />;
  },
);

export interface MultiSelectProps extends BaseProps {
  value: string[];
  onChange: (value: string[]) => void;
  /** Shown when nothing is picked. */
  placeholder?: string;
  /** Lets the person add a value the list doesn't have ("claude-*"), from the search field. */
  creatable?: boolean;
  /** What a picked value is called, for the chips' remove buttons ("team"). */
  noun?: string;
}

/**
 * Several values from a list, shown as chips in the field: each chip has its own remove button,
 * Backspace on the field removes the last one, and the list (with a search field) stays open
 * while you pick. With `creatable`, a value the list doesn't have can be added from the search.
 */
export function MultiSelect({
  options: given,
  value,
  onChange,
  placeholder = 'Choose…',
  creatable = false,
  noun = 'value',
  disabled,
  className,
  size = 'md',
  mono,
  name,
  id,
  ...aria
}: MultiSelectProps) {
  const autoId = useId();
  const triggerId = id ?? `multi${autoId}`;
  const listId = `${triggerId}-list`;
  const field = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const [target, setTarget] = useState<HTMLElement | null>(null);
  const [active, setActive] = useState(0);
  const [query, setQuery] = useState('');
  const [listLabel, setListLabel] = useState<string | undefined>(undefined);
  const open = target !== null;
  const isTop = useLayer(open);

  const known = useMemo(() => {
    const extra = value.filter((v) => !given.some((o) => o.value === v)).map((v) => ({ value: v, label: v }));
    return [...given, ...extra];
  }, [given, value]);
  const q = query.trim();
  const options = useMemo<SelectOption[]>(() => {
    const lower = q.toLowerCase();
    const matches = lower
      ? known.filter((o) => o.label.toLowerCase().includes(lower) || o.value.toLowerCase().includes(lower))
      : known;
    const exists = known.some((o) => o.value.toLowerCase() === lower);
    return creatable && q && !exists ? [...matches, { value: q, label: `Add “${q}”` }] : matches;
  }, [known, q, creatable]);
  const labelFor = (v: string) => known.find((o) => o.value === v)?.label ?? v;
  const optionId = (i: number) => `${listId}-${i}`;

  useFloating(field, list, open, { matchWidth: true });

  const closeList = (focusTrigger: boolean) => {
    setTarget(null);
    setQuery('');
    if (focusTrigger) trigger.current?.focus();
  };
  useOutsidePress(open, isTop, [field, list], () => closeList(false));

  useEffect(() => {
    if (open) search.current?.focus();
  }, [open]);
  useEffect(() => {
    if (open) scrollIntoView(list.current, optionId(active));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, active]);

  const openList = () => {
    if (disabled || open) return;
    setListLabel(labelOf(trigger.current, aria['aria-label']));
    setActive(0);
    setTarget(targetFor(field.current));
  };
  const toggle = (v: string) => {
    onChange(value.includes(v) ? value.filter((x) => x !== v) : [...value, v]);
  };

  const onSearchKey = (e: KeyboardEvent<HTMLInputElement>) => {
    const moved = e.key === 'Home' || e.key === 'End' ? null : navigate(e.key, active, options);
    if (moved !== null) {
      e.preventDefault();
      setActive(moved);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const o = options[active];
      if (o && !o.disabled) {
        toggle(o.value);
        if (o.label.startsWith('Add “')) setQuery('');
      }
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      closeList(true);
    } else if (e.key === 'Tab') {
      trigger.current?.focus();
      closeList(false);
    } else if (e.key === 'Backspace' && query === '' && value.length > 0) {
      onChange(value.slice(0, -1));
    }
  };

  return (
    <>
      <div
        ref={field}
        onClick={(e) => {
          if (e.target === e.currentTarget) openList();
        }}
        className={cn(
          'flex w-full min-w-0 flex-wrap items-center gap-1 rounded-control border border-line bg-canvas px-1 py-0.5',
          'hover:border-line-strong has-[button[role=combobox]:focus]:border-accent has-[button[role=combobox]:focus]:ring-2 has-[button[role=combobox]:focus]:ring-accent-dim',
          open && 'border-accent',
          aria['aria-invalid'] && 'border-error',
          disabled && 'cursor-not-allowed opacity-60',
          size === 'sm' ? 'min-h-7' : 'min-h-8',
          className,
        )}
      >
        {value.map((v) => (
          <span
            key={v}
            className={cn(
              'inline-flex h-6 max-w-full items-center gap-1 rounded-xs border border-line bg-sunken pr-0.5 pl-2 text-caption text-fg',
              mono && 'mono',
            )}
          >
            <span className="truncate">{labelFor(v)}</span>
            <button
              type="button"
              disabled={disabled}
              aria-label={`Remove ${noun} ${labelFor(v)}`}
              onClick={() => onChange(value.filter((x) => x !== v))}
              className="inline-flex h-5 w-5 items-center justify-center rounded-xs text-dim hover:bg-hover hover:text-fg"
            >
              <Icon name="x" size={12} />
            </button>
          </span>
        ))}
        <button
          ref={trigger}
          id={triggerId}
          type="button"
          role="combobox"
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-controls={open ? listId : undefined}
          aria-label={aria['aria-label']}
          aria-describedby={aria['aria-describedby']}
          aria-invalid={aria['aria-invalid']}
          aria-required={aria['aria-required']}
          data-autofocus={aria['data-autofocus'] || undefined}
          disabled={disabled}
          onClick={() => (open ? closeList(true) : openList())}
          onKeyDown={(e) => {
            if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(e.key)) {
              e.preventDefault();
              openList();
            } else if (e.key === 'Backspace' && value.length > 0) {
              e.preventDefault();
              onChange(value.slice(0, -1));
            }
          }}
          className="flex h-6 min-w-16 flex-1 items-center justify-between gap-2 rounded-xs px-1.5 text-left text-ui text-dim outline-none"
        >
          <span className="truncate">{value.length === 0 ? placeholder : 'Add…'}</span>
          <Icon name="chevronDown" size={14} className="shrink-0" />
        </button>
      </div>
      {name !== undefined ? <input type="hidden" name={name} value={value.join(',')} /> : null}
      {target
        ? createPortal(
            <div ref={list} className={LIST} data-select-popup>
              <div className="flex shrink-0 items-center gap-2 border-b border-line px-2.5">
                <Icon name="search" size={14} className="text-dim" />
                <input
                  ref={search}
                  type="text"
                  role="combobox"
                  aria-expanded="true"
                  aria-controls={listId}
                  aria-autocomplete="list"
                  aria-activedescendant={options[active] ? optionId(active) : undefined}
                  aria-label={
                    creatable ? `Search or add ${listLabel ?? noun}` : `Search ${listLabel ?? noun}`
                  }
                  placeholder={creatable ? 'Search or add…' : 'Search…'}
                  autoComplete="off"
                  spellCheck={false}
                  value={query}
                  onChange={(e) => {
                    setQuery(e.target.value);
                    setActive(0);
                  }}
                  onKeyDown={onSearchKey}
                  className="h-9 min-w-0 flex-1 bg-transparent text-ui text-fg outline-none placeholder:text-dim"
                />
              </div>
              <div
                id={listId}
                role="listbox"
                aria-multiselectable="true"
                aria-label={listLabel}
                tabIndex={-1}
                className="min-h-0 overflow-y-auto overscroll-contain p-1"
              >
                {options.length === 0 ? (
                  <div className="flex justify-center px-2.5 py-3">
                    <EmptyPill>{creatable ? 'Type to add a value' : 'No matches'}</EmptyPill>
                  </div>
                ) : null}
                {options.map((o, i) => (
                  <OptionRow
                    key={`${o.value}-${String(o.label.startsWith('Add “'))}`}
                    id={optionId(i)}
                    option={o}
                    multi
                    selected={value.includes(o.value)}
                    active={i === active}
                    onPick={() => {
                      setActive(i);
                      toggle(o.value);
                      if (o.label.startsWith('Add “')) setQuery('');
                      search.current?.focus();
                    }}
                    onHover={() => {
                      if (i !== active) setActive(i);
                    }}
                  />
                ))}
              </div>
              {value.length > 0 ? (
                <div className="flex shrink-0 items-center justify-between border-t border-line px-2.5 py-1.5 text-caption text-muted">
                  <span>{value.length} chosen</span>
                  <button
                    type="button"
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => onChange([])}
                    className="rounded-xs px-1 text-muted hover:text-fg"
                  >
                    Clear
                  </button>
                </div>
              ) : null}
            </div>,
            target,
          )
        : null}
    </>
  );
}

/** Splits a comma-separated value into the chips of a MultiSelect, and back. */
export function splitValues(text: string): string[] {
  return text
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
}

export function joinValues(values: string[]): string {
  return values.join(', ');
}
