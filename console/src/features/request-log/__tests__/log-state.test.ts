import { beforeEach, describe, expect, it } from 'vitest';

import type { LogRecord } from '@/lib/api/types';
import { emptySlot } from '@/lib/slot';
import { decodeFilters, encodeFilters } from '@/lib/url-filters';

import { isStandard, moveColumn, resolveColumns, STANDARD_COLUMNS, toggleColumn } from '../columns';
import { bodyPatch } from '../components/replay-dialog';
import { useLogs } from '../store';
import {
  activeFilterCount,
  decodeSort,
  EMPTY_LOG_FILTERS,
  encodeSort,
  filterChips,
  filtersFromView,
  fromUrlState,
  LOG_URL_DEFAULTS,
  matchesFilters,
  toUrlState,
  viewFilters,
} from '../types';

function record(id: string, extra: Partial<LogRecord> = {}): LogRecord {
  return {
    id,
    request_id: id,
    status_code: 200,
    started_at: '2026-06-01T00:00:00Z',
    timing: { total_ms: 500 },
    cost: { total: '0.02' },
    cache: { status: 'miss' },
    usage: { input: 10, output: 5 },
    guardrails: [],
    stream: null,
    model: 'm',
    provider: 'p',
    key: 'k',
    team: 't',
    end_user: null,
    tags: ['env:prod', 'app:a'],
    ...extra,
  } as LogRecord;
}

describe('the log in the URL and in saved views', () => {
  const filters = {
    ...EMPTY_LOG_FILTERS,
    statusClass: ['5xx' as const],
    environment: ['prod'],
    tag: ['app:a'],
    latencyMin: '100',
    latencyMax: '900',
    costMin: '0.01',
    costMax: '0.5',
    flagged: true,
    q: 'refund',
  };

  it('round-trips every filter and the sort through the URL', () => {
    const params = encodeFilters(
      new URLSearchParams('org=gateway'),
      toUrlState(filters, 'ttft_ms', 'asc'),
      LOG_URL_DEFAULTS,
    );
    expect(params.get('org')).toBe('gateway');
    expect(params.get('sort')).toBe('ttft_ms:asc');
    expect(params.getAll('environment')).toEqual(['prod']);
    const back = fromUrlState(decodeFilters(params, LOG_URL_DEFAULTS));
    expect(back).toEqual({ filters, sort: 'ttft_ms', order: 'asc' });
    expect(
      encodeFilters(
        new URLSearchParams(),
        toUrlState(EMPTY_LOG_FILTERS, 'started_at', 'desc'),
        LOG_URL_DEFAULTS,
      ).toString(),
    ).toBe('');
  });

  it('ignores values the log does not know', () => {
    const back = fromUrlState(
      decodeFilters(
        new URLSearchParams('status=9xx&cache=maybe&latency_min=abc&sort=name:up'),
        LOG_URL_DEFAULTS,
      ),
    );
    expect(back.filters.statusClass).toEqual([]);
    expect(back.filters.cache).toEqual([]);
    expect(back.filters.latencyMin).toBe('');
    expect(back.sort).toBe('started_at');
  });

  it('stores view filters in the list parameters and reads the older names too', () => {
    expect(viewFilters(filters)).toMatchObject({
      status_class: ['5xx'],
      environment: ['prod'],
      tag: ['app:a'],
      latency_ms_min: 100,
      latency_ms_max: 900,
      cost_usd_min: '0.01',
      cost_usd_max: '0.5',
      flagged: true,
      q: 'refund',
    });
    expect(filtersFromView(viewFilters(filters))).toEqual(filters);
    expect(filtersFromView({ statusClass: ['4xx'], latencyMin: '2000' })).toMatchObject({
      statusClass: ['4xx'],
      latencyMin: '2000',
    });
    expect(encodeSort('started_at', 'desc')).toBeNull();
    expect(decodeSort('cost_usd:desc')).toEqual({ sort: 'cost_usd', order: 'desc' });
  });

  it('names ranges in chips', () => {
    const chips = filterChips(filters).map((c) => `${c.label}: ${c.value}`);
    expect(chips).toContain('Latency: 100 ms to 900 ms');
    expect(chips).toContain('Cost: $0.01 to $0.5');
    expect(chips).toContain('Environment: prod');
  });

  it('shows every filter a link sets as a chip: project, user and request id too', () => {
    const state = fromUrlState(
      decodeFilters(
        new URLSearchParams('project=zendesk-assistant&user=ada&request_id=01a1-x&team=support'),
        LOG_URL_DEFAULTS,
      ),
    );
    const chips = filterChips(state.filters).map((c) => `${c.label}: ${c.value}`);
    expect(chips).toEqual(['Team: support', 'Project: zendesk-assistant', 'User: ada', 'Request ID: 01a1-x']);
    expect(activeFilterCount(state.filters)).toBe(4);
    const without = filterChips(state.filters).find((c) => c.label === 'Project')?.without;
    expect(without?.project).toEqual([]);
    expect(toUrlState(state.filters, 'started_at', 'desc')).toMatchObject({
      project: ['zendesk-assistant'],
      user: ['ada'],
      request_id: ['01a1-x'],
    });
  });

  it('checks a live record against every filter', () => {
    expect(matchesFilters(record('a', { project: 'p1' }), { ...EMPTY_LOG_FILTERS, project: ['p2'] })).toBe(
      false,
    );
    expect(matchesFilters(record('a'), { ...EMPTY_LOG_FILTERS, requestId: ['a'] })).toBe(true);
    expect(matchesFilters(record('a'), EMPTY_LOG_FILTERS)).toBe(true);
    expect(matchesFilters(record('a'), { ...EMPTY_LOG_FILTERS, environment: ['staging'] })).toBe(false);
    expect(matchesFilters(record('a'), { ...EMPTY_LOG_FILTERS, tag: ['app:a'] })).toBe(true);
    expect(matchesFilters(record('a'), { ...EMPTY_LOG_FILTERS, latencyMax: '400' })).toBe(false);
    expect(matchesFilters(record('a'), { ...EMPTY_LOG_FILTERS, costMin: '0.05' })).toBe(false);
    expect(matchesFilters(record('a'), { ...EMPTY_LOG_FILTERS, statusClass: ['2xx'], q: 'A' })).toBe(true);
  });
});

