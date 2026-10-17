import { describe, expect, it } from 'vitest';

import type { AlertChannelResource, AlertRuleResource, AuditEntry, UserResource } from '@/lib/api/types';

import { mergePatch } from '../form-errors';
import { lastChange } from '../last-change';
import {
  channelCell,
  channelToggle,
  MATRIX_ROWS,
  type MatrixRow,
  webhookCell,
  webhookToggle,
} from '../matrix';
import { remainderText, sharesErrors, sharesFrom, sharesText, sharesTo } from '../priority';
import {
  providerErrors,
  providerForm,
  providerRows,
  providerSpec,
  signInsByProvider,
  splitCertificates,
} from '../sso';

const meta = { etag: '"1"', generation: 1, created_at: '', updated_at: '', managed_by: 'api' as const };
function row(id: string): MatrixRow {
  const found = MATRIX_ROWS.find((r) => r.id === id);
  if (!found) throw new Error(id);
  return found;
}

function rule(name: string, spec: Partial<AlertRuleResource['spec']>): AlertRuleResource {
  return { name, spec: { category: 'errors', ...spec }, metadata: meta };
}

function channel(name: string, spec: Partial<AlertChannelResource['spec']> = {}): AlertChannelResource {
  return {
    name,
    spec: { type: 'slack', slack: { webhook_url: 'x' }, ...spec },
    metadata: meta,
  };
}

describe('merge patches', () => {
  it('sends only what changed, nulls what was removed, and keeps blank secrets', () => {
    const before = {
      issuer: 'https://a',
      client_secret: '[REDACTED]',
      scopes: ['openid'],
      display_name: 'A',
    };
    const after = { issuer: 'https://b', scopes: ['openid'] };
    expect(mergePatch(before, after, ['client_secret'])).toEqual({ issuer: 'https://b', display_name: null });
    expect(
      mergePatch({ idp: { metadata_url: 'u', metadata_refresh: '24h' } }, { idp: { entity_id: 'e' } }),
    ).toEqual({
      idp: { entity_id: 'e', metadata_url: null, metadata_refresh: null },
    });
  });
});

describe('single sign-on providers', () => {
  const sso = {
    oidc: { okta: { issuer: 'https://okta.example', client_id: 'c', display_name: 'Okta' } },
    saml: { corp: { idp: { metadata_url: 'https://idp.example/metadata' } } },
    oauth: { github: { type: 'github' as const, client_id: 'g', client_secret: '[REDACTED]' } },
  };

  it('lists every protocol with its button label', () => {
    expect(providerRows(sso).map((r) => `${r.protocol}/${r.name}/${r.displayName}`)).toEqual([
      'oidc/okta/Okta',
      'saml/corp/corp',
      'oauth/github/github',
    ]);
  });

  it('counts people by the provider of their last sign-in in 30 days', () => {
    const now = Date.parse('2026-10-15T00:00:00Z');
    const user = (name: string, at: string, method: string, provider: string | null) =>
      ({
        name,
        spec: { email: `${name}@x` },
        metadata: { ...meta, last_login_at: at, last_login_method: method, last_login_provider: provider },
      }) as unknown as UserResource;
    const counts = signInsByProvider(
      [
        user('a', '2026-10-10T00:00:00Z', 'oidc', 'okta'),
        user('b', '2026-10-12T00:00:00Z', 'saml', null),
        user('c', '2026-08-01T00:00:00Z', 'oidc', 'okta'),
        user('d', '2026-10-14T00:00:00Z', 'local', null),
      ],
      providerRows(sso),
      now,
    );
    expect(counts.get('oidc/okta')).toBe(1);
    expect(counts.get('saml/corp')).toBe(1);
    expect(counts.get('oauth/github')).toBeUndefined();
  });

  it('writes a SAML provider from one source of identity provider details', () => {
    const form = { ...providerForm('saml', null, undefined), name: 'corp', idpSource: 'fields' as const };
    const missing = providerErrors(form, true, []);
    expect(Object.keys(missing).sort()).toEqual(['certificates', 'entityId', 'ssoUrl']);
    const filled = {
      ...form,
      entityId: 'https://idp.example',
      ssoUrl: 'https://idp.example/sso',
      certificates:
        '-----BEGIN CERTIFICATE-----\nAAA\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nBBB\n-----END CERTIFICATE-----',
      mappings: [{ match: 'groups', value: 'ops', role: 'viewer', scope: 'gateway' }],
    };
    expect(providerErrors(filled, true, [])).toEqual({});
    const spec = providerSpec(filled);
    expect(spec.idp).toEqual({
      entity_id: 'https://idp.example',
      sso_url: 'https://idp.example/sso',
      certificates: splitCertificates(filled.certificates),
    });
    expect((spec.idp as { certificates: string[] }).certificates).toHaveLength(2);
    expect(spec.role_mappings).toEqual([
      { attribute: 'groups', value: 'ops', role: 'viewer', scope: 'gateway' },
    ]);
  });

  it('keeps a stored secret when the field is left blank', () => {
    const form = providerForm('oauth', 'github', sso.oauth.github);
    expect(form.clientSecret).toBe('');
    expect(providerSpec(form)).not.toHaveProperty('client_secret');
    expect(providerErrors(form, false, ['github'])).toEqual({});
  });
});

