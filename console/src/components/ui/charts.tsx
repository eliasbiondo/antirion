'use client';

import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from 'react';

import { cn } from '@/lib/cn';

import { noteCountFits, noteTitle } from './chart-notes';
import { EmptyPill } from './states';

export type ChartTone = 'accent' | 'success' | 'warn' | 'error' | 'info' | 'muted';

const STROKE: Record<ChartTone, string> = {
  accent: 'stroke-accent',
  success: 'stroke-success',
  warn: 'stroke-warn',
  error: 'stroke-error',
  info: 'stroke-info',
  muted: 'stroke-dim',
};

const FILL: Record<ChartTone, string> = {
  accent: 'fill-accent',
  success: 'fill-success',
  warn: 'fill-warn',
  error: 'fill-error',
  info: 'fill-info',
  muted: 'fill-dim',
};

/** Gradient stops take their colour from a class, so no style attribute is emitted. */
const STOP: Record<ChartTone, string> = {
  accent: '[stop-color:var(--ant-accent)]',
  success: '[stop-color:var(--ant-success)]',
  warn: '[stop-color:var(--ant-warn)]',
  error: '[stop-color:var(--ant-error)]',
  info: '[stop-color:var(--ant-info)]',
  muted: '[stop-color:var(--ant-fg-dim)]',
};

const BG: Record<ChartTone, string> = {
  accent: 'bg-accent',
  success: 'bg-success',
  warn: 'bg-warn',
  error: 'bg-error',
  info: 'bg-info',
  muted: 'bg-dim',
};

function finite(values: (number | null | undefined)[]): number[] {
  return values.map((v) => (typeof v === 'number' && Number.isFinite(v) ? v : 0));
}

/**
 * The rendered width of an element, so a chart draws in real pixels and its text stays readable.
 * Returns the width and a callback ref for the element (which may mount after the first render).
 */
export function useWidth(fallback: number): [number, (el: HTMLDivElement | null) => void] {
  const [node, setNode] = useState<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(fallback);
  useLayoutEffect(() => {
    if (!node) return;
    const measure = () => {
      const w = Math.round(node.getBoundingClientRect().width);
      if (w > 0) setWidth(w);
    };
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(measure);
    ro.observe(node);
    return () => ro.disconnect();
  }, [node]);
  return [width, setNode];
}

/** A rounded axis: the top of the scale and evenly spaced ticks (0, 25, 50, 75, 100). */
export function niceScale(peak: number, count = 4): { top: number; ticks: number[] } {
  if (!(peak > 0)) return { top: 1, ticks: Array.from({ length: count + 1 }, (_, i) => i / count) };
  const raw = peak / count;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * magnitude).find((s) => s >= raw) ?? 10 * magnitude;
  const top = step * count;
  return { top, ticks: Array.from({ length: count + 1 }, (_, i) => i * step) };
}

/**
 * A rounded axis whose tick labels read differently: when the formatter rounds two ticks to the same
 * text (a peak of 1 shown as whole numbers), the ticks fall back to whole steps.
 */
export function readableScale(
  peak: number,
  format: (v: number) => string,
  count = 4,
): { top: number; ticks: number[]; tickText: string[] } {
  const nice = niceScale(peak, count);
  const text = nice.ticks.map(format);
  if (new Set(text).size === text.length) return { ...nice, tickText: text };
  const step = Math.max(1, Math.ceil(nice.top / count));
  const top = Math.max(step, Math.ceil(peak / step) * step);
  const ticks = Array.from({ length: Math.round(top / step) + 1 }, (_, i) => i * step);
  return { top, ticks, tickText: ticks.map(format) };
}

