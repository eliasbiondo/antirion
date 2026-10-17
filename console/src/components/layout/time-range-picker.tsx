'use client';

import { useRef, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Calendar, type Day, parseDay, parseTime, TimeField, toDay } from '@/components/ui/date-picker';
import { Select } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { Popover } from '@/components/ui/overlay';
import { Tooltip } from '@/components/ui/tooltip';
import { cn } from '@/lib/cn';
import {
  allowedIntervals,
  defaultInterval,
  type Interval,
  presetLabel,
  PRESETS,
  resolveRange,
  zoneLabel,
} from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useResolvedRange, useTimeRange } from '@/stores/time-range';

const JUMPS: { label: string; minutes: number }[] = [
  { label: '−1h', minutes: 60 },
  { label: '−24h', minutes: 1440 },
  { label: '−3d', minutes: 4320 },
  { label: '−14d', minutes: 20_160 },
];

/** "14 Oct": the year is in the calendar's title. */
const shortDay = (day: Day) =>
  parseDay(day)?.toLocaleDateString('en-GB', { day: 'numeric', month: 'short' }) ?? day;
const pad = (n: number) => String(n).padStart(2, '0');
const clock = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;

/** A local day and `HH:mm` as a time in ms, or NaN. */
function at(day: Day | null, time: string): number {
  const d = parseDay(day);
  const t = parseTime(time);
  if (!d || !t) return Number.NaN;
  return new Date(d.getFullYear(), d.getMonth(), d.getDate(), t.h, t.m).getTime();
}

