import { describe, expect, it } from 'vitest';

import { readableScale } from '../charts';

const whole = (v: number) => String(Math.round(v));

describe('readableScale', () => {
  it('keeps the rounded ticks when their labels differ', () => {
    expect(readableScale(180, whole).tickText).toEqual(['0', '50', '100', '150', '200']);
  });

  it('falls back to whole steps when rounding would repeat a label', () => {
    const scale = readableScale(1, whole);
    expect(scale.tickText).toEqual(['0', '1']);
    expect(scale.top).toBe(1);
  });

  it('keeps whole-number ticks for a small peak', () => {
    expect(readableScale(3, whole).tickText).toEqual(['0', '1', '2', '3', '4']);
  });
});
