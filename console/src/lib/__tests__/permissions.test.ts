import { describe, expect, it } from 'vitest';

import { navAllowed, NAV } from '../nav';
import { noPermissionReason, permissionPhrase } from '../permissions';

describe('permission words', () => {
  it('says what a permission lets someone do, never its id', () => {
    expect(permissionPhrase('config:apply')).toBe('apply configuration changes');
    expect(permissionPhrase('keys:create')).toBe('create API keys');
    expect(permissionPhrase('alerts:write')).toBe('change alerts');
    expect(noPermissionReason('cache:admin')).toBe("You don't have permission to manage the cache.");
    expect(noPermissionReason('role:gateway_admin')).toBe('Only a gateway admin can do this.');
  });
});

describe('nav access', () => {
  const item = (id: string) => {
    const found = NAV.flatMap((s) => s.items).find((i) => i.id === id);
    if (!found) throw new Error(`no nav item ${id}`);
    return found;
  };
  const holding =
    (perms: string[], gateway: boolean) =>
    (permission: string, gatewayWide: boolean): boolean =>
      perms.includes(permission) && (gateway || !gatewayWide);

  it('hides pages a scoped developer cannot read, with the gateway-wide ones too', () => {
    const developer = holding(['keys:read', 'logs:read', 'analytics:read', 'projects:read'], false);
    const shown = NAV.flatMap((s) => s.items)
      .filter((i) => navAllowed(i, developer))
      .map((i) => i.id);
    expect(shown).toEqual(['overview', 'request-log', 'analytics', 'budgets', 'safety', 'keys']);
    const teamViewer = holding(['models:read', 'alerts:read'], false);
    expect(navAllowed(item('models'), teamViewer)).toBe(false);
    expect(navAllowed(item('alerts'), teamViewer)).toBe(true);
    expect(navAllowed(item('models'), holding(['models:read'], true))).toBe(true);
  });
});
