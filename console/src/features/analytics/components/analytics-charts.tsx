'use client';

import { useState } from 'react';

import { useWidth } from '@/components/ui/charts';
import { SvgMark, useModelFaces } from '@/components/ui/marks';
import { Tooltip } from '@/components/ui/tooltip';
import { EmptyState } from '@/components/ui/states';
import type { UsageBucket } from '@/lib/api/types';
import { cn } from '@/lib/cn';
import { formatCount, formatLatency, formatMoney } from '@/lib/format';

import {
  type EfficiencyLayout,
  type EfficiencyPoint,
  LABEL_MARK,
  layoutEfficiency,
  type PlacedBubble,
} from '../efficiency-layout';

export type { EfficiencyPoint } from '../efficiency-layout';

const WEEKDAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
const WEEKDAY_NAMES = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];

/** Six steps of the accent, so the cells take their colour from classes (no style attribute). */
const LEVELS = [
  'bg-sunken',
  'bg-[color-mix(in_oklab,var(--ant-accent)_14%,var(--ant-bg-sunken))]',
  'bg-[color-mix(in_oklab,var(--ant-accent)_30%,var(--ant-bg-sunken))]',
  'bg-[color-mix(in_oklab,var(--ant-accent)_48%,var(--ant-bg-sunken))]',
  'bg-[color-mix(in_oklab,var(--ant-accent)_68%,var(--ant-bg-sunken))]',
  'bg-accent',
];

function level(v: number, max: number): number {
  if (v <= 0 || max <= 0) return 0;
  return Math.max(1, Math.min(LEVELS.length - 1, Math.ceil((v / max) * (LEVELS.length - 1))));
}

/** Requests by weekday and hour over the last 7 days (the prototype's heatmap), in the reader's time zone. */
export function Heatmap({ rows, timeZone }: { rows: UsageBucket[]; timeZone: string }) {
  const grid = Array.from({ length: 7 }, () => new Array<number>(24).fill(0));
  const fmt = new Intl.DateTimeFormat('en-US', {
    timeZone,
    weekday: 'short',
    hour: 'numeric',
    hour12: false,
  });
  for (const r of rows) {
    const parts = fmt.formatToParts(new Date(r.start));
    const day = WEEKDAYS.indexOf(parts.find((p) => p.type === 'weekday')?.value ?? '');
    const hour = Number(parts.find((p) => p.type === 'hour')?.value ?? 0) % 24;
    const row = grid[day];
    if (row) row[hour] = (row[hour] ?? 0) + r.requests;
  }
  const max = Math.max(0, ...grid.flat());
  const peak = grid.flatMap((r, d) => r.map((v, h) => ({ v, d, h }))).sort((a, b) => b.v - a.v)[0];
  const zone = timeZone === 'UTC' ? 'UTC' : timeZone.split('/').pop()?.replace(/_/g, ' ');
  return (
    // The cells are square at every width: their size steps with the panel's width (a container
    // query), the grid keeps its own width and scrolls inside the panel rather than stretch.
    <div className="@container w-full">
      <figure className="m-0 w-fit max-w-full [--cell:10px] [--gap:2px] @min-[400px]:[--cell:12px] @min-[470px]:[--cell:14px] @min-[540px]:[--cell:16px] @min-[600px]:[--cell:18px] @min-[600px]:[--gap:3px] @min-[660px]:[--cell:20px]">
        <div className="overflow-x-auto">
          <div
            role="img"
            aria-label={
              peak && peak.v > 0
                ? `Requests by weekday and hour over the last 7 days. Busiest: ${WEEKDAY_NAMES[peak.d] ?? ''} ${peak.h}:00, ${formatCount(peak.v)} requests.`
                : 'No requests in the last 7 days'
            }
            className="grid w-max grid-cols-[28px_repeat(24,var(--cell))] gap-(--gap)"
          >
            {grid.map((r, d) => (
              <div key={d} className="contents">
                <span className="flex items-center text-caption text-dim">{WEEKDAYS[d]}</span>
                {r.map((v, h) => (
                  <Tooltip
                    key={h}
                    content={`${WEEKDAYS[d] ?? ''} ${String(h).padStart(2, '0')}:00 · ${formatCount(v)} requests`}
                    delay={150}
                  >
                    <span
                      data-cell=""
                      className={cn(
                        'aspect-square h-(--cell) w-(--cell) rounded-[2px]',
                        LEVELS[level(v, max)],
                      )}
                    />
                  </Tooltip>
                ))}
              </div>
            ))}
            <span />
            {Array.from({ length: 24 }, (_, h) => (
              <span key={h} className="mono flex justify-center pt-1 text-caption whitespace-nowrap text-dim">
                {h % 6 === 0 ? String(h).padStart(2, '0') : ''}
              </span>
            ))}
          </div>
        </div>
        <figcaption className="mt-3 flex flex-wrap items-center justify-between gap-x-4 gap-y-2 text-caption text-muted">
          <span>
            {peak && peak.v > 0
              ? `Busiest: ${WEEKDAY_NAMES[peak.d] ?? ''} ${String(peak.h).padStart(2, '0')}:00 ${zone ?? ''} (${formatCount(peak.v)} requests)`
              : 'No requests in the last 7 days'}
          </span>
          <span className="flex items-center gap-(--gap)" aria-hidden="true">
            <span className="mr-1">Less</span>
            {LEVELS.map((c) => (
              <span key={c} data-swatch="" className={cn('h-(--cell) w-(--cell) rounded-[2px]', c)} />
            ))}
            <span className="ml-1">More</span>
          </span>
        </figcaption>
      </figure>
    </div>
  );
}

