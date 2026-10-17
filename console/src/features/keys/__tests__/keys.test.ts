import { describe, expect, it } from 'vitest';

import type { KeyResource, UserResource } from '@/lib/api/types';

import { EMPTY_KEY_FORM, fromKeySpec, keyFormSchema, toKeySpec } from '../form';
import {
  budgetsFor,
  filterKeys,
  EMPTY_FILTERS,
  KEY_URL_FILTERS,
  keyFiltersOf,
  keyFiltersToUrl,
  NO_OWNER,
  tightestBudget,
} from '../hooks';
import { dueKeys } from '../components/keys-tabs';
import { change, lastDay, splitDays } from '../components/key-facts';
import { keyStatus, keyType } from '../types';

const meta = {
  etag: '"1"',
  generation: 1,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  managed_by: 'api' as const,
  kid: 'abc',
  hint: 'sk-antirion-abc-…wxyz',
  created_by: 'ada',
};
const key = (spec: KeyResource['spec'], extra: Partial<KeyResource['metadata']> = {}): KeyResource => ({
  name: 'k',
  spec,
  metadata: { ...meta, ...extra },
});
const NOW = new Date('2026-06-01T00:00:00Z').getTime();

describe('key status and type', () => {
  it('derives the status from expiry, enabled and the rotation window', () => {
    expect(keyStatus(key({}), NOW)).toBe('active');
    expect(keyStatus(key({ expires_at: '2026-05-01T00:00:00Z' }), NOW)).toBe('expired');
    expect(keyStatus(key({ expires_at: '2026-06-05T00:00:00Z' }), NOW)).toBe('expiring');
    expect(keyStatus(key({ enabled: false }), NOW)).toBe('disabled');
    expect(
      keyStatus(key({}, { rotation: { previous_kid: 'old', valid_until: '2026-06-02T00:00:00Z' } }), NOW),
    ).toBe('rotating');
  });
  it('derives the type from the owner, the creator and the lifetime', () => {
    const users = new Map<string, UserResource>([
      ['svc', { name: 'svc', spec: { kind: 'service' }, metadata: meta }],
    ]);
    expect(keyType(key({ owner: 'svc' }), users)).toBe('service');
    expect(keyType(key({}, { created_by: 'playground' }), users)).toBe('ephemeral');
    expect(keyType(key({ expires_at: '2026-01-05T00:00:00Z' }), users)).toBe('ephemeral');
    expect(keyType(key({ owner: 'ada' }), users)).toBe('user');
  });

  it('lists a key with no owner as a service key, as the gateway types it', () => {
    expect(keyType(key({}), new Map())).toBe('service');
  });

  it("takes the gateway's type when it reports one", () => {
    expect(keyType(key({ owner: 'ada' }, { type: 'service' }), new Map())).toBe('service');
    expect(keyType(key({}, { type: 'ephemeral' }), new Map())).toBe('ephemeral');
  });
});

describe('key form', () => {
  it('keeps fields the form does not show when editing', () => {
    const base = {
      project: 'p',
      tags: ['team:x'],
      metadata: { a: 'b' },
      policies: { limits: { other: { unit: 'requests' as const, limit: 5, per: '1s' } } },
    };
    const values = { ...fromKeySpec('k', base), rpm: '120' };
    const spec = toKeySpec(values, base, NOW, true);
    expect(spec.tags).toEqual(['team:x']);
    expect(spec.metadata).toEqual({ a: 'b' });
    expect(spec.policies?.limits).toMatchObject({
      other: { limit: 5 },
      rpm: { unit: 'requests', limit: 120, per: '1m' },
    });
  });
  it('drops empty lists and sets the expiry', () => {
    const spec = toKeySpec({ ...EMPTY_KEY_FORM, name: 'k', expiry: '30d' }, {}, NOW);
    expect(spec.endpoints).toBeUndefined();
    expect(spec.expires_at).toBe('2026-07-01T00:00:00Z');
  });
  it('validates names, IPs and limits', () => {
    expect(keyFormSchema.safeParse({ ...EMPTY_KEY_FORM, name: 'ok-name' }).success).toBe(true);
    expect(keyFormSchema.safeParse({ ...EMPTY_KEY_FORM, name: 'Bad Name' }).success).toBe(false);
    expect(keyFormSchema.safeParse({ ...EMPTY_KEY_FORM, name: 'k', allowedIps: 'nope' }).success).toBe(false);
    expect(keyFormSchema.safeParse({ ...EMPTY_KEY_FORM, name: 'k', rpm: '1.5' }).success).toBe(false);
  });
  it('filters by type, status and text', () => {
    const views = [
      {
        key: key({}),
        name: 'alpha',
        type: 'user' as const,
        status: 'active' as const,
        project: 'default',
        owner: 'ada',
      },
      {
        key: key({}),
        name: 'beta',
        type: 'service' as const,
        status: 'disabled' as const,
        project: 'default',
        owner: null,
      },
    ];
    expect(
      filterKeys(views, { ...EMPTY_FILTERS, type: 'service' }, () => undefined).map((v) => v.name),
    ).toEqual(['beta']);
    expect(filterKeys(views, { ...EMPTY_FILTERS, query: 'ada' }, () => undefined).map((v) => v.name)).toEqual(
      ['alpha'],
    );
  });
});