function linePath(values: number[], x: (i: number) => number, y: (v: number) => number): string {
  return values.map((v, i) => `${i === 0 ? 'M' : 'L'}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
}

export interface SparklineProps {
  values: (number | null | undefined)[];
  tone?: ChartTone;
  area?: boolean;
  height?: number;
  label: string;
  className?: string;
}

/**
 * A small trend line for KPI tiles, with a soft gradient under it. An all-zero series is drawn as a
 * flat dashed baseline, so "nothing yet" does not look like a broken chart.
 */
export function Sparkline({
  values,
  tone = 'accent',
  area = true,
  height = 22,
  label,
  className,
}: SparklineProps) {
  const id = useId().replace(/[^a-zA-Z0-9]/g, '');
  const data = finite(values);
  if (data.length < 2) {
    return <div className={cn('h-[22px]', className)} aria-hidden="true" />;
  }
  const max = Math.max(...data);
  const min = Math.min(...data, 0);
  const flat = max === min;
  const span = max - min || 1;
  const step = 100 / (data.length - 1);
  const y = (v: number) => height - 1 - ((v - min) / span) * (height - 3);
  const line = linePath(data, (i) => i * step, y);
  return (
    <svg
      viewBox={`0 0 100 ${height}`}
      preserveAspectRatio="none"
      className={cn('block h-[22px] w-full overflow-visible', className)}
      role="img"
      aria-label={label}
    >
      {flat ? (
        <line
          x1="0"
          x2="100"
          y1={height - 1}
          y2={height - 1}
          className="stroke-dim"
          strokeWidth={1}
          strokeDasharray="3 3"
          vectorEffect="non-scaling-stroke"
        />
      ) : (
        <>
          {area ? (
            <>
              <defs>
                <linearGradient id={`spark-${id}`} x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" className={STOP[tone]} stopOpacity={0.28} />
                  <stop offset="100%" className={STOP[tone]} stopOpacity={0} />
                </linearGradient>
              </defs>
              <path d={`${line} L100,${height} L0,${height} Z`} fill={`url(#spark-${id})`} />
            </>
          ) : null}
          <path
            d={line}
            fill="none"
            className={STROKE[tone]}
            strokeWidth={1.25}
            strokeLinejoin="round"
            vectorEffect="non-scaling-stroke"
          />
        </>
      )}
    </svg>
  );
}

export interface Series {
  name: string;
  values: (number | null | undefined)[];
  tone: ChartTone;
  dashed?: boolean;
}

export interface ChartAnnotation {
  /** The bucket the event falls in. */
  index: number;
  /** One short line ("Configuration 45 applied"). */
  label: string;
  tone: ChartTone;
}

export interface TimeChartProps {
  series: Series[];
  /** One label per bucket (formatted times). */
  labels: string[];
  kind: 'area' | 'line' | 'bars' | 'stacked-bars' | 'stacked-area';
  height?: number;
  formatValue?: (v: number) => string;
  title: string;
  /** A horizontal reference line (a budget, a threshold). */
  reference?: { value: number; label: string } | undefined;
  /** A vertical marker at a bucket index ("today"). */
  marker?: { index: number; label: string } | undefined;
  /**
   * Events on the time axis (a configuration change, an alert): a flag at the top of the plot at
   * their bucket, listed in the hover card of that bucket and in the hidden table.
   */
  annotations?: ChartAnnotation[] | undefined;
  /**
   * Shown over an empty frame when every value is zero ("No requests in this range"); without it,
   * zeros are drawn as a flat line.
   */
  emptyLabel?: string;
  /**
   * Line and area charts only: a missing value (`null`) leaves a gap instead of dropping to zero,
   * so an actual series can stop where a projection starts.
   */
  gaps?: boolean;
  /** The room each axis label needs, in pixels (default 76); wider for long labels ("Wed 05:05"). */
  labelWidth?: number;
  /**
   * A line on its own scale over the series (dollars saved over request bars), with its axis on the
   * right; listed in the hover card and the hidden table like a series.
   */
  overlay?: ChartOverlay | undefined;
  /**
   * Short horizontal marks at the right edge of the plot, one per value (each team's budget beside
   * its line), in the series' tone; listed in the hidden table. They don't stretch the scale: a mark
   * above the plot's top is left out of the drawing.
   */
  ticks?: { value: number; label: string; tone: ChartTone }[] | undefined;
  className?: string;
}

export interface ChartOverlay {
  name: string;
  values: (number | null | undefined)[];
  tone: ChartTone;
  format: (v: number) => string;
}