/** The bucket the usage report folds its smaller models into. */
const OTHER = 'other';
const TIP_W = 196;
const TIP_H = 76;

/**
 * Cost per request against p95 latency, one bubble per model sized by its requests (the
 * prototype's model efficiency). Every bubble is labelled with its maker's mark and name; hovering
 * or focusing one shows its figures, choosing it opens the model. Fast and cheap sits bottom left.
 */
export function EfficiencyChart({
  points,
  height = 280,
  onOpen,
}: {
  points: EfficiencyPoint[];
  height?: number;
  onOpen?: (model: string) => void;
}) {
  const [width, ref] = useWidth(560);
  const [active, setActive] = useState<string | null>(null);
  const faceOf = useModelFaces();
  const data = points.filter((p) => p.p95 !== null && p.requests > 0);
  if (data.length === 0)
    return <EmptyState size="sm" title="No requests with a measured latency in this range" />;
  const labelOf = (p: EfficiencyPoint) => (p.name === OTHER ? 'Names not in the catalog' : p.name);
  const l = layoutEfficiency(data, width, height, labelOf);
  const { plot } = l;
  const hovered = l.bubbles.find((b) => b.name === active) ?? null;
  const open = (name: string) => (name !== OTHER && onOpen ? onOpen(name) : undefined);
  return (
    <figure className="m-0">
      <div ref={ref} className="w-full">
        <svg
          width={width}
          height={height}
          viewBox={`0 0 ${width} ${height}`}
          className="block max-w-full"
          role="group"
          aria-label="Cost per request against p95 latency, by model"
        >
          {l.yTicks.map((t, i) => (
            <g key={`y${t.text}`}>
              <line
                x1={plot.left}
                x2={plot.right}
                y1={Math.round(t.y) + 0.5}
                y2={Math.round(t.y) + 0.5}
                className={i === 0 ? 'stroke-line' : 'stroke-grid'}
              />
              <text x={plot.left - 8} y={t.y + 4} textAnchor="end" className="mono fill-dim" fontSize={11}>
                {t.text}
              </text>
            </g>
          ))}
          {l.xTicks.map((t) => (
            <text
              key={`x${t.text}`}
              x={t.x}
              y={plot.bottom + 16}
              textAnchor="middle"
              className="mono fill-dim"
              fontSize={11}
            >
              {t.text}
            </text>
          ))}
          <line
            x1={plot.left + 0.5}
            x2={plot.left + 0.5}
            y1={plot.top}
            y2={plot.bottom}
            className="stroke-line"
          />
          <text
            x={(plot.left + plot.right) / 2}
            y={height - 4}
            textAnchor="middle"
            className="fill-muted"
            fontSize={11}
          >
            p95 latency
          </text>
          <text
            transform={`translate(11 ${(plot.top + plot.bottom) / 2}) rotate(-90)`}
            textAnchor="middle"
            className="fill-muted"
            fontSize={11}
          >
            Cost per request
          </text>
          {l.guides ? (
            <g aria-hidden="true" className="stroke-line-strong" strokeDasharray="3 4">
              <line x1={l.guides.x} x2={l.guides.x} y1={plot.top} y2={plot.bottom} />
              <line x1={plot.left} x2={plot.right} y1={l.guides.y} y2={l.guides.y} />
            </g>
          ) : null}
          {l.captions.map((c) => (
            <text
              key={c.text}
              x={c.x}
              y={c.y}
              textAnchor={c.anchor}
              fontSize={11}
              aria-hidden="true"
              className={c.tone === 'success' ? 'fill-success' : 'fill-error'}
              fillOpacity={0.8}
            >
              {c.text}
            </text>
          ))}
          {l.bubbles.map((b) => {
            const on = active === b.name;
            const other = b.name === OTHER;
            return (
              <g
                key={b.name}
                data-model={b.name}
                role={other ? 'img' : 'link'}
                tabIndex={other ? undefined : 0}
                aria-label={`${labelOf(b)}: ${formatMoney(b.costPerRequest)} per request, p95 ${formatLatency(b.p95)}, ${formatCount(b.requests)} requests`}
                onMouseEnter={() => setActive(b.name)}
                onMouseLeave={() => setActive(null)}
                onFocus={() => setActive(b.name)}
                onBlur={() => setActive(null)}
                onClick={() => open(b.name)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') open(b.name);
                }}
                className={cn('outline-none', other ? 'cursor-default' : 'cursor-pointer')}
              >
                <circle
                  cx={b.cx}
                  cy={b.cy}
                  r={b.r}
                  className={other ? 'fill-dim stroke-dim' : 'fill-accent stroke-accent'}
                  fillOpacity={on ? 0.5 : 0.22}
                  strokeWidth={on ? 2 : 1.25}
                />
              </g>
            );
          })}
          {/* Labels above every bubble, so a crowded label or its leader is never hidden. */}
          {l.bubbles.map((b) => {
            const on = active === b.name;
            const other = b.name === OTHER;
            const face = other ? null : faceOf(b.name);
            const { box, leader } = b.label;
            return (
              <g
                key={`label-${b.name}`}
                data-label={b.name}
                aria-hidden="true"
                onMouseEnter={() => setActive(b.name)}
                onMouseLeave={() => setActive(null)}
                onClick={() => open(b.name)}
                className={cn(on ? 'text-fg' : 'text-muted', other ? 'cursor-default' : 'cursor-pointer')}
              >
                {leader ? (
                  <line
                    x1={leader.x1}
                    y1={leader.y1}
                    x2={leader.x2}
                    y2={leader.y2}
                    className="stroke-current"
                    strokeOpacity={0.6}
                  />
                ) : null}
                {face ? <SvgMark face={face} x={box.x + 1} y={box.y + 2} size={12} /> : null}
                <text
                  x={box.x + (face ? LABEL_MARK : 0) + 1}
                  y={box.y + 12}
                  fontSize={11}
                  className={cn('mono', on ? 'fill-fg' : 'fill-muted')}
                >
                  {labelOf(b)}
                </text>
              </g>
            );
          })}
          {hovered ? <EfficiencyTip b={hovered} label={labelOf(hovered)} width={width} plot={plot} /> : null}
        </svg>
      </div>
      {l.bubbles.length === 1 ? (
        <figcaption className="mt-2 text-caption text-muted">
          One model in this range: compare models once more than one serves requests.
        </figcaption>
      ) : null}
    </figure>
  );
}

