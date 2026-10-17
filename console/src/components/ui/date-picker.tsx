'use client';

import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react';

import { cn } from '@/lib/cn';

import { IconButton } from './button';
import { Icon } from './icon';
import { Popover } from './overlay';

/** A calendar day as `YYYY-MM-DD`, in local time (what a date field holds). */
export type Day = string;

const pad = (n: number) => String(n).padStart(2, '0');

export function toDay(d: Date): Day {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export function parseDay(value: string | null | undefined): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value ?? '');
  if (!m) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return Number.isNaN(d.getTime()) ? null : d;
}

export function addDays(d: Date, n: number): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);
}

/** The same day `n` months away, clamped to the month's last day (31 Jan + 1 month = 28 Feb). */
export function addMonths(d: Date, n: number): Date {
  const target = new Date(d.getFullYear(), d.getMonth() + n, 1);
  const last = new Date(target.getFullYear(), target.getMonth() + 1, 0).getDate();
  return new Date(target.getFullYear(), target.getMonth(), Math.min(d.getDate(), last));
}

const WEEKDAYS = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];

/** The 6 × 7 days shown for a month, weeks starting on Monday. */
export function monthGrid(month: Date): Date[] {
  const firstDay = new Date(month.getFullYear(), month.getMonth(), 1);
  const offset = (firstDay.getDay() + 6) % 7;
  const start = addDays(firstDay, -offset);
  return Array.from({ length: 42 }, (_, i) => addDays(start, i));
}