/** The width of an axis label in the 11px mono face, close enough to keep labels apart. */
const TICK_CHAR_W = 6.7;
const TICK_GAP = 8;

/**
 * The x-axis labels to draw: every `every`-th one, less any that would touch the one before (the
 * first is drawn from its point to the right on line charts, so it needs more room).
 */
function axisLabels(
  labels: string[],
  every: number,
  x: (i: number) => number,
  startFirst: boolean,
): { i: number; text: string; anchor: 'start' | 'middle' }[] {
  const out: { i: number; text: string; anchor: 'start' | 'middle' }[] = [];
  let right = -Infinity;
  labels.forEach((text, i) => {
    if (i % every !== 0) return;
    const w = text.length * TICK_CHAR_W;
    const anchor = i === 0 && startFirst ? 'start' : 'middle';
    const left = anchor === 'start' ? x(i) : x(i) - w / 2;
    if (left < right + TICK_GAP) return;
    out.push({ i, text, anchor });
    right = left + w;
  });
  return out;
}

const PAD_T = 10;
const PAD_R = 8;
const PAD_B = 24;
const TICK_FONT = 11;

/**
 * Charts over time buckets in the prototype's style: thin lines over a soft gradient, quiet grid
 * lines, 11px axis labels drawn at their real size, a hover crosshair with every series' value, an
 * optional reference line and marker, and a visually hidden table of the same data.
 */
