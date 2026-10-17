import { describe, expect, it } from 'vitest';

import { formatLatencyTick, formatMoneyTick } from '@/lib/format';

import {
  boxesOverlap,
  boxHitsCircle,
  bubbleRadius,
  candidates,
  type EfficiencyPoint,
  layoutEfficiency,
  MAX_RADIUS,
  MIN_RADIUS,
} from '../efficiency-layout';

const DEMO: EfficiencyPoint[] = [
  { name: 'gpt-4o', requests: 24, costPerRequest: 0.0051, p95: 1500 },
  { name: 'gpt-4o-mini', requests: 48, costPerRequest: 0.0003, p95: 1650 },
  { name: 'mistral-large', requests: 15, costPerRequest: 0.0034, p95: 1590 },
  { name: 'claude-sonnet', requests: 11, costPerRequest: 0.008, p95: 804 },
  { name: 'gpt-5-preview', requests: 4, costPerRequest: 0.0096, p95: 686 },
  { name: 'llama-70b', requests: 10, costPerRequest: 0.0002, p95: 1750 },
  { name: 'house-model', requests: 8, costPerRequest: 0.00014, p95: 1790 },
  { name: 'other', requests: 3, costPerRequest: 0, p95: 0 },
];

describe('bubbleRadius', () => {
  it('scales the area with requests, between a minimum and a maximum', () => {
    expect(bubbleRadius(100, 100)).toBe(MAX_RADIUS);
    expect(bubbleRadius(25, 100)).toBeCloseTo(MAX_RADIUS / 2);
    expect(bubbleRadius(1, 10_000)).toBe(MIN_RADIUS);
    expect(bubbleRadius(0, 0)).toBe(MIN_RADIUS);
  });
});

describe('tick formatting', () => {
  it('reads "0 ms" and whole seconds, and money with the decimals its step needs', () => {
    expect(formatLatencyTick(0)).toBe('0 ms');
    expect(formatLatencyTick(500)).toBe('500 ms');
    expect(formatLatencyTick(1500)).toBe('1.5 s');
    expect(formatLatencyTick(2000)).toBe('2 s');
    expect(formatMoneyTick(0, 0.0025)).toBe('$0');
    expect(formatMoneyTick(0.005, 0.0025)).toBe('$0.0050');
    expect(formatMoneyTick(0.002, 0.002)).toBe('$0.002');
    expect(formatMoneyTick(1.5, 0.5)).toBe('$1.5');
  });
});

describe('layoutEfficiency', () => {
  const l = layoutEfficiency(DEMO, 560, 280, (p) => (p.name === 'other' ? 'Other models' : p.name));

  it('keeps every bubble whole inside the plot', () => {
    for (const b of l.bubbles) {
      expect(b.cx - b.r).toBeGreaterThanOrEqual(l.plot.left);
      expect(b.cx + b.r).toBeLessThanOrEqual(l.plot.right);
      expect(b.cy - b.r).toBeGreaterThanOrEqual(l.plot.top);
      expect(b.cy + b.r).toBeLessThanOrEqual(l.plot.bottom);
    }
  });

  it('starts the axes at "0 ms" and "$0" with nice ticks, and leaves room for the longest money tick', () => {
    expect(l.xTicks[0]?.text).toBe('0 ms');
    expect(l.xTicks.map((t) => t.text)).toEqual(['0 ms', '500 ms', '1 s', '1.5 s', '2 s']);
    expect(l.yTicks[0]?.text).toBe('$0');
    expect(l.yTicks.at(-1)?.text).toBe('$0.0100');
    expect(l.plot.left).toBeGreaterThan('$0.0100'.length * 6.7);
  });

  it('labels every bubble, inside the chart, with no two labels overlapping', () => {
    expect(l.bubbles).toHaveLength(DEMO.length);
    const boxes = l.bubbles.map((b) => b.label.box);
    for (const box of boxes) {
      expect(box.w).toBeGreaterThan(0);
      expect(box.x).toBeGreaterThanOrEqual(0);
      expect(box.x + box.w).toBeLessThanOrEqual(560);
      expect(box.y).toBeGreaterThanOrEqual(l.plot.top);
      expect(box.y + box.h).toBeLessThanOrEqual(l.plot.bottom);
    }
    boxes.forEach((a, i) => boxes.slice(i + 1).forEach((b) => expect(boxesOverlap(a, b)).toBe(false)));
  });

  it('keeps a lone label beside its bubble and draws a leader only when it moved away', () => {
    const b = l.bubbles.find((x) => x.name === 'gpt-5-preview');
    expect(b?.label.leader).toBeNull();
    for (const x of l.bubbles) {
      if (!x.label.leader) continue;
      const { x1, y1 } = x.label.leader;
      expect(Math.hypot(x1 - x.cx, y1 - x.cy)).toBeCloseTo(x.r);
    }
  });

  it('draws median guides and puts a corner caption only where nothing is drawn', () => {
    expect(l.guides).not.toBeNull();
    for (const c of l.captions) {
      const box = {
        x: c.anchor === 'end' ? c.x - c.text.length * 6 : c.x,
        y: c.y - 11,
        w: c.text.length * 6,
        h: 14,
      };
      for (const b of l.bubbles) {
        expect(boxHitsCircle(box, b)).toBe(false);
        expect(boxesOverlap(box, b.label.box)).toBe(false);
      }
    }
  });

  it('draws a single model without guides or captions, and nothing without latency', () => {
    const one = layoutEfficiency(
      [{ name: 'gpt-4o', requests: 5, costPerRequest: 0.002, p95: 900 }],
      560,
      280,
    );
    expect(one.bubbles).toHaveLength(1);
    expect(one.guides).toBeNull();
    expect(one.captions).toEqual([]);
    expect(
      layoutEfficiency([{ name: 'x', requests: 4, costPerRequest: 0.1, p95: null }], 560, 280).bubbles,
    ).toEqual([]);
  });

  it('offers spots on every side, nearest first', () => {
    const spots = candidates(100, 100, 10, 40, 4);
    expect(spots).toHaveLength(16);
    expect(spots[0]).toEqual({ x: 114, y: 92, w: 40, h: 16 });
    expect(spots[1]?.x).toBe(100 - 14 - 40);
  });
});
