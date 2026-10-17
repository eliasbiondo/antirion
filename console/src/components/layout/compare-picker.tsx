'use client';

import { useRef, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Calendar, type Day, parseDay, parseTime, TimeField, toDay } from '@/components/ui/date-picker';
import { Icon } from '@/components/ui/icon';
import { Popover } from '@/components/ui/overlay';
import { Tooltip } from '@/components/ui/tooltip';
import { cn } from '@/lib/cn';
import { type CompareValue, compareTitle } from '@/lib/time';
import { useResolvedRange, useTimeRange } from '@/stores/time-range';

const PICKS: CompareValue[] = [{ kind: 'previous' }, { kind: 'week' }, { kind: 'month' }];

const pad = (n: number) => String(n).padStart(2, '0');
const clock = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;

/**
 * "Compare with": the window every delta of the page is measured against. Next to the time range,
 * in the URL, and always as long as the range.
 */
export function ComparePicker() {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const compare = useTimeRange((s) => s.compare);
  const setCompare = useTimeRange((s) => s.setCompare);
  const resolved = useResolvedRange();
  const length = resolved.to.getTime() - resolved.from.getTime();
  const latest = new Date(resolved.from.getTime() - length);
  const initial = compare.kind === 'custom' ? new Date(compare.from) : latest;
  const [custom, setCustom] = useState(compare.kind === 'custom');
  const [day, setDay] = useState<Day | null>(() => toDay(initial));
  const [time, setTime] = useState(() => clock(initial));
  const [focus, setFocus] = useState<Day>(() => toDay(initial));

  const d = parseDay(day);
  const t = parseTime(time);
  const start =
    d && t ? new Date(d.getFullYear(), d.getMonth(), d.getDate(), t.h, t.m).getTime() : Number.NaN;
  const valid = Number.isFinite(start) && start <= latest.getTime();

  return (
    <div className="relative">
      <Tooltip content={open ? null : `Deltas compare with ${resolved.previous.label}`} side="bottom">
        <button
          ref={anchor}
          type="button"
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-label={`Compare with: ${compareTitle(compare)}`}
          onClick={() => {
            setOpen(!open);
            setCustom(compare.kind === 'custom');
          }}
          className="inline-flex h-7 items-center gap-1.5 rounded-control border border-line bg-canvas px-2 text-ui text-fg hover:border-line-strong hover:bg-hover"
        >
          <Icon name="compare" size={14} className="text-muted" />
          <span className="whitespace-nowrap max-xl:sr-only">
            <span className="text-muted">vs </span>
            {compareTitle(compare)}
          </span>
          <Icon name="chevronDown" size={12} className="text-dim" />
        </button>
      </Tooltip>
      <Popover
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        align="right"
        label="Compare with"
        className="w-[300px] max-w-[calc(100vw-16px)]"
      >
        <div className="border-b border-line px-3 py-2 text-caption text-muted">
          Deltas on this page compare with a window as long as the range.
        </div>
        <ul className="flex flex-col p-1">
          {[...PICKS, { kind: 'custom', from: 0 } as const].map((p) => {
            const on = p.kind === compare.kind;
            const selected = custom ? p.kind === 'custom' : on;
            return (
              <li key={p.kind}>
                <button
                  type="button"
                  aria-pressed={on}
                  onClick={() => {
                    if (p.kind === 'custom') {
                      setCustom(true);
                      return;
                    }
                    setCustom(false);
                    setCompare(p);
                    setOpen(false);
                  }}
                  className={cn(
                    'flex w-full items-center justify-between rounded-sm px-2.5 py-1.5 text-ui',
                    selected ? 'bg-accent-dim text-fg' : 'text-fg hover:bg-hover',
                  )}
                >
                  {p.kind === 'custom' ? 'Custom start…' : compareTitle(p)}
                  <span className={on ? 'text-accent' : 'invisible'}>
                    <Icon name="check" size={14} />
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
        {custom ? (
          <form
            className="flex flex-col gap-3 border-t border-line p-3"
            onSubmit={(e) => {
              e.preventDefault();
              if (!valid) return;
              setCompare({ kind: 'custom', from: start });
              setOpen(false);
            }}
          >
            <Calendar
              selected={{ from: day, to: day }}
              focus={focus}
              onFocusChange={setFocus}
              onPick={setDay}
              max={toDay(latest)}
              label="Comparison start"
            />
            <div className="flex items-center gap-2">
              <span className="text-caption text-muted">Starts at</span>
              <TimeField
                aria-label="Comparison start time"
                value={time}
                onChange={setTime}
                className="w-[64px]"
              />
            </div>
            <div className="flex items-center justify-between gap-2">
              <span className={cn('text-caption', valid ? 'text-muted' : 'text-error')}>
                {valid ? 'Same length as the range' : 'Pick a start before the range’s own'}
              </span>
              <Button size="sm" type="submit" disabled={!valid}>
                Compare
              </Button>
            </div>
          </form>
        ) : null}
      </Popover>
    </div>
  );
}