export function TimeChart({
  series,
  labels,
  kind,
  height = 180,
  formatValue = (v) => String(Math.round(v)),
  title,
  reference,
  marker,
  annotations,
  emptyLabel,
  gaps = false,
  labelWidth = 76,
  overlay,
  ticks: valueTicks,
  className,
}: TimeChartProps) {
  const tip = useRef<HTMLDivElement>(null);
  const [width, wrap] = useWidth(600);
  const gid = useId().replace(/[^a-zA-Z0-9]/g, '');
  const [hover, setHover] = useState<number | null>(null);

  const n = labels.length;
  const stacked = kind === 'stacked-bars' || kind === 'stacked-area';
  const barKind = kind === 'bars' || kind === 'stacked-bars';
  const cleaned = series.map((s) => ({
    ...s,
    data: finite(s.values).slice(0, n),
    present: s.values.slice(0, n).map((v) => typeof v === 'number' && Number.isFinite(v)),
  }));
  const gapped = gaps && (kind === 'line' || kind === 'area');
  const totals = Array.from({ length: n }, (_, i) => cleaned.reduce((acc, s) => acc + (s.data[i] ?? 0), 0));
  const peak = stacked ? Math.max(0, ...totals) : Math.max(0, ...cleaned.flatMap((s) => s.data));
  const empty = Boolean(emptyLabel) && peak === 0;
  const { top, ticks, tickText } = readableScale(Math.max(peak, reference?.value ?? 0), formatValue);
  const padL = Math.max(28, Math.ceil(Math.max(...tickText.map((t) => t.length)) * 6.8) + 10);
  const over = overlay ? finite(overlay.values).slice(0, n) : null;
  const overScale = overlay && over ? readableScale(Math.max(0, ...over), overlay.format) : null;
  const padR = overScale
    ? Math.max(PAD_R, Math.ceil(Math.max(...overScale.tickText.map((t) => t.length)) * 6.8) + 10)
    : PAD_R;
  const plotW = Math.max(10, width - padL - padR);
  const plotH = height - PAD_T - PAD_B;
  const slot = plotW / Math.max(n, 1);
  const x = (i: number) =>
    barKind ? padL + i * slot + slot / 2 : padL + (n > 1 ? (i / (n - 1)) * plotW : plotW / 2);
  const y = (v: number) => PAD_T + plotH - (v / top) * plotH;
  const yOver = (v: number) => PAD_T + plotH - (v / (overScale?.top ?? 1)) * plotH;
  const barW = Math.max(1, Math.min(18, slot * 0.72));
  const maxLabels = Math.max(2, Math.floor(plotW / labelWidth));
  const labelEvery = Math.max(1, Math.ceil(n / maxLabels));
  const shownLabels = axisLabels(labels, labelEvery, x, !barKind);
  const noteMap = new Map<number, ChartAnnotation[]>();
  for (const a of annotations ?? [])
    if (a.index >= 0 && a.index < n) noteMap.set(a.index, [...(noteMap.get(a.index) ?? []), a]);
  const notes = empty ? [] : [...noteMap.entries()].sort(([a], [b]) => a - b);
  const countFits = noteCountFits(
    notes.map(([i]) => x(i)),
    notes.map(([, list]) => list.length),
    width - padR,
  );

  useEffect(() => {
    const el = tip.current;
    if (!el || hover === null) return;
    const px = x(hover);
    const flip = px + 16 + el.offsetWidth > width;
    el.style.left = `${flip ? px - 12 - el.offsetWidth : px + 12}px`;
    el.style.top = `${PAD_T}px`;
  });

  let body: ReactNode = null;
  if (empty) {
    body = null;
  } else if (barKind) {
    body = Array.from({ length: n }, (_, i) => {
      let base = 0;
      return (
        <g key={i} className={hover !== null && hover !== i ? 'opacity-60' : undefined}>
          {cleaned.map((s, si) => {
            const v = s.data[i] ?? 0;
            if (kind === 'bars' && si > 0) return null;
            const yTop = y(base + v);
            const h = y(base) - yTop;
            base += kind === 'stacked-bars' ? v : 0;
            if (h <= 0) return null;
            return (
              <rect
                key={s.name}
                x={x(i) - barW / 2}
                y={yTop}
                width={barW}
                height={h}
                className={FILL[s.tone]}
                fillOpacity={si === 0 ? 0.85 : 1}
              />
            );
          })}
        </g>
      );
    });
  } else if (kind === 'stacked-area') {
    const acc = new Array<number>(n).fill(0);
    body = cleaned.map((s) => {
      const lower = [...acc];
      for (let i = 0; i < n; i += 1) acc[i] = (acc[i] ?? 0) + (s.data[i] ?? 0);
      const upper = [...acc];
      const topLine = linePath(upper, x, y);
      const bottom = lower
        .map((v, i) => [i, v] as const)
        .reverse()
        .map(([i, v]) => `L${x(i).toFixed(1)},${y(v).toFixed(1)}`)
        .join(' ');
      return (
        <g key={s.name}>
          <path d={`${topLine} ${bottom} Z`} className={FILL[s.tone]} fillOpacity={0.35} />
          <path
            d={topLine}
            fill="none"
            className={STROKE[s.tone]}
            strokeWidth={1.25}
            strokeLinejoin="round"
          />
        </g>
      );
    });
  } else {
    body = cleaned.map((s, si) => {
      // Runs of consecutive indices to draw; without gaps, the whole series is one run.
      const runs: number[][] = [];
      if (gapped) {
        let run: number[] = [];
        s.present.forEach((ok, i) => {
          if (ok) run.push(i);
          else if (run.length) {
            runs.push(run);
            run = [];
          }
        });
        if (run.length) runs.push(run);
      } else {
        runs.push(s.data.map((_, i) => i));
      }
      return (
        <g key={s.name}>
          {kind === 'area' ? (
            <defs>
              <linearGradient id={`area-${gid}-${si}`} x1="0" x2="0" y1="0" y2="1">
                <stop offset="0%" className={STOP[s.tone]} stopOpacity={si === 0 ? 0.32 : 0.22} />
                <stop offset="100%" className={STOP[s.tone]} stopOpacity={0.01} />
              </linearGradient>
            </defs>
          ) : null}
          {runs.map((run) => {
            const first = run[0] ?? 0;
            const last = run[run.length - 1] ?? 0;
            const d = run
              .map((i, k) => `${k === 0 ? 'M' : 'L'}${x(i).toFixed(1)},${y(s.data[i] ?? 0).toFixed(1)}`)
              .join(' ');
            return (
              <g key={first}>
                {kind === 'area' ? (
                  <path
                    d={`${d} L${x(last).toFixed(1)},${y(0).toFixed(1)} L${x(first).toFixed(1)},${y(0).toFixed(1)} Z`}
                    fill={`url(#area-${gid}-${si})`}
                  />
                ) : null}
                <path
                  d={d}
                  fill="none"
                  className={STROKE[s.tone]}
                  strokeWidth={si === 0 ? 1.5 : 1.25}
                  strokeLinejoin="round"
                  strokeDasharray={s.dashed ? '4 3' : undefined}
                />
              </g>
            );
          })}
        </g>
      );
    });
  }

  if (n === 0) {
    return (
      <div
        className={cn('flex items-center justify-center', className)}
        ref={(el) => {
          if (el) el.style.height = `${height}px`;
        }}
      >
        <EmptyPill>{emptyLabel ?? 'No data in this range'}</EmptyPill>
      </div>
    );
  }

  function onMove(e: React.MouseEvent<SVGSVGElement>) {
    if (empty) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const mx = ((e.clientX - rect.left) / rect.width) * width;
    const i = barKind ? Math.floor((mx - padL) / slot) : Math.round(((mx - padL) / plotW) * (n - 1));
    setHover(i < 0 || i >= n ? null : i);
  }

  return (
    <figure className={cn('relative m-0 min-w-0', className)}>
      <div ref={wrap} className="relative w-full">
        <svg
          width={width}
          height={height}
          viewBox={`0 0 ${width} ${height}`}
          className="block max-w-full overflow-visible"
          role="img"
          aria-label={title}
          onMouseMove={onMove}
          onMouseLeave={() => setHover(null)}
        >
          {ticks.map((t, i) => (
            <g key={t}>
              <line
                x1={padL}
                x2={width - padR}
                y1={Math.round(y(t)) + 0.5}
                y2={Math.round(y(t)) + 0.5}
                className={i === 0 ? 'stroke-line' : 'stroke-grid'}
                strokeWidth={1}
              />
              <text x={padL - 8} y={y(t) + 4} textAnchor="end" className="mono fill-dim" fontSize={TICK_FONT}>
                {tickText[i]}
              </text>
            </g>
          ))}
          {body}
          {overScale && over && overlay && !empty ? (
            <g>
              {overScale.ticks.map((t, i) =>
                i === 0 ? null : (
                  <text
                    key={`o${t}`}
                    x={width - padR + 8}
                    y={yOver(t) + 4}
                    textAnchor="start"
                    className={cn('mono', FILL[overlay.tone])}
                    fontSize={TICK_FONT}
                  >
                    {overScale.tickText[i]}
                  </text>
                ),
              )}
              <path
                d={linePath(over, x, yOver)}
                fill="none"
                className={STROKE[overlay.tone]}
                strokeWidth={1.75}
                strokeLinejoin="round"
              />
            </g>
          ) : null}
          {reference && !empty ? (
            <g>
              <line
                x1={padL}
                x2={width - padR}
                y1={y(reference.value)}
                y2={y(reference.value)}
                className="stroke-muted"
                strokeDasharray="4 3"
              />
              <text
                x={width - padR - 4}
                y={y(reference.value) - 5}
                textAnchor="end"
                className="fill-muted"
                fontSize={TICK_FONT}
              >
                {reference.label}
              </text>
            </g>
          ) : null}
          {!empty
            ? (valueTicks ?? [])
                .filter((t) => t.value <= top)
                .map((t) => (
                  <line
                    key={`tick-${t.label}`}
                    x1={width - padR - 14}
                    x2={width - padR}
                    y1={y(t.value)}
                    y2={y(t.value)}
                    className={STROKE[t.tone]}
                    strokeWidth={2}
                  >
                    <title>{t.label}</title>
                  </line>
                ))
            : null}
          {marker && marker.index >= 0 && marker.index < n ? (
            <g>
              <line
                x1={x(marker.index)}
                x2={x(marker.index)}
                y1={PAD_T}
                y2={PAD_T + plotH}
                className="stroke-muted"
                strokeDasharray="2 3"
              />
              <text
                x={x(marker.index) + 4}
                y={notes.some(([i]) => Math.abs(x(i) - x(marker.index)) < 40) ? PAD_T + 22 : PAD_T + 10}
                className="fill-muted"
                fontSize={TICK_FONT}
              >
                {marker.label}
              </text>
            </g>
          ) : null}
          {notes.map(([i, list], k) => (
            <g key={`note-${i}`} aria-hidden="true">
              <title>{noteTitle(list.map((a) => a.label))}</title>
              <line
                x1={x(i)}
                x2={x(i)}
                y1={PAD_T + 4}
                y2={PAD_T + plotH}
                className={STROKE[list[0]?.tone ?? 'muted']}
                strokeOpacity={0.45}
                strokeDasharray="1 3"
              />
              <rect
                x={x(i) - 3.5}
                y={PAD_T - 3}
                width={7}
                height={7}
                rx={1.5}
                transform={`rotate(45 ${x(i)} ${PAD_T + 0.5})`}
                className={FILL[list[0]?.tone ?? 'muted']}
              />
              {countFits[k] ? (
                <text x={x(i) + 8} y={PAD_T + 3} className="mono fill-muted" fontSize={10}>
                  {list.length}
                </text>
              ) : null}
            </g>
          ))}
          {hover !== null && !empty ? (
            <g aria-hidden="true">
              <line
                x1={x(hover)}
                x2={x(hover)}
                y1={PAD_T}
                y2={PAD_T + plotH}
                className="stroke-muted"
                strokeWidth={1}
                strokeDasharray="2 2"
              />
              {!barKind
                ? cleaned.map((s, si) => {
                    const v = stacked
                      ? cleaned.slice(0, si + 1).reduce((a, c) => a + (c.data[hover] ?? 0), 0)
                      : (s.data[hover] ?? 0);
                    if (gapped && !s.present[hover]) return null;
                    return (
                      <circle
                        key={s.name}
                        cx={x(hover)}
                        cy={y(v)}
                        r={3.5}
                        className={cn(FILL[s.tone], 'stroke-raised')}
                        strokeWidth={2}
                      />
                    );
                  })
                : null}
              {over && overlay ? (
                <circle
                  cx={x(hover)}
                  cy={yOver(over[hover] ?? 0)}
                  r={3.5}
                  className={cn(FILL[overlay.tone], 'stroke-raised')}
                  strokeWidth={2}
                />
              ) : null}
            </g>
          ) : null}
          {shownLabels.map(({ i, text, anchor }) => (
            <text
              key={i}
              x={x(i)}
              y={height - 6}
              textAnchor={anchor}
              className="mono fill-dim"
              fontSize={TICK_FONT}
            >
              {text}
            </text>
          ))}
        </svg>
        {empty ? (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center pb-4">
            <EmptyPill>{emptyLabel}</EmptyPill>
          </div>
        ) : null}
        {hover !== null && !empty ? (
          <div
            ref={tip}
            aria-hidden="true"
            className="pointer-events-none absolute z-10 min-w-36 rounded-control border border-line-strong bg-raised px-2.5 py-2 shadow-pop"
          >
            <div className="mono mb-1 text-caption text-muted">{labels[hover]}</div>
            {cleaned
              .filter((s) => !gapped || s.present[hover])
              .map((s) => (
                <div key={s.name} className="flex items-center gap-2 text-caption">
                  <span className={cn('h-2 w-2 shrink-0 rounded-[2px]', BG[s.tone])} />
                  <span className="flex-1 text-muted">{s.name}</span>
                  <span className="mono text-fg tabular-nums">{formatValue(s.data[hover] ?? 0)}</span>
                </div>
              ))}
            {over && overlay ? (
              <div className="flex items-center gap-2 text-caption">
                <span className={cn('h-0.5 w-2.5 shrink-0 rounded-full', BG[overlay.tone])} />
                <span className="flex-1 text-muted">{overlay.name}</span>
                <span className="mono text-fg tabular-nums">{overlay.format(over[hover] ?? 0)}</span>
              </div>
            ) : null}
            {(noteMap.get(hover) ?? []).slice(0, 3).map((a, k) => (
              <div
                key={`n${k}`}
                className="mt-1 flex items-start gap-2 border-t border-line pt-1 text-caption"
              >
                <span className={cn('mt-1 h-2 w-2 shrink-0 rotate-45 rounded-[1px]', BG[a.tone])} />
                <span className="max-w-56 text-fg">{a.label}</span>
              </div>
            ))}
            {(noteMap.get(hover)?.length ?? 0) > 3 ? (
              <div className="mt-1 text-caption text-muted">
                and {(noteMap.get(hover)?.length ?? 0) - 3} more
              </div>
            ) : null}
          </div>
        ) : null}
      </div>
      <figcaption className="sr-only">
        <table>
          <caption>{title}</caption>
          <thead>
            <tr>
              <th scope="col">Time</th>
              {series.map((s) => (
                <th key={s.name} scope="col">
                  {s.name}
                </th>
              ))}
              {overlay ? <th scope="col">{overlay.name}</th> : null}
              {noteMap.size > 0 ? <th scope="col">Events</th> : null}
            </tr>
          </thead>
          <tbody>
            {labels.map((l, i) => (
              <tr key={i}>
                <th scope="row">{l}</th>
                {cleaned.map((s) => (
                  <td key={s.name}>{gapped && !s.present[i] ? 'No data' : formatValue(s.data[i] ?? 0)}</td>
                ))}
                {over && overlay ? <td>{overlay.format(over[i] ?? 0)}</td> : null}
                {noteMap.size > 0 ? <td>{(noteMap.get(i) ?? []).map((a) => a.label).join('; ')}</td> : null}
              </tr>
            ))}
          </tbody>
        </table>
        {valueTicks && valueTicks.length > 0 ? (
          <ul>
            {valueTicks.map((t) => (
              <li key={t.label}>
                {t.label}: {formatValue(t.value)}
              </li>
            ))}
          </ul>
        ) : null}
      </figcaption>
    </figure>
  );
}

