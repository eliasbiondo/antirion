import { describe, expect, it } from 'vitest';

import { isAdjustment, lineSubject, usageSourceWord, type LedgerLine } from '@/features/ledger/api';

import { peopleNote, type Person } from '../people';
import { counterGap, trailingRate } from '../sources';
import { runsOutWords } from '../types';

const date = (iso: string) => iso.slice(0, 10);

describe('runsOutWords', () => {
  const base = { spent: 100, effective: 200, state: 'on_pace' as const, rolling: false, runsOutAt: null };

  it('says a blocked or spent budget has already run out', () => {
    expect(runsOutWords({ ...base, spent: 2050, effective: 2000, state: 'over' }, date)).toBe(
      'Already reached',
    );
    expect(runsOutWords({ ...base, state: 'blocked' }, date)).toBe('Already reached');
  });

  it('gives the forecast date, else not this period, and nothing for a rolling budget', () => {
    expect(runsOutWords({ ...base, runsOutAt: '2026-10-20T00:00:00Z' }, date)).toBe('2026-10-20');
    expect(runsOutWords(base, date)).toBe('Not this period');
    expect(runsOutWords({ ...base, rolling: true }, date)).toBe('—');
  });
});

describe('spend sources', () => {
  it('projects from the trailing week of the same costs', () => {
    expect(trailingRate([1, 1, 1, 1, 1, 1, 1, 8, 8], 9)).toBeCloseTo((5 + 16) / 7);
    expect(trailingRate([3, 3], 2)).toBe(3);
    expect(trailingRate([], 0)).toBe(0);
  });

  it('reports a gap between counters and request costs only when it matters', () => {
    expect(counterGap(6400, 38.22)).toBeCloseTo(6361.78);
    expect(counterGap(100, 99.5)).toBeNull();
    expect(counterGap(10_000, 9_950)).toBeNull();
  });
});

describe('peopleNote', () => {
  const person = (requests: number): Person => ({
    name: 'ada',
    displayName: 'Ada',
    title: null,
    team: null,
    service: false,
    lastActive: null,
    requests,
    cost: 0,
    perRequest: null,
    outlier: false,
  });

  it('explains that only keys people own count', () => {
    expect(peopleNote([person(0), person(0)])).toMatch(/keys no person owns, such as service keys/);
    expect(peopleNote([person(3)])).toMatch(/is not counted here/);
  });
});

describe('ledger lines', () => {
  const line = (over: Partial<LedgerLine>): LedgerLine => ({
    request_id: 'r1',
    line: 0,
    purpose: 'primary',
    status: 'success',
    settled_at: '2026-10-14T21:06:00Z',
    billed_usd: '1',
    usage_source: 'upstream',
    ...over,
  });

  it('describes an adjustment as one, not as a provider report', () => {
    const adj = line({ status: 'adjustment', purpose: 'adjustment' });
    expect(isAdjustment(adj)).toBe(true);
    expect(usageSourceWord(adj)).toBe('Entered by hand');
    expect(lineSubject(adj)).toBe('Spend adjustment');
    expect(usageSourceWord(line({}))).toBe('Reported by the provider');
  });
});