describe('owner filter, budgets, rotation and hours', () => {
  const view = (
    name: string,
    owner: string | null,
    spec: KeyResource['spec'] = {},
    extra: Partial<KeyResource['metadata']> = {},
  ) => ({
    key: { ...key({ project: 'p', ...(owner ? { owner } : {}), ...spec }, extra), name },
    name,
    type: 'user' as const,
    status: 'active' as const,
    project: 'p',
    owner,
  });
  it('filters by owner and by no owner', () => {
    const views = [view('a', 'ada'), view('b', null)];
    expect(filterKeys(views, { ...EMPTY_FILTERS, owner: 'ada' }, () => undefined).map((v) => v.name)).toEqual(
      ['a'],
    );
    expect(
      filterKeys(views, { ...EMPTY_FILTERS, owner: NO_OWNER }, () => undefined).map((v) => v.name),
    ).toEqual(['b']);
  });
  it("picks the tightest budget and only the partitions that are the key's", () => {
    const c = (
      level: 'key' | 'project' | 'team' | 'organization',
      owner: string,
      spent: string,
      partition: string | null = null,
    ) => ({
      level,
      owner,
      name: 'm',
      partition,
      unit: 'usd' as const,
      mode: 'soft' as const,
      amount: '10',
      spent,
      reserved: '0',
      remaining: '0',
      period_start: '2026-06-01T00:00:00Z',
      resets_at: null,
    });
    const counters = [
      c('project', 'p', '2'),
      c('team', 't', '9'),
      c('organization', 'o', '9.5', 'user=bob'),
      c('organization', 'o', '5', 'user=ada'),
    ];
    const v = view('a', 'ada');
    expect(budgetsFor(v, counters, 't', 'o')).toHaveLength(3);
    expect(tightestBudget(v, counters, 't', 'o')?.level).toBe('team');
  });
  it('lists keys due within 14 days with the reminders already sent', () => {
    const due = dueKeys(
      [
        view('a', 'ada', {}, { rotation_due_at: '2026-06-06T00:00:00Z' }),
        view('b', null, { expires_at: '2026-07-01T00:00:00Z' }),
      ],
      [14, 7, 1],
      NOW,
    );
    expect(due).toHaveLength(1);
    expect(due[0]).toMatchObject({ kind: 'rotation', sent: [14, 7], next: 1 });
  });
  it('splits 48 hours into the last 24 and the 24 before', () => {
    const now = new Date('2026-06-02T12:30:00Z').getTime();
    const hours = [
      { start: '2026-06-01T10:00:00Z', requests: 4, cost: 1 },
      { start: '2026-06-02T11:00:00Z', requests: 6, cost: 2 },
    ];
    const { current, previous } = splitDays(hours, now);
    expect(current).toHaveLength(1);
    expect(previous).toHaveLength(1);
    expect(change(6, 4)).toBeCloseTo(0.5);
    expect(change(6, 0)).toBeNull();
    const day = lastDay(hours, now, (h) => h.requests);
    expect(day.values).toHaveLength(24);
    expect(day.values[22]).toBe(6);
  });
});

