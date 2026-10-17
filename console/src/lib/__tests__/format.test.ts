import { describe, expect, it } from 'vitest';

import {
  compact,
  formatBytes,
  formatCount,
  formatDelta,
  formatDuration,
  formatFixed,
  formatLatency,
  formatMoney,
  formatPercent,
  formatPerMinute,
  formatSigned,
  formatTokens,
  initials,
  parseDuration,
  plural,
} from '../format';

describe('money', () => {
  it('keeps three significant digits under ten cents and cents above', () => {
    expect(formatMoney('0.0347')).toBe('$0.0347');
    expect(formatMoney(0.00015)).toBe('$0.00015');
    expect(formatMoney(0.551)).toBe('$0.55');
    expect(formatMoney(0.15)).toBe('$0.15');
    expect(formatMoney('2.5')).toBe('$2.50');
    expect(formatMoney(1234.5)).toBe('$1,234.50');
  });
  it('compacts thousands and signs when asked', () => {
    expect(formatMoney(19_500, { compact: true })).toBe('$19.5K');
    expect(formatMoney(-25, { sign: true })).toBe('−$25.00');
    expect(formatMoney(25, { sign: true })).toBe('+$25.00');
  });
  it('shows a dash for missing values', () => {
    expect(formatMoney(null)).toBe('—');
    expect(formatMoney('')).toBe('—');
  });
});

describe('latency, tokens and counts', () => {
  it('formats latency in ms then seconds', () => {
    expect(formatLatency(952.4)).toBe('952 ms');
    expect(formatLatency(2428)).toBe('2.43 s');
    expect(formatLatency(null)).toBe('—');
  });
  it('formats tokens', () => {
    expect(formatTokens(950)).toBe('950');
    expect(formatTokens(18_100)).toBe('18.1K');
    expect(formatTokens(4_790_000)).toBe('4.79M');
  });
  it('formats counts and compacts above ten thousand', () => {
    expect(formatCount(12_345)).toBe('12,345');
    expect(formatCount(12_345, { compact: true })).toBe('12.3K');
    expect(compact(1_500_000)).toBe('1.5M');
  });
});

describe('ratios, durations and sizes', () => {
  it('formats percentages and deltas', () => {
    expect(formatPercent(0.1234)).toBe('12.3%');
    expect(formatDelta(110, 100)).toBe('+10.0%');
    expect(formatDelta(0.05, 0.08, 'points')).toBe('−3.0pp');
    expect(formatDelta(5, 0)).toBe('new');
  });
  it('formats durations', () => {
    expect(formatDuration(15_120)).toBe('4h 12m');
    expect(formatDuration(185)).toBe('3m 05s');
    expect(formatDuration(1_051_200)).toBe('12d 4h');
  });
  it('parses config durations', () => {
    expect(parseDuration('90d')).toBe(7_776_000);
    expect(parseDuration('1h30m')).toBe(5400);
    expect(parseDuration('soon')).toBeNull();
  });
  it('formats bytes and initials', () => {
    expect(formatBytes(48_200_000_000)).toBe('48.2 GB');
    expect(initials('Priya Okonkwo')).toBe('PO');
    expect(initials('ada')).toBe('A');
  });
});

describe('plural', () => {
  it('puts the noun in the number of its count', () => {
    expect(plural(1, 'provider')).toBe('1 provider');
    expect(plural(0, 'admin')).toBe('0 admins');
    expect(plural(3, 'policy', 'policies')).toBe('3 policies');
    expect(plural(12_345, 'request')).toBe('12,345 requests');
  });
});

describe('negative zero', () => {
  it('never prints -0, whatever rounds to zero', () => {
    expect(formatPerMinute(0, { taken: true })).toBe('0.0/min');
    expect(formatPerMinute(0.01, { taken: true })).toBe('0.0/min');
    expect(formatPerMinute(0.4, { taken: true })).toBe('−0.4/min');
    expect(formatPerMinute(12.4)).toBe('12/min');
    expect(formatPerMinute(-0.01)).toBe('0.0/min');
    expect(formatFixed(-0.04)).toBe('0.0');
    expect(formatFixed(-0)).toBe('0.0');
    expect(formatSigned(-0.0001, 1, { percent: true })).toBe('0.0%');
    expect(formatSigned(-0.123, 1, { percent: true })).toBe('−12.3%');
    expect(formatSigned(0.5)).toBe('+0.5');
    expect(formatPercent(-0.00001)).toBe('0%');
    expect(formatCount(-0.2)).toBe('0');
    expect(formatTokens(-0.3)).toBe('0');
    expect(formatMoney(-0.0000001)).toBe('$0.00');
    expect(formatMoney(-2)).toBe('−$2.00');
    expect(compact(-0.01)).toBe('0.0');
    expect(formatLatency(-0.001)).toBe('0 ms');
    expect(formatDelta(99.999, 100)).toBe('no change');
  });
});