const monthTitle = (d: Date) => d.toLocaleDateString('en-GB', { month: 'long', year: 'numeric' });
const dayName = (d: Date) =>
  d.toLocaleDateString('en-GB', { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' });

/** "14 Oct 2026". */
export function formatDay(value: Day): string {
  const d = parseDay(value);
  return d ? d.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' }) : value;
}

export interface CalendarProps {
  /** The chosen day, or the two ends of a chosen range. */
  selected?: Day | { from: Day | null; to: Day | null } | null;
  onPick: (day: Day) => void;
  /** The day that holds the keyboard focus (and decides the month shown). */
  focus: Day;
  onFocusChange: (day: Day) => void;
  min?: Day | undefined;
  max?: Day | undefined;
  /** Moves focus into the grid when it mounts. */
  autoFocus?: boolean;
  label?: string;
}

/**
 * A month grid (the WAI-ARIA date picker's grid): one tab stop on the focused day; arrows move by a
 * day or a week, Home and End to the week's ends, Page Up and Down by a month (with Shift, a
 * year), Enter or Space picks.
 */
export function Calendar({
  selected,
  onPick,
  focus,
  onFocusChange,
  min,
  max,
  autoFocus,
  label = 'Calendar',
}: CalendarProps) {
  const titleId = useId();
  const grid = useRef<HTMLTableElement>(null);
  const moved = useRef(Boolean(autoFocus));
  const focusDate = parseDay(focus) ?? new Date();
  const today = toDay(new Date());
  const days = monthGrid(focusDate);
  const range = selected && typeof selected === 'object' ? selected : null;
  const single = typeof selected === 'string' ? selected : null;

  useEffect(() => {
    if (!moved.current) return;
    grid.current?.querySelector<HTMLButtonElement>('button[tabindex="0"]')?.focus();
  }, [focus]);

  const outside = (day: Day) => (min !== undefined && day < min) || (max !== undefined && day > max);

  const go = (d: Date) => {
    moved.current = true;
    onFocusChange(toDay(d));
  };

  const onKey = (e: KeyboardEvent<HTMLTableElement>) => {
    const k = e.key;
    let next: Date | null = null;
    if (k === 'ArrowRight') next = addDays(focusDate, 1);
    else if (k === 'ArrowLeft') next = addDays(focusDate, -1);
    else if (k === 'ArrowDown') next = addDays(focusDate, 7);
    else if (k === 'ArrowUp') next = addDays(focusDate, -7);
    else if (k === 'Home') next = addDays(focusDate, -((focusDate.getDay() + 6) % 7));
    else if (k === 'End') next = addDays(focusDate, 6 - ((focusDate.getDay() + 6) % 7));
    else if (k === 'PageDown') next = addMonths(focusDate, e.shiftKey ? 12 : 1);
    else if (k === 'PageUp') next = addMonths(focusDate, e.shiftKey ? -12 : -1);
    if (next) {
      e.preventDefault();
      go(next);
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <IconButton
          icon="chevronLeft"
          label="Previous month"
          onClick={() => {
            moved.current = false;
            onFocusChange(toDay(addMonths(focusDate, -1)));
          }}
        />
        <h3 id={titleId} aria-live="polite" className="text-ui font-semibold text-fg">
          {monthTitle(focusDate)}
        </h3>
        <IconButton
          icon="chevronRight"
          label="Next month"
          onClick={() => {
            moved.current = false;
            onFocusChange(toDay(addMonths(focusDate, 1)));
          }}
        />
      </div>
      <table
        ref={grid}
        role="grid"
        aria-labelledby={titleId}
        data-label={label}
        onKeyDown={onKey}
        className="w-full border-collapse"
      >
        <thead>
          <tr>
            {WEEKDAYS.map((w) => (
              <th
                key={w}
                scope="col"
                abbr={w}
                className="h-7 w-8 text-center text-caption font-medium text-dim"
              >
                {w.slice(0, 2)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {Array.from({ length: 6 }, (_, week) => (
            <tr key={week}>
              {days.slice(week * 7, week * 7 + 7).map((d) => {
                const day = toDay(d);
                const inMonth = d.getMonth() === focusDate.getMonth();
                const isFocus = day === focus;
                const from = range?.from ?? null;
                const to = range?.to ?? null;
                const isEnd = single === day || from === day || to === day;
                const between = from !== null && to !== null && day > from && day < to;
                const disabled = outside(day);
                return (
                  <td
                    key={day}
                    role="gridcell"
                    aria-selected={isEnd || between}
                    className={cn(
                      'p-0 text-center',
                      between && 'bg-accent-dim',
                      from === day && to !== null && to !== from && 'rounded-l-sm bg-accent-dim',
                      to === day && from !== null && to !== from && 'rounded-r-sm bg-accent-dim',
                    )}
                  >
                    <button
                      type="button"
                      tabIndex={isFocus ? 0 : -1}
                      aria-current={day === today ? 'date' : undefined}
                      aria-label={dayName(d)}
                      disabled={disabled}
                      onClick={() => {
                        moved.current = true;
                        onFocusChange(day);
                        onPick(day);
                      }}
                      className={cn(
                        'mono relative h-8 w-full min-w-8 rounded-sm text-caption tabular-nums transition-colors',
                        'focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
                        'disabled:cursor-not-allowed disabled:opacity-35',
                        isEnd
                          ? 'bg-accent font-semibold text-accent-fg'
                          : inMonth
                            ? 'text-fg hover:bg-hover'
                            : 'text-dim hover:bg-hover',
                        day === today && !isEnd && 'font-semibold text-accent',
                      )}
                    >
                      {d.getDate()}
                    </button>
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export interface DatePickerProps {
  value: Day;
  onChange: (value: Day) => void;
  placeholder?: string;
  min?: Day | undefined;
  max?: Day | undefined;
  /** Offers "Clear" (back to no date). */
  clearable?: boolean;
  id?: string;
  'aria-label'?: string;
  'aria-describedby'?: string;
  'aria-invalid'?: boolean;
  'aria-required'?: boolean;
  disabled?: boolean;
  className?: string;
}

/**
 * A date field: a button with the date that opens a calendar in a popover (the WAI-ARIA date
 * picker dialog). Focus starts on the chosen day (or today); picking closes it and focus returns
 * to the field; Escape closes without a change.
 */
export function DatePicker({
  value,
  onChange,
  placeholder = 'Pick a date',
  min,
  max,
  clearable = false,
  id,
  disabled,
  className,
  ...aria
}: DatePickerProps) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [focus, setFocus] = useState<Day>(() => value || toDay(new Date()));
  const close = () => {
    setOpen(false);
    anchor.current?.focus();
  };
  return (
    <div className="relative">
      <button
        ref={anchor}
        id={id}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={aria['aria-label']}
        aria-describedby={aria['aria-describedby']}
        data-invalid={aria['aria-invalid'] || undefined}
        disabled={disabled}
        onClick={() => {
          setFocus(value || toDay(new Date()));
          setOpen(!open);
        }}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && !open) {
            e.preventDefault();
            setFocus(value || toDay(new Date()));
            setOpen(true);
          }
        }}
        className={cn(
          'flex h-8 w-full min-w-36 items-center gap-2 rounded-control border border-line bg-canvas px-2.5 text-left text-ui',
          'hover:border-line-strong focus:border-accent focus:ring-2 focus:ring-accent-dim focus:outline-none focus-visible:outline-none',
          'disabled:cursor-not-allowed disabled:opacity-60 aria-expanded:border-accent data-invalid:border-error',
          className,
        )}
      >
        <Icon name="calendar" size={14} className="shrink-0 text-dim" />
        <span className={cn('min-w-0 flex-1 truncate', value ? 'text-fg' : 'text-dim')}>
          {value ? formatDay(value) : placeholder}
        </span>
      </button>
      <Popover
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        label="Choose a date"
        className="w-[272px] p-3"
      >
        <Calendar
          selected={value || null}
          focus={focus}
          onFocusChange={setFocus}
          min={min}
          max={max}
          autoFocus
          label="Choose a date"
          onPick={(day) => {
            onChange(day);
            close();
          }}
        />
        <div className="mt-2 flex items-center justify-between border-t border-line pt-2">
          <button
            type="button"
            onClick={() => {
              const t = toDay(new Date());
              if ((min === undefined || t >= min) && (max === undefined || t <= max)) {
                onChange(t);
                close();
              } else setFocus(t);
            }}
            className="rounded-xs px-1.5 py-0.5 text-caption text-accent hover:bg-hover"
          >
            Today
          </button>
          {clearable && value ? (
            <button
              type="button"
              onClick={() => {
                onChange('');
                close();
              }}
              className="rounded-xs px-1.5 py-0.5 text-caption text-muted hover:bg-hover hover:text-fg"
            >
              Clear
            </button>
          ) : null}
        </div>
      </Popover>
    </div>
  );
}

/** `HH:mm` on a 24-hour clock, or null. */
export function parseTime(value: string): { h: number; m: number } | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(value.trim());
  if (!m) return null;
  const h = Number(m[1]);
  const min = Number(m[2]);
  return h < 24 && min < 60 ? { h, m: min } : null;
}

export interface TimeFieldProps {
  value: string;
  onChange: (value: string) => void;
  id?: string;
  'aria-label'?: string;
  'aria-invalid'?: boolean;
  className?: string;
}

/**
 * A 24-hour time typed as `HH:mm` (a text field, not the browser's time control); the arrow keys
 * step the minutes by 15 (with Shift, the hours by one).
 */
export function TimeField({ value, onChange, className, 'aria-invalid': invalid, ...rest }: TimeFieldProps) {
  const step = (minutes: number) => {
    const t = parseTime(value) ?? { h: 0, m: 0 };
    const total = (((t.h * 60 + t.m + minutes) % 1440) + 1440) % 1440;
    onChange(`${pad(Math.floor(total / 60))}:${pad(total % 60)}`);
  };
  return (
    <input
      type="text"
      inputMode="numeric"
      autoComplete="off"
      spellCheck={false}
      placeholder="HH:MM"
      maxLength={5}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
          e.preventDefault();
          const sign = e.key === 'ArrowUp' ? 1 : -1;
          step(sign * (e.shiftKey ? 60 : 15));
        }
      }}
      aria-invalid={invalid ?? (parseTime(value) === null ? true : undefined)}
      {...rest}
      className={cn(
        'mono h-8 w-[72px] rounded-control border border-line bg-canvas px-2 text-ui text-fg placeholder:text-dim',
        'hover:border-line-strong focus:border-accent focus:ring-2 focus:ring-accent-dim focus:outline-none aria-[invalid=true]:border-error',
        className,
      )}
    />
  );
}