export interface LegendItem {
  label: string;
  tone: ChartTone;
  dashed?: boolean;
  /** A total shown after the label ("$9.34"). */
  value?: string;
}

/** The series key of a chart: a small square in the series' colour and its name. */
export function Legend({ items, className }: { items: LegendItem[]; className?: string }) {
  return (
    <ul className={cn('flex flex-wrap items-center gap-x-3 gap-y-1 text-caption text-muted', className)}>
      {items.map((item) => (
        <li key={item.label} className="flex items-center gap-1.5">
          <span
            aria-hidden="true"
            className={cn('inline-block h-2 w-2 rounded-[2px]', BG[item.tone], item.dashed && 'opacity-60')}
          />
          {item.label}
          {item.value ? <span className="mono text-fg tabular-nums">{item.value}</span> : null}
        </li>
      ))}
    </ul>
  );
}

export interface MeterProps {
  value: number;
  max: number;
  tone?: ChartTone;
  /** An explicit colour (a team's), instead of a tone. */
  color?: string | undefined;
  /** A projected value drawn hatched after the solid part. */
  projected?: number | undefined;
  /** A tick (today's pacing, a threshold) as a fraction of `max`. */
  tick?: number | undefined;
  label: string;
  className?: string;
}

/** A horizontal bar of `value / max` on a sunken track, with an optional projection and tick. */
export function Meter({ value, max, tone = 'accent', color, projected, tick, label, className }: MeterProps) {
  const safeMax = max > 0 ? max : 1;
  const scale = Math.max(safeMax, projected ?? 0, value);
  const w = (v: number) => Math.max(0, Math.min(100, (v / scale) * 100));
  const pct = Math.round((value / safeMax) * 100);
  const filled = w(value);
  return (
    <svg
      viewBox="0 0 100 8"
      preserveAspectRatio="none"
      className={cn('block h-2 w-full overflow-visible', className)}
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={safeMax}
      aria-valuenow={value}
      aria-valuetext={`${pct}%`}
    >
      <rect x="0" y="0" width="100" height="8" rx="1" className="fill-sunken" />
      {projected !== undefined && projected > value ? (
        <rect
          x={filled}
          y="0"
          width={w(projected) - filled}
          height="8"
          className={color ? undefined : FILL[tone]}
          fill={color}
          fillOpacity={0.35}
        />
      ) : null}
      {filled > 0 ? (
        <rect
          x="0"
          y="0"
          width={Math.max(0.8, filled)}
          height="8"
          rx="1"
          className={color ? undefined : FILL[tone]}
          fill={color}
        />
      ) : null}
      {max > 0 && scale > max ? (
        <rect x={w(max) - 0.3} y="-1" width="0.6" height="10" className="fill-fg" />
      ) : null}
      {tick !== undefined ? (
        <rect x={Math.min(99.4, tick * w(safeMax))} y="-1" width="0.6" height="10" className="fill-muted" />
      ) : null}
    </svg>
  );
}

