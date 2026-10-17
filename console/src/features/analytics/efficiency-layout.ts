/**
 * The layout of the model efficiency chart (cost per request against p95 latency, one bubble per
 * model sized by its requests), kept apart from drawing so it can be tested: nice axes padded so no
 * bubble clips, area-proportional bubbles, a label for every bubble placed clear of the others (with
 * a leader line when crowded), median guides and corner captions that never sit under data.
 */
import { niceScale } from '@/components/ui/charts';
import { formatLatencyTick, formatMoneyTick } from '@/lib/format';

export interface EfficiencyPoint {
  name: string;
  requests: number;
  costPerRequest: number;
  p95: number | null;
}

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface PlacedLabel {
  box: Box;
  /** A line from the bubble's edge to the label when it sits away from its bubble. */
  leader: { x1: number; y1: number; x2: number; y2: number } | null;
}

export interface PlacedBubble extends EfficiencyPoint {
  p95: number;
  cx: number;
  cy: number;
  r: number;
  label: PlacedLabel;
}

export interface Caption {
  text: string;
  tone: 'success' | 'error';
  x: number;
  y: number;
  anchor: 'start' | 'end';
}

export interface EfficiencyLayout {
  width: number;
  height: number;
  plot: { left: number; top: number; right: number; bottom: number };
  xTicks: { x: number; text: string }[];
  yTicks: { y: number; text: string }[];
  bubbles: PlacedBubble[];
  /** Dashed lines at the median p95 and the median cost, with two models or more. */
  guides: { x: number; y: number } | null;
  captions: Caption[];
}

/** Bubble radii: the smallest model stays visible, the largest doesn't swamp the plot. */
export const MIN_RADIUS = 5;
export const MAX_RADIUS = 20;
/** A label's height and the room its mark and gap take before the text. */
export const LABEL_HEIGHT = 16;
export const LABEL_MARK = 16;
const CAPTION_HEIGHT = 14;

/** The width of a label's text at 11px JetBrains Mono. */
export const monoWidth = (text: string) => text.length * 6.7;

/** Area-proportional radius: twice the requests, twice the area. */
export function bubbleRadius(requests: number, maxRequests: number): number {
  if (!(maxRequests > 0) || !(requests > 0)) return MIN_RADIUS;
  return Math.max(MIN_RADIUS, Math.sqrt(requests / maxRequests) * MAX_RADIUS);
}

function median(values: number[]): number {
  const s = [...values].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? (s[mid] ?? 0) : ((s[mid - 1] ?? 0) + (s[mid] ?? 0)) / 2;
}

export function boxesOverlap(a: Box, b: Box): boolean {
  return a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y;
}

function overlapArea(a: Box, b: Box): number {
  const w = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
  const h = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
  return w > 0 && h > 0 ? w * h : 0;
}

export function boxHitsCircle(b: Box, c: { cx: number; cy: number; r: number }): boolean {
  const nx = Math.max(b.x, Math.min(c.cx, b.x + b.w));
  const ny = Math.max(b.y, Math.min(c.cy, b.y + b.h));
  return (nx - c.cx) ** 2 + (ny - c.cy) ** 2 < c.r ** 2;
}

/** Directions to try, the four sides first, then the diagonals, then the eight between. */
const ANGLES = [0, 180, 270, 90, 315, 45, 225, 135, 337.5, 22.5, 202.5, 157.5, 292.5, 247.5, 67.5, 112.5].map(
  (deg) => (deg * Math.PI) / 180,
);

/**
 * Candidate spots for a `w`-wide label around a bubble, nearest first: in each direction, the box
 * whose nearest edge sits `gap` px beyond the bubble's edge.
 */
export function candidates(cx: number, cy: number, r: number, w: number, gap: number): Box[] {
  const h = LABEL_HEIGHT;
  return ANGLES.map((a) => {
    const cos = Math.cos(a);
    const sin = Math.sin(a);
    const bx = cx + cos * (r + gap) + (Math.abs(cos) < 1e-9 ? 0 : Math.sign(cos) * (w / 2));
    const by = cy + sin * (r + gap) + (Math.abs(sin) < 1e-9 ? 0 : Math.sign(sin) * (h / 2));
    return { x: bx - w / 2, y: by - h / 2, w, h };
  });
}

/** How far a box reaches into a circle, 0 when clear. */
function depthIn(b: Box, c: { cx: number; cy: number; r: number }): number {
  const nx = Math.max(b.x, Math.min(c.cx, b.x + b.w));
  const ny = Math.max(b.y, Math.min(c.cy, b.y + b.h));
  return Math.max(0, c.r - Math.hypot(nx - c.cx, ny - c.cy));
}

/** The point of a box nearest to a centre, and the bubble's edge towards it, for a leader line. */
function leaderFor(box: Box, cx: number, cy: number, r: number): PlacedLabel['leader'] {
  const px = Math.max(box.x, Math.min(cx, box.x + box.w));
  const py = Math.max(box.y, Math.min(cy, box.y + box.h));
  const dist = Math.hypot(px - cx, py - cy);
  if (dist <= r + 6) return null;
  return { x1: cx + ((px - cx) / dist) * r, y1: cy + ((py - cy) / dist) * r, x2: px, y2: py };
}

/**
 * Lays the chart out in a `width` × `height` box. `labelOf` gives each point's label text (the
 * model's name, "Other models" for the rest); `measure` its width.
 */