/** The time range: presets, a custom range with quick jumps, the granularity and the time zone. */
export function TimeRangePicker() {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const range = useTimeRange((s) => s.range);
  const interval = useTimeRange((s) => s.interval);
  const setRange = useTimeRange((s) => s.setRange);
  const setInterval = useTimeRange((s) => s.setInterval);
  const resolved = useResolvedRange();
  const timeZone = usePreferences((s) => s.timeZone);
  const [mode, setMode] = useState<'presets' | 'custom'>('presets');
  const [fromDay, setFromDay] = useState<Day | null>(() => toDay(resolved.from));
  const [toDayValue, setToDay] = useState<Day | null>(() => toDay(resolved.to));
  const [fromTime, setFromTime] = useState(() => clock(resolved.from));
  const [toTime, setToTime] = useState(() => clock(resolved.to));
  const [focus, setFocus] = useState<Day>(() => toDay(resolved.to));
  const retention = useTimeRange((s) => s.retention);
  // The oldest time the page still has data for; the picker offers nothing before it.
  const [openedAt, setOpenedAt] = useState(() => Date.now());
  const earliest = retention ? openedAt - retention.seconds * 1000 : null;
  const kept = retention ? `Requests are kept for ${retention.label}` : null;

  const fromMs = at(fromDay, fromTime);
  const toMs = at(toDayValue, toTime);
  const tooOld = earliest !== null && Number.isFinite(fromMs) && fromMs < earliest - 60_000;
  const valid = Number.isFinite(fromMs) && Number.isFinite(toMs) && toMs > fromMs && !tooOld;

  function jump(minutes: number) {
    const end = new Date();
    const start = new Date(end.getTime() - minutes * 60_000);
    setFromDay(toDay(start));
    setFromTime(clock(start));
    setToDay(toDay(end));
    setToTime(clock(end));
    setFocus(toDay(end));
  }

  /** The first pick starts a new range; the second ends it (either order). */
  function pick(day: Day) {
    if (fromDay === null || toDayValue !== null) {
      setFromDay(day);
      setToDay(null);
    } else if (day < fromDay) {
      setToDay(fromDay);
      setFromDay(day);
    } else setToDay(day);
  }

  return (
    <div className="relative">
      <Tooltip content={open ? null : `Granularity: ${interval}`} side="bottom">
        <button
          ref={anchor}
          type="button"
          aria-haspopup="dialog"
          aria-expanded={open}
          onClick={() => {
            setOpen(!open);
            setOpenedAt(Date.now());
            setMode(range.kind === 'custom' ? 'custom' : 'presets');
          }}
          className="inline-flex h-7 items-center gap-1.5 rounded-control border border-line bg-canvas px-2 text-ui text-fg hover:border-line-strong hover:bg-hover"
        >
          <Icon name="clock" size={14} className="text-muted" />
          <span className="whitespace-nowrap max-sm:sr-only">{resolved.label}</span>
          <Icon name="chevronDown" size={12} className="text-dim" />
        </button>
      </Tooltip>
      <Popover
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        align="right"
        label="Time range"
        className="w-[320px] max-w-[calc(100vw-16px)]"
      >
        <div role="tablist" aria-label="Range kind" className="flex border-b border-line">
          {(['presets', 'custom'] as const).map((m) => (
            <button
              key={m}
              role="tab"
              type="button"
              aria-selected={mode === m}
              onClick={() => setMode(m)}
              className={cn(
                'flex-1 border-b-2 py-2 text-ui',
                mode === m
                  ? 'border-accent font-semibold text-fg'
                  : 'border-transparent text-muted hover:text-fg',
              )}
            >
              {m === 'presets' ? 'Presets' : 'Custom'}
            </button>
          ))}
        </div>
        {mode === 'presets' ? (
          <ul className="flex flex-col p-1">
            {PRESETS.map((p) => {
              const on = range.kind === 'preset' && range.preset === p;
              const beyond =
                retention !== null &&
                resolveRange({ kind: 'preset', preset: p }, new Date(openedAt)).seconds > retention.seconds;
              return (
                <li key={p}>
                  <Tooltip content={beyond ? kept : null} side="bottom">
                    <button
                      type="button"
                      aria-pressed={on}
                      disabled={beyond}
                      onClick={() => {
                        setRange({ kind: 'preset', preset: p });
                        setOpen(false);
                      }}
                      className={cn(
                        'flex w-full items-center justify-between rounded-sm px-2.5 py-1.5 text-ui',
                        'disabled:cursor-not-allowed disabled:text-dim disabled:hover:bg-transparent',
                        on ? 'bg-accent-dim text-fg' : 'text-fg hover:bg-hover',
                      )}
                    >
                      {presetLabel(p)}
                      <span className="flex items-center gap-2">
                        <span className="mono text-caption text-dim">
                          {defaultInterval({ kind: 'preset', preset: p })}
                        </span>
                        <span className={on ? 'text-accent' : 'invisible'}>
                          <Icon name="check" size={14} />
                        </span>
                      </span>
                      {beyond ? <span className="sr-only">. {kept}</span> : null}
                    </button>
                  </Tooltip>
                </li>
              );
            })}
            {kept ? <li className="px-2.5 pt-1 pb-1.5 text-caption text-muted">{kept}.</li> : null}
          </ul>
        ) : (
          <form
            className="flex flex-col gap-3 p-3"
            onSubmit={(e) => {
              e.preventDefault();
              if (!valid) return;
              setRange({ kind: 'custom', from: fromMs, to: toMs });
              setOpen(false);
            }}
          >
            <Calendar
              selected={{ from: fromDay, to: toDayValue }}
              focus={focus}
              onFocusChange={setFocus}
              onPick={pick}
              max={toDay(new Date())}
              min={earliest !== null ? toDay(new Date(earliest)) : undefined}
              label="Custom range"
            />
            <div className="grid grid-cols-2 gap-2">
              <div className="flex flex-col gap-1">
                <span className="text-caption text-muted">From</span>
                <div className="flex items-center gap-1.5">
                  <span className="min-w-0 flex-1 truncate text-caption text-fg">
                    {fromDay ? shortDay(fromDay) : '—'}
                  </span>
                  <TimeField
                    aria-label="From time"
                    value={fromTime}
                    onChange={setFromTime}
                    className="w-[64px]"
                  />
                </div>
              </div>
              <div className="flex flex-col gap-1">
                <span className="text-caption text-muted">To</span>
                <div className="flex items-center gap-1.5">
                  <span className="min-w-0 flex-1 truncate text-caption text-fg">
                    {toDayValue ? shortDay(toDayValue) : 'Pick a day'}
                  </span>
                  <TimeField aria-label="To time" value={toTime} onChange={setToTime} className="w-[64px]" />
                </div>
              </div>
            </div>
            <div className="flex items-center gap-1.5">
              <span className="text-caption text-muted">Jump to</span>
              {JUMPS.map((j) => (
                <button
                  key={j.label}
                  type="button"
                  onClick={() => jump(j.minutes)}
                  className="mono h-7 rounded-control border border-line px-1.5 text-caption text-fg hover:bg-hover"
                >
                  {j.label}
                </button>
              ))}
            </div>
            <div className="flex items-center justify-between">
              <span className={cn('text-caption', valid ? 'text-muted' : 'text-error')}>
                {valid
                  ? `${((toMs - fromMs) / 3_600_000).toFixed(1)} h range`
                  : toDayValue === null
                    ? 'Pick the last day'
                    : tooOld
                      ? `${kept ?? ''}; start later`
                      : 'End must be after start'}
              </span>
              <div className="flex gap-1.5">
                <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
                  Cancel
                </Button>
                <Button size="sm" type="submit" disabled={!valid}>
                  Apply range
                </Button>
              </div>
            </div>
          </form>
        )}
        <div className="flex items-center justify-between gap-2 border-t border-line px-3 py-2">
          <div className="flex items-center gap-2 text-caption text-muted">
            <span>Granularity</span>
            <div className="w-20">
              <Select
                size="sm"
                mono
                aria-label="Granularity"
                value={interval}
                onChange={(v) => setInterval(v as Interval)}
                options={allowedIntervals(resolved.seconds).map((i) => ({ value: i, label: i }))}
              />
            </div>
          </div>
          <span className="text-caption text-dim">Times in {zoneLabel(timeZone)}</span>
        </div>
      </Popover>
    </div>
  );
}