describe('regions, IP sets and priority in the form', () => {
  it('writes named sets as set: entries and reads them back apart from the ranges', () => {
    const spec = toKeySpec(
      {
        ...EMPTY_KEY_FORM,
        name: 'k',
        allowedIps: '10.0.0.0/8',
        ipSets: ['office'],
        regions: ['eu-west-1'],
        denyRegions: ['us-east-1'],
        priorityClass: 'batch',
      },
      {},
      NOW,
    );
    expect(spec).toMatchObject({
      allowed_ips: ['10.0.0.0/8', 'set:office'],
      regions: ['eu-west-1'],
      deny_regions: ['us-east-1'],
      priority_class: 'batch',
    });
    const back = fromKeySpec('k', spec);
    expect(back).toMatchObject({ allowedIps: '10.0.0.0/8', ipSets: ['office'], priorityClass: 'batch' });
    expect(
      toKeySpec({ ...back, ipSets: [], regions: [], priorityClass: '' }, spec, NOW, true),
    ).not.toHaveProperty('priority_class');
    expect(keyFormSchema.safeParse({ ...EMPTY_KEY_FORM, name: 'k', priorityClass: 'Not OK' }).success).toBe(
      false,
    );
  });
});

describe('certificate-bound keys in the form', () => {
  const FP = 'ab'.repeat(32);

  it('writes the fingerprint or the name and reads it back', () => {
    const spec = toKeySpec(
      { ...EMPTY_KEY_FORM, name: 'k', certBinding: 'fingerprint', certValue: ` ${FP} ` },
      {},
      NOW,
    );
    expect(spec.require_client_cert).toEqual({ sha256_fingerprint: FP });
    expect(fromKeySpec('k', spec)).toMatchObject({ certBinding: 'fingerprint', certValue: FP });
    const san = toKeySpec(
      { ...EMPTY_KEY_FORM, name: 'k', certBinding: 'san', certValue: 'spiffe://a/b' },
      {},
      NOW,
    );
    expect(san.require_client_cert).toEqual({ san: 'spiffe://a/b' });
    expect(fromKeySpec('k', san)).toMatchObject({ certBinding: 'san', certValue: 'spiffe://a/b' });
  });

  it('removes the binding when it is no longer required', () => {
    const spec = toKeySpec({ ...EMPTY_KEY_FORM, name: 'k', certBinding: 'san', certValue: 'svc' }, {}, NOW);
    const back = fromKeySpec('k', spec);
    expect(toKeySpec({ ...back, certBinding: 'none' }, spec, NOW, true)).not.toHaveProperty(
      'require_client_cert',
    );
  });

  it('accepts a fingerprint with or without colons, and nothing else', () => {
    const ok = (certValue: string, certBinding: 'fingerprint' | 'san' = 'fingerprint') =>
      keyFormSchema.safeParse({ ...EMPTY_KEY_FORM, name: 'k', certBinding, certValue }).success;
    expect(ok(FP)).toBe(true);
    expect(ok((FP.match(/../g) ?? []).join(':'))).toBe(true);
    expect(ok('abc')).toBe(false);
    expect(ok('', 'san')).toBe(false);
    expect(ok('spiffe://a/b', 'san')).toBe(true);
  });
});

describe('key filters in the URL', () => {
  it('reads what a link carries and ignores types and statuses it does not know', () => {
    expect(
      keyFiltersOf({
        ...KEY_URL_FILTERS,
        q: 'bot',
        type: 'robot',
        status: ['disabled', 'gone'],
        owner: 'ada',
      }),
    ).toEqual({ query: 'bot', type: 'all', statuses: ['disabled'], team: '', owner: 'ada' });
  });

  it('writes filters back as the same parameters', () => {
    const f = {
      query: 'ci',
      type: 'service' as const,
      statuses: ['active' as const],
      team: 'core',
      owner: '',
    };
    expect(keyFiltersOf({ ...keyFiltersToUrl(f), starred: false })).toEqual(f);
  });
});