export function layoutEfficiency(
  points: EfficiencyPoint[],
  width: number,
  height: number,
  labelOf: (p: EfficiencyPoint) => string = (p) => p.name,
  measure: (text: string) => number = monoWidth,
): EfficiencyLayout {
  const data = points.filter((p): p is EfficiencyPoint & { p95: number } => p.p95 !== null && p.requests > 0);
  const ys = niceScale(Math.max(0, ...data.map((p) => p.costPerRequest)), 4);
  const xs = niceScale(Math.max(0, ...data.map((p) => p.p95)), 4);
  const yStep = ys.ticks[1] ?? ys.top;
  const yText = ys.ticks.map((t) => formatMoneyTick(t, yStep));
  const left = Math.ceil(22 + Math.max(...yText.map(measure)) + 8);
  const plot = { left, top: 8, right: width - 8, bottom: height - 40 };
  const maxReq = Math.max(1, ...data.map((p) => p.requests));
  // Padding inside the axes: the biggest bubble at either end of a domain stays whole.
  const pad = MAX_RADIUS + 2;
  const x = (v: number) => plot.left + pad + (v / xs.top) * (plot.right - plot.left - 2 * pad);
  const y = (v: number) => plot.bottom - pad - (v / ys.top) * (plot.bottom - plot.top - 2 * pad);

  const circles = data.map((p) => ({
    p,
    cx: x(p.p95),
    cy: y(p.costPerRequest),
    r: bubbleRadius(p.requests, maxReq),
  }));
  const inside = (b: Box) =>
    b.x >= 2 && b.x + b.w <= width - 2 && b.y >= plot.top && b.y + b.h <= plot.bottom;
  const placed: Box[] = [];
  const labels = new Map<string, PlacedLabel>();
  // Larger bubbles choose first; every bubble gets a label, the least crowded spot when none is free.
  for (const c of [...circles].sort((a, b) => b.p.requests - a.p.requests)) {
    const text = labelOf(c.p);
    const w = (text === c.p.name ? LABEL_MARK : 0) + measure(text) + 2;
    const others = circles.filter((o) => o !== c);
    const spots = [4, 14, 26, 40, 56, 76, 100, 130].flatMap((gap) => candidates(c.cx, c.cy, c.r, w, gap));
    const crossings = (b: Box) => {
      const leader = leaderFor(b, c.cx, c.cy, c.r);
      if (!leader) return 0;
      let hits = 0;
      const steps = Math.max(2, Math.ceil(Math.hypot(leader.x2 - leader.x1, leader.y2 - leader.y1) / 4));
      for (let i = 1; i < steps; i += 1) {
        const px = leader.x1 + ((leader.x2 - leader.x1) * i) / steps;
        const py = leader.y1 + ((leader.y2 - leader.y1) * i) / steps;
        if (placed.some((o) => px > o.x && px < o.x + o.w && py > o.y && py < o.y + o.h)) hits += 1;
      }
      return hits;
    };
    const cost = (b: Box) =>
      (inside(b) ? 0 : 1e6) +
      crossings(b) * 120 +
      placed.reduce((sum, o) => sum + overlapArea(b, o), 0) * 4 +
      others.reduce((sum, o) => {
        const depth = depthIn(b, { ...o, r: o.r + 1 });
        return sum + (depth > 0 ? 40 + depth * 30 : 0);
      }, 0);
    // The nearest free spot; when none is, the least crowded one, a little nearer being better.
    const free = spots.find((b) => cost(b) === 0);
    const box =
      free ?? spots.reduce((best, b, i) => (cost(b) + i < cost(best) + spots.indexOf(best) ? b : best));
    placed.push(box);
    labels.set(c.p.name, { box, leader: leaderFor(box, c.cx, c.cy, c.r) });
  }
  const bubbles: PlacedBubble[] = circles.map((c) => ({
    ...c.p,
    cx: c.cx,
    cy: c.cy,
    r: c.r,
    label: labels.get(c.p.name) ?? { box: { x: c.cx, y: c.cy, w: 0, h: 0 }, leader: null },
  }));

  const guides =
    data.length >= 2
      ? { x: x(median(data.map((p) => p.p95))), y: y(median(data.map((p) => p.costPerRequest))) }
      : null;
  // Corner captions, only where nothing is drawn.
  const captions: Caption[] = [];
  if (guides) {
    const clear = (b: Box) =>
      !placed.some((o) => boxesOverlap(b, o)) && !circles.some((c) => boxHitsCircle(b, { ...c, r: c.r + 2 }));
    const fast = 'Fast and cheap';
    const slow = 'Slow and expensive';
    const fastBox = {
      x: plot.left + 6,
      y: plot.bottom - 6 - CAPTION_HEIGHT,
      w: fast.length * 6,
      h: CAPTION_HEIGHT,
    };
    const slowBox = {
      x: plot.right - 6 - slow.length * 6,
      y: plot.top + 4,
      w: slow.length * 6,
      h: CAPTION_HEIGHT,
    };
    if (clear(fastBox))
      captions.push({ text: fast, tone: 'success', x: fastBox.x, y: fastBox.y + 11, anchor: 'start' });
    if (clear(slowBox))
      captions.push({ text: slow, tone: 'error', x: plot.right - 6, y: slowBox.y + 11, anchor: 'end' });
  }
  return {
    width,
    height,
    plot,
    xTicks: xs.ticks.map((t) => ({ x: x(t), text: formatLatencyTick(t) })),
    yTicks: ys.ticks.map((t, i) => ({ y: y(t), text: yText[i] ?? '' })),
    bubbles,
    guides,
    captions,
  };
}