export interface BarRowProps {
  /** The row's name, with its swatch if any. */
  name: ReactNode;
  value: number;
  max: number;
  tone?: ChartTone;
  color?: string | undefined;
  /** The figures on the right ("$0.4K / $18K · 2%"). */
  display: ReactNode;
  /** The accessible name of the bar. */
  label: string;
  className?: string;
  /** Makes the row a toggle button (a bar that filters a list when clicked). */
  onSelect?: (() => void) | undefined;
  /** With `onSelect`: whether the row's filter is on. */
  pressed?: boolean | undefined;
}

/** A leaderboard row as in the prototype: name, a bar on a track, the figures. */
export function BarRow({
  name,
  value,
  max,
  tone = 'accent',
  color,
  display,
  label,
  className,
  onSelect,
  pressed,
}: BarRowProps) {
  const grid = 'grid min-h-7 grid-cols-[minmax(0,1fr)_minmax(40px,0.6fr)_auto] items-center gap-3 text-ui';
  const body = (
    <>
      <span className="flex min-w-0 items-center gap-2 truncate text-fg">{name}</span>
      <Meter value={value} max={max} tone={tone} color={color} label={label} />
      <span className="mono text-right text-caption whitespace-nowrap text-fg tabular-nums">{display}</span>
    </>
  );
  if (!onSelect) return <li className={cn(grid, className)}>{body}</li>;
  return (
    <li className={className}>
      <button
        type="button"
        aria-pressed={pressed ?? false}
        onClick={onSelect}
        className={cn(
          grid,
          '-mx-1.5 w-[calc(100%+12px)] rounded-control px-1.5 text-left transition-colors hover:bg-hover',
          pressed && 'bg-accent-dim',
        )}
      >
        {body}
      </button>
    </li>
  );
}

