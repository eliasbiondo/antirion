import { describe, expect, it } from 'vitest';

import type { LogRecord } from '@/lib/api/types';

import {
  activeFilterCount,
  applyLocally,
  EMPTY_LOG_FILTERS,
  filterChips,
  filtersToQuery,
  flags,
} from '../types';

function record(id: string, status: number, total: number, extra: Partial<LogRecord> = {}): LogRecord {
  return {
    id,
    request_id: id,
    status_code: status,
    started_at: '2026-06-01T00:00:00Z',
    timing: { total_ms: total },
    cost: { total: '0.01' },
    cache: { status: 'miss' },
    usage: { input: 10, output: 5 },
    guardrails: [],
    stream: null,
    model: 'm',
    key: 'k',
    team: 't',
    end_user: null,
    ...extra,
  } as LogRecord;
}

describe('request log filters', () => {
  it('turns filters into contract parameters', () => {
    expect(
      filtersToQuery({ ...EMPTY_LOG_FILTERS, statusClass: ['5xx'], latencyMin: '2000', q: ' abc ' }),
    ).toEqual({ status_class: ['5xx'], latency_ms_min: 2000, q: 'abc' });
    expect(activeFilterCount({ ...EMPTY_LOG_FILTERS, flagged: true, provider: ['a', 'b'] })).toBe(3);
  });

  it('applies the filters a gateway refused on the loaded records', () => {
    const rows = [record('a', 200, 100), record('b', 502, 3000), record('c', 404, 50)];
    const out = applyLocally(
      rows,
      { ...EMPTY_LOG_FILTERS, statusClass: ['5xx', '4xx'] },
      new Set(['status_class']),
      'started_at',
      'desc',
    );
    expect(out.map((r) => r.id)).toEqual(['b', 'c']);
    const sorted = applyLocally(rows, EMPTY_LOG_FILTERS, new Set(['sort']), 'latency_ms', 'desc');
    expect(sorted.map((r) => r.id)).toEqual(['b', 'a', 'c']);
    expect(
      applyLocally(rows, { ...EMPTY_LOG_FILTERS, statusClass: ['5xx'] }, new Set(), 'started_at', 'desc'),
    ).toHaveLength(3);
  });

  it('lists non-pass guardrail categories as flags', () => {
    const r = record('a', 200, 10, {
      guardrails: [
        { verdict: 'pass', category: 'pii' },
        { verdict: 'redact', category: 'pii' },
      ],
    });
    expect(flags(r)).toEqual(['pii']);
  });
});

describe('active filter chips', () => {
  it('lists each active filter with the filters left after removing it', () => {
    const f = { ...EMPTY_LOG_FILTERS, statusClass: ['4xx', '5xx'] as const, latencyMin: '2000', q: 'abc' };
    const chips = filterChips({ ...f, statusClass: [...f.statusClass] });
    expect(chips.map((c) => `${c.label}: ${c.value}`)).toEqual([
      'Status: 4xx, 5xx',
      'Latency: at least 2000 ms',
      'Search: “abc”',
    ]);
    expect(chips[0]?.without.statusClass).toEqual([]);
    expect(chips[0]?.without.latencyMin).toBe('2000');
  });
});