describe('columns', () => {
  it('falls back to the standard set and keeps the time column', () => {
    expect(resolveColumns(null)).toEqual(STANDARD_COLUMNS);
    expect(resolveColumns(['cost', 'bogus', 'cost'])).toEqual(['time', 'cost']);
    expect(isStandard(STANDARD_COLUMNS)).toBe(true);
  });

  it('shows, hides and moves columns', () => {
    const shown = toggleColumn(STANDARD_COLUMNS, 'ttft');
    expect(shown.indexOf('ttft')).toBe(shown.indexOf('latency') + 1);
    expect(toggleColumn(shown, 'ttft')).toEqual(STANDARD_COLUMNS);
    expect(toggleColumn(STANDARD_COLUMNS, 'time')).toEqual(STANDARD_COLUMNS);
    expect(moveColumn(['time', 'cost', 'status'], 'status', -1)).toEqual(['time', 'status', 'cost']);
    expect(moveColumn(['time', 'cost'], 'time', -1)).toEqual(['time', 'cost']);
  });
});

describe('replay', () => {
  it('patches only the members that changed', () => {
    expect(bodyPatch({ model: 'a', temperature: 1, 'x/y': 1 }, { model: 'b', 'x/y': 1, top_p: 0.5 })).toEqual(
      [
        { op: 'remove', path: '/temperature' },
        { op: 'add', path: '/model', value: 'b' },
        { op: 'add', path: '/top_p', value: 0.5 },
      ],
    );
    expect(bodyPatch({ a: [1] }, { a: [1] })).toEqual([]);
  });
});

describe('live rows', () => {
  beforeEach(() => {
    useLogs.setState({
      records: { ...emptySlot(), data: [record('old')] },
      incoming: [],
      held: false,
      cursor: 'c',
    });
  });

  it('joins the table at once, or waits while held, without repeats', () => {
    useLogs.getState().receive([record('n1'), record('old')]);
    expect(useLogs.getState().records.data?.map((r) => r.id)).toEqual(['n1', 'old']);
    useLogs.getState().setHeld(true);
    useLogs.getState().receive([record('n2')]);
    useLogs.getState().receive([record('n2'), record('n3')]);
    expect(useLogs.getState().incoming.map((r) => r.id)).toEqual(['n3', 'n2']);
    expect(useLogs.getState().records.data).toHaveLength(2);
    useLogs.getState().setHeld(false);
    expect(useLogs.getState().records.data?.map((r) => r.id)).toEqual(['n3', 'n2', 'n1', 'old']);
    expect(useLogs.getState().incoming).toEqual([]);
  });
});