/** A bubble's figures beside it, kept inside the chart. */
function EfficiencyTip({
  b,
  label,
  width,
  plot,
}: {
  b: PlacedBubble;
  label: string;
  width: number;
  plot: EfficiencyLayout['plot'];
}) {
  const right = b.cx + b.r + 10 + TIP_W <= width;
  const x = right ? b.cx + b.r + 10 : Math.max(2, b.cx - b.r - 10 - TIP_W);
  const y = Math.min(Math.max(plot.top, b.cy - TIP_H / 2), plot.bottom - TIP_H);
  const rows: [string, string][] = [
    ['Cost per request', formatMoney(b.costPerRequest)],
    ['p95 latency', formatLatency(b.p95)],
    ['Requests', formatCount(b.requests)],
  ];
  return (
    <g aria-hidden="true" pointerEvents="none">
      <rect x={x} y={y} width={TIP_W} height={TIP_H} rx={6} className="fill-raised stroke-line-strong" />
      <text x={x + 10} y={y + 18} fontSize={12} fontWeight={600} className="mono fill-fg">
        {label.length > 26 ? `${label.slice(0, 25)}…` : label}
      </text>
      {rows.map(([k, v], i) => (
        <g key={k}>
          <text x={x + 10} y={y + 36 + i * 15} fontSize={11} className="fill-muted">
            {k}
          </text>
          <text
            x={x + TIP_W - 10}
            y={y + 36 + i * 15}
            fontSize={11}
            textAnchor="end"
            className="mono fill-fg"
          >
            {v}
          </text>
        </g>
      ))}
    </g>
  );
}
