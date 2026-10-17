import { describe, expect, it } from 'vitest';

import { titleFor } from '@/features/change-requests/describe';

import { rowsToCsv } from '../csv';
import { favouriteLink } from '../favourites';
import { activeNav, NAV } from '../nav';
import { formatTokenPrice } from '../format';
import { comparisonWindow, decodeCompare, encodeCompare, resolveRange } from '../time';
import { decodeFilters, encodeFilters } from '../url-filters';

describe('URL filters', () => {
  const defaults = { q: '', state: [] as string[], starred: false };

  it('round-trips text, lists and switches, writing only what differs from the defaults', () => {
    const params = encodeFilters(
      new URLSearchParams('org=acme'),
      { q: 'slow', state: ['a', 'b'], starred: true },
      defaults,
    );
    expect(params.toString()).toBe('org=acme&q=slow&state=a&state=b&starred=1');
    expect(decodeFilters(params, defaults)).toEqual({ q: 'slow', state: ['a', 'b'], starred: true });
    expect(encodeFilters(params, defaults, defaults).toString()).toBe('org=acme');
  });

  it('keeps the default for a missing or malformed value', () => {
    expect(decodeFilters(new URLSearchParams('starred=maybe'), defaults)).toEqual(defaults);
  });
});

describe('comparison windows', () => {
  const from = new Date('2026-10-14T06:00:00Z');
  const to = new Date('2026-10-14T12:00:00Z');

  it('is the preceding window by default and names it', () => {
    const w = comparisonWindow(from, to, { kind: 'previous' }, 'Last 6 hours');
    expect(w.from.toISOString()).toBe('2026-10-14T00:00:00.000Z');
    expect(w.to.toISOString()).toBe('2026-10-14T06:00:00.000Z');
    expect(w.label).toBe('the previous 6 hours');
  });

  it('moves a week or a calendar month back, with the same length', () => {
    const week = comparisonWindow(from, to, { kind: 'week' }, 'Last 6 hours');
    expect(week.from.toISOString()).toBe('2026-10-07T06:00:00.000Z');
    expect(week.to.getTime() - week.from.getTime()).toBe(6 * 3_600_000);
    const month = comparisonWindow(from, to, { kind: 'month' }, 'Last 6 hours');
    expect(month.from.getUTCMonth()).toBe(8);
    expect(month.label).toBe('the same time last month');
  });

  it('starts a custom window where asked, and lives in the URL', () => {
    const start = Date.parse('2026-10-01T00:00:00Z');
    const w = comparisonWindow(from, to, { kind: 'custom', from: start }, 'x');
    expect(w.from.getTime()).toBe(start);
    expect(w.to.getTime()).toBe(start + 6 * 3_600_000);
    expect(encodeCompare({ kind: 'previous' })).toBeNull();
    expect(decodeCompare(encodeCompare({ kind: 'custom', from: start }))).toEqual({
      kind: 'custom',
      from: start,
    });
    expect(decodeCompare('week')).toEqual({ kind: 'week' });
    expect(decodeCompare('nonsense')).toEqual({ kind: 'previous' });
    const resolved = resolveRange({ kind: 'preset', preset: '6h' }, to, { kind: 'week' });
    expect(resolved.previous.label).toBe('the same time last week');
  });
});

describe('shell helpers', () => {
  it('states a price per 1M or per 1K tokens', () => {
    expect(formatTokenPrice('2.50', '1M')).toBe('$2.50 / 1M');
    expect(formatTokenPrice('2.50', '1K')).toBe('$0.0025 / 1K');
  });

  it('exports any rows as CSV with the given columns', () => {
    const rows = [{ name: 'a,b', n: 1 }];
    expect(
      rowsToCsv(
        [
          { header: 'name', value: (r) => r.name },
          { header: 'n', value: (r) => r.n },
        ],
        rows,
      ),
    ).toBe('name,n\r\n"a,b",1\r\n');
  });

  it('titles a change request after the refused call', () => {
    expect(titleFor({ method: 'PATCH', path: '/routes/default' })).toBe('Change route default');
    expect(titleFor({ method: 'POST', path: '/routes', body: { name: 'fast' } })).toBe('Create route fast');
    expect(titleFor({ method: 'DELETE', path: '/alerts/rules/p95' })).toBe('Delete alert rule p95');
    expect(titleFor({ method: 'PUT', path: '/settings/routing' })).toBe('Change routing settings');
  });

  it('links a favourite to its screen', () => {
    expect(favouriteLink({ kind: 'key', name: 'ci' })).toMatchObject({
      path: '/keys/',
      query: { key: 'ci' },
    });
    expect(favouriteLink({ kind: 'page', name: '/models/' })).toMatchObject({
      label: 'Models',
      kind: 'Page',
    });
    expect(favouriteLink({ kind: 'mcp_server', name: 'issues' })).toMatchObject({
      kind: 'MCP server',
      path: '/mcp/',
      query: { server: 'issues' },
    });
    expect(favouriteLink({ kind: 'plugin', name: 'pii' })).toMatchObject({
      path: '/plugins/',
      query: { plugin: 'pii' },
    });
    expect(favouriteLink({ kind: 'page', name: '/agents/' })).toMatchObject({
      label: 'Agents',
      kind: 'Page',
    });
  });

  it('puts MCP, agents and plugins in an Extend group, each behind its read permission', () => {
    const extend = NAV.find((s) => s.label === 'Extend');
    expect(extend?.items.map((i) => [i.label, i.permission])).toEqual([
      ['MCP', 'mcp:read'],
      ['Agents', 'mcp:read'],
      ['Plugins', 'plugins:read'],
    ]);
    expect(activeNav('/plugins/')).toBe('plugins');
  });
});
