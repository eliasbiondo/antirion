import { describe, expect, it } from 'vitest';

import {
  allowedIntervals,
  decodeRange,
  defaultInterval,
  encodeRange,
  formatRelative,
  resolveRange,
  smallestInterval,
} from '../time';

describe('time ranges', () => {
  it('defaults the bucket by preset', () => {
    expect(defaultInterval({ kind: 'preset', preset: '15m' })).toBe('1m');
    expect(defaultInterval({ kind: 'preset', preset: '1h' })).toBe('1m');
    expect(defaultInterval({ kind: 'preset', preset: '6h' })).toBe('5m');
    expect(defaultInterval({ kind: 'preset', preset: '24h' })).toBe('15m');
    expect(defaultInterval({ kind: 'preset', preset: '7d' })).toBe('1h');
    expect(defaultInterval({ kind: 'preset', preset: '30d' })).toBe('1d');
  });
  it('never allows more than 1,440 buckets', () => {
    expect(allowedIntervals(30 * 86_400)).not.toContain('1m');
    expect(smallestInterval(2 * 86_400)).toBe('5m');
  });
  it('compares with the preceding equal window', () => {
    const now = new Date('2026-10-01T12:00:00Z');
    const r = resolveRange({ kind: 'preset', preset: '6h' }, now);
    expect(r.from.toISOString()).toBe('2026-10-01T06:00:00.000Z');
    expect(r.previous.from.toISOString()).toBe('2026-10-01T00:00:00.000Z');
    expect(r.previous.to.toISOString()).toBe(r.from.toISOString());
  });
  it('round-trips through the URL', () => {
    expect(decodeRange(encodeRange({ kind: 'preset', preset: '7d' }))).toEqual({
      kind: 'preset',
      preset: '7d',
    });
    expect(decodeRange('1700000000000-1700003600000')).toEqual({
      kind: 'custom',
      from: 1_700_000_000_000,
      to: 1_700_003_600_000,
    });
    expect(decodeRange('1700003600000-1700000000000')).toBeNull();
    expect(decodeRange('yesterday')).toBeNull();
  });
  it('says how long ago', () => {
    const now = new Date('2026-10-01T12:00:00Z');
    expect(formatRelative('2026-10-01T11:59:25Z', now)).toBe('35s ago');
    expect(formatRelative('2026-10-01T09:00:00Z', now)).toBe('3h ago');
    expect(formatRelative('2026-10-13T12:00:00Z', now)).toBe('in 12d');
  });
});
