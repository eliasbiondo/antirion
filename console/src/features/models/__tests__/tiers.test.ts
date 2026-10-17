import { describe, expect, it } from 'vitest';

import { describeAudit } from '../activity';
import { tierErrors, tierPatch, tiersOf, tiersText } from '../tiers';
import type { AuditEntry } from '@/lib/api/types';

describe('request-dependent prices in the price grid', () => {
  it('reads service tiers and the batch multiplier of a price', () => {
    const t = tiersOf({ input: '1', service_tiers: { flex: '0.5', priority: '2' }, batch_multiplier: '0.5' });
    expect(t).toEqual({
      tiers: [
        { tier: 'flex', multiplier: '0.5' },
        { tier: 'priority', multiplier: '2' },
      ],
      batch: '0.5',
    });
    expect(tiersText(t.tiers)).toBe('flex ×0.5 · priority ×2');
    expect(tiersOf(null)).toEqual({ tiers: [], batch: '' });
  });

  it('writes a merge patch that removes the tiers taken out', () => {
    const before = [
      { tier: 'flex', multiplier: '0.5' },
      { tier: 'priority', multiplier: '2' },
    ];
    expect(tierPatch(before, [{ tier: 'flex', multiplier: '0.4' }], '')).toEqual({
      service_tiers: { priority: null, flex: '0.4' },
      batch_multiplier: null,
    });
    expect(tierPatch(before, [], '0.5')).toEqual({ service_tiers: null, batch_multiplier: '0.5' });
  });

  it('refuses bad tier names, repeated tiers and multipliers that are not numbers', () => {
    expect(tierErrors([{ tier: 'Flex Tier', multiplier: '1' }], '')).toMatch(/Name each tier/);
    expect(
      tierErrors(
        [
          { tier: 'flex', multiplier: '1' },
          { tier: 'flex', multiplier: '2' },
        ],
        '',
      ),
    ).toBe('Tier flex is listed twice');
    expect(tierErrors([{ tier: 'flex', multiplier: 'half' }], '')).toMatch(/multiplier/);
    expect(tierErrors([], 'x')).toMatch(/batch/);
    expect(tierErrors([{ tier: 'flex', multiplier: '0.5' }], '0.5')).toBeNull();
  });
});

describe('access request activity reads the facts of the one change ', () => {
  it('finds the scope in the change’s after, as in the old object', () => {
    const base = {
      id: '1',
      time: '2026-10-15T10:00:00Z',
      actor: { type: 'operator', id: 'ana' },
      action: 'POST access_requests',
      resource: 'models/m/access_requests/r1',
      outcome: 'success',
      status_code: 201,
      generation_before: 1,
      generation_after: 1,
      request_id: 'q',
      source_ip: null,
    } as const;
    const listed = describeAudit({
      ...base,
      changes: [
        { op: 'update', path: 'models.m.access_requests.r1', before: null, after: { scope: 'team/x' } },
      ],
    } as unknown as AuditEntry);
    const legacy = describeAudit({ ...base, changes: { scope: 'team/x' } } as unknown as AuditEntry);
    expect(listed?.text).toBe(legacy?.text);
    expect(listed?.text).toContain('team/x');
  });
});