/** Horizontal bars for a ranked list (traffic by provider, members by team). */
export function BarList({
  items,
  format,
  label,
  onSelect,
  selected,
}: {
  items: { name: string; value: number; tone?: ChartTone; href?: string; swatch?: ReactNode }[];
  format: (v: number) => string;
  label: string;
  /** Makes each row a toggle that calls this with its name (a provider bar that filters a list). */
  onSelect?: (name: string) => void;
  /** With `onSelect`: the row whose filter is on. */
  selected?: string | undefined;
}) {
  // The longest bar is the largest value, also below 1 (dollars, ratios); 1 only when all are 0.
  const peak = Math.max(0, ...items.map((i) => i.value));
  const max = peak > 0 ? peak : 1;
  return (
    <ul aria-label={label} className="flex flex-col gap-1">
      {items.map((item) => (
        <BarRow
          key={item.name}
          name={
            <>
              {item.swatch}
              <span className="truncate">{item.name}</span>
            </>
          }
          value={item.value}
          max={max}
          tone={item.tone ?? 'accent'}
          display={format(item.value)}
          label={`${item.name}: ${format(item.value)}`}
          onSelect={onSelect ? () => onSelect(item.name) : undefined}
          pressed={onSelect ? selected === item.name : undefined}
        />
      ))}
    </ul>
  );
}