describe('notification matrix', () => {
  const slack = channel('ops-slack');
  const pager = channel('pager', { type: 'pagerduty', min_severity: 'critical' } as never);

  it('reads a severity from every metric rule of it, and respects a channel’s floor', () => {
    const rules = [
      rule('a', { severity: 'critical', channels: ['ops-slack'], metric: 'error_rate', threshold: 1 }),
      rule('b', { severity: 'critical', channels: [], metric: 'error_rate', threshold: 1 }),
    ];
    expect(channelCell(row('critical'), slack, rules).kind).toBe('mixed');
    expect(channelCell(row('warning'), slack, rules).kind).toBe('blocked');
    expect(channelCell(row('warning'), pager, rules).kind).toBe('blocked');
    const plan = channelToggle(row('critical'), 'ops-slack', true, rules);
    expect(plan.replace.map((r) => [r.rule.name, r.spec.channels])).toEqual([['b', ['ops-slack']]]);
  });

  it('creates the category’s event rule when none sends it, and leaves broader rules alone', () => {
    const plan = channelToggle(row('budget-exceeded'), 'ops-slack', true, []);
    expect(plan.create).toMatchObject({
      name: 'notify-budget-exceeded',
      spec: { kind: 'event', events: ['budget.exhausted'], channels: ['ops-slack'], category: 'budget' },
    });
    const rules = [
      rule('all-budget', {
        kind: 'event',
        events: ['budget.*'],
        channels: ['ops-slack'],
        category: 'budget',
      }),
      rule('notify-budget-exceeded', {
        kind: 'event',
        events: ['budget.exhausted'],
        channels: ['ops-slack'],
        category: 'budget',
      }),
    ];
    expect(channelCell(row('budget-exceeded'), slack, rules).kind).toBe('on');
    const off = channelToggle(row('budget-exceeded'), 'ops-slack', false, rules);
    expect(off.replace.map((r) => r.rule.name)).toEqual(['notify-budget-exceeded']);
    expect(off.still).toEqual(['all-budget']);
  });

  it('edits a webhook’s subscription and says when a pattern still covers the events', () => {
    const hook = { url: 'https://h', events: ['budget.*', 'key.anomaly'] };
    expect(webhookCell(row('budget-forecast'), hook).kind).toBe('on');
    expect(webhookCell(row('key-rotations'), hook).kind).toBe('off');
    expect(webhookCell(row('critical'), hook).kind).toBe('blocked');
    expect(webhookToggle(row('key-rotations'), hook, true).events).toEqual([
      'budget.*',
      'key.anomaly',
      'key.rotation_due',
      'key.expiring',
    ]);
    expect(webhookToggle(row('budget-forecast'), hook, false)).toEqual({
      events: hook.events,
      still: ['budget.*'],
    });
    expect(webhookToggle(row('key-anomalies'), hook, false).events).toEqual(['budget.*']);
  });
});

describe('last change of a connection', () => {
  const entry = (time: string, path: string, before: unknown, after: unknown): AuditEntry =>
    ({
      id: time,
      time,
      actor: { type: 'operator', id: 'ada' },
      action: 'settings.observability.update',
      resource: 'settings/observability',
      outcome: 'success',
      changes: [{ op: 'update', path, before, after }],
    }) as unknown as AuditEntry;

  it('finds the newest write that changed the connection, inside a broader change or directly', () => {
    const entries = [
      entry('3', 'observability', { traces: { enabled: false } }, { traces: { enabled: true } }),
      entry('2', 'observability.request_logs.sinks.http.wh', null, { url: 'https://w' }),
      entry('1', 'observability', {}, { request_logs: { sinks: { http: { wh: { url: 'https://v' } } } } }),
    ];
    expect(lastChange(entries, ['observability.request_logs.sinks.http'])?.id).toBe('2');
    expect(lastChange(entries, ['observability.traces'])?.id).toBe('3');
    expect(lastChange(entries, ['security.secrets.stores.vault'])).toBeNull();
    const odd = { ...entries[0], changes: { generation: 3 } } as unknown as AuditEntry;
    expect(lastChange([odd, ...entries], ['observability.traces'])?.id).toBe('3');
  });
});

describe('priority shares of a limit', () => {
  it('reads shares as percents and writes them back as ratios', () => {
    const rows = sharesFrom({ prod: 0.9, dev: 0.05 });
    expect(rows).toEqual([
      { cls: 'prod', percent: '90' },
      { cls: 'dev', percent: '5' },
    ]);
    expect(sharesTo(rows)).toEqual({ prod: 0.9, dev: 0.05 });
    expect(sharesTo([])).toBeNull();
    expect(remainderText(rows)).toBe('5% for keys of any other class or none');
    expect(sharesText({ prod: 0.9, dev: 0.1 }, 'borrow')).toBe('prod 90% · dev 10%, borrowing');
  });

  it('refuses bad classes, repeated classes and shares over 100%', () => {
    expect(sharesErrors([{ cls: 'Prod', percent: '10' }])).toHaveProperty('shares.0');
    expect(
      sharesErrors([
        { cls: 'a', percent: '10' },
        { cls: 'a', percent: '10' },
      ]),
    ).toHaveProperty('shares.1', 'Each class once');
    expect(
      sharesErrors([
        { cls: 'a', percent: '60' },
        { cls: 'b', percent: '50' },
      ]),
    ).toEqual({ shares: 'The shares add up to 110%; at most 100%' });
    expect(sharesErrors([{ cls: 'a', percent: '' }])).toHaveProperty('shares.0');
  });
});
