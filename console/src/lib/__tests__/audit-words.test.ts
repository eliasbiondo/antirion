import { describe, expect, it } from 'vitest';

import {
  auditActionLabel,
  auditActorBy,
  auditEntryLabel,
  auditActorKind,
  auditActorLabel,
  auditToggle,
} from '../audit-words';

describe('audit entries in words', () => {
  it('words every action code, however many parts it has', () => {
    expect(auditActionLabel('keys.create')).toBe('Key created');
    expect(auditActionLabel('a2a.agents.create')).toBe('Agent created');
    expect(auditActionLabel('mcp.gateways.create')).toBe('MCP gateway created');
    expect(auditActionLabel('mcp.servers.create')).toBe('MCP server created');
    expect(auditActionLabel('alerts.channels.create')).toBe('Alert channel created');
    expect(auditActionLabel('settings.policies.update')).toBe('Policies settings updated');
    expect(auditActionLabel('settings.observability.update')).toBe('Observability settings updated');
    expect(auditActionLabel('playground.mint')).toBe('Playground key made');
    expect(auditActionLabel('GET /admin/v1/auth/me/preferences')).toBe('Read preferences');
  });

  it('names actors without repeating their kind', () => {
    expect(auditActorLabel({ type: 'token', id: 'bootstrap' })).toBe('Bootstrap token');
    expect(auditActorKind({ type: 'token', id: 'bootstrap' })).toBeNull();
    expect(auditActorLabel({ type: 'system', id: 'key_reminders' })).toBe('The gateway (key reminders)');
    expect(auditActorLabel({ type: 'user', id: 'ada' })).toBe('ada');
    expect(auditActorKind({ type: 'user', id: 'ada' })).toBe('user');
  });

  it('names a token by whose it is, and reads the same after "by" everywhere', () => {
    const tokens = { wjqfot3ltsdw: "ada's token" };
    expect(auditActorLabel({ type: 'token', id: 'wjqfot3ltsdw' }, tokens)).toBe("ada's token");
    expect(auditActorLabel({ type: 'token', id: 'wjqfot3ltsdw' })).toBe('A token (wjqfot3l…)');
    expect(auditActorBy({ type: 'token', id: 'bootstrap' })).toBe('Bootstrap token');
    expect(auditActorBy({ id: 'bootstrap' })).toBe('Bootstrap token');
    expect(auditActorBy({ type: 'system', id: 'key_reminders' })).toBe('the gateway (key reminders)');
  });

  it('tells an edit that only turned something off or on', () => {
    expect(auditToggle([{ op: 'update', path: 'keys.k.enabled', before: true, after: false }])).toBe(false);
    expect(auditToggle([{ op: 'update', path: 'enabled', before: false, after: true }])).toBe(true);
    expect(auditToggle([{ op: 'update', path: 'keys.k.description', before: 'a', after: 'b' }])).toBeNull();
    expect(auditToggle(null)).toBeNull();
    // A whole spec before and after, with only `enabled` changed: an absent field is on.
    const spec = { project: 'dashboards', expires_at: '2027-01-14T13:09:35Z' };
    expect(
      auditToggle([{ op: 'update', path: 'keys.k', before: spec, after: { ...spec, enabled: false } }]),
    ).toBe(false);
    expect(
      auditToggle([
        {
          op: 'update',
          path: 'keys.k',
          before: { ...spec, enabled: false },
          after: { ...spec, enabled: true },
        },
      ]),
    ).toBe(true);
    expect(
      auditToggle([{ op: 'update', path: 'keys.k', before: spec, after: { ...spec, description: 'x' } }]),
    ).toBeNull();
  });

  it('words a plugin attached to every request and a switch, from the changes', () => {
    const policies = (attach: string[]) => ({ guardrails: { input: [], output: [] }, plugins: { attach } });
    expect(
      auditEntryLabel({
        action: 'settings.policies.update',
        resource: 'settings/policies',
        changes: [{ op: 'update', path: 'policies', before: policies([]), after: policies(['audit']) }],
      }),
    ).toBe('Plugin audit turned on for every request');
    const spec = { type: 'webhook', enabled: true };
    expect(
      auditEntryLabel({
        action: 'plugins.update',
        resource: 'plugins/audit',
        changes: [{ op: 'update', path: 'plugins.audit', before: spec, after: { ...spec, enabled: false } }],
      }),
    ).toBe('Plugin audit turned off');
    expect(auditEntryLabel({ action: 'keys.create', resource: 'keys/k', changes: null })).toBe('Key created');
  });
});
