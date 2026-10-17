import { create } from 'zustand';

import { api, setUnauthenticatedHandler } from '@/lib/api/client';
import type { Me } from '@/lib/api/types';

export type SessionStatus = 'unknown' | 'loading' | 'authenticated' | 'anonymous' | 'expired';

interface SessionState {
  status: SessionStatus;
  me: Me | null;
  /** Set when the session ended under the operator (401 on a call): sign-in says so. */
  expiredAt: number | null;
  load: () => Promise<void>;
  adopt: (me: Me) => void;
  signInWithToken: (token: string) => Promise<Me>;
  signOut: () => Promise<void>;
  markExpired: () => void;
}

/**
 * The signed-in principal. The session itself is the `antirion_session` cookie, HttpOnly and set
 * by the gateway, so nothing here is persisted: the admin token typed at sign-in is sent once to
 * `POST /auth/session` and dropped.
 */
export const useSession = create<SessionState>()((set, get) => ({
  status: 'unknown',
  me: null,
  expiredAt: null,

  async load() {
    if (get().status === 'loading') return;
    set({ status: 'loading' });
    try {
      const me = await api.get('/auth/me');
      set({ status: 'authenticated', me, expiredAt: null });
    } catch {
      set({ status: 'anonymous', me: null });
    }
  },

  adopt(me) {
    set({ status: 'authenticated', me, expiredAt: null });
  },

  async signInWithToken(token) {
    const me = await api.post('/auth/session', { body: { token } });
    set({ status: 'authenticated', me, expiredAt: null });
    return me;
  },

  async signOut() {
    try {
      await api.post('/auth/logout');
    } finally {
      set({ status: 'anonymous', me: null });
    }
  },

  markExpired() {
    if (get().status === 'authenticated') set({ status: 'expired', expiredAt: Date.now() });
  },
}));

setUnauthenticatedHandler(() => useSession.getState().markExpired());

/** The scopes a principal holds permissions in: `gateway`, `organization/acme`, `team/x`, … */
export function permissionScopes(me: Me | null): string[] {
  return me ? Object.keys(me.permissions) : [];
}

/**
 * Whether the principal holds `permission` for the active scope: a gateway-scope grant covers
 * every organization; an organization grant covers that organization; a narrower grant (team or
 * project) is enough to show the control, since the server checks the exact resource.
 */
export function hasPermission(me: Me | null, permission: string, organization: string | null): boolean {
  if (!me) return false;
  const perms = me.permissions;
  if (perms.gateway?.includes(permission)) return true;
  if (organization && perms[`organization/${organization}`]?.includes(permission)) return true;
  return Object.entries(perms).some(
    ([scope, list]) =>
      (scope.startsWith('team/') || scope.startsWith('project/')) && list.includes(permission),
  );
}

/** Whether the principal holds a permission on the whole gateway, as gateway-wide reports require. */
export function hasGatewayPermission(me: Me | null, permission: string): boolean {
  return Boolean(me?.permissions.gateway?.includes(permission));
}

/** A nav item's check (`navAllowed`) for this principal in an organization. */
export function navHolds(me: Me | null, organization: string | null) {
  return (permission: string, gatewayWide: boolean) =>
    gatewayWide ? hasGatewayPermission(me, permission) : hasPermission(me, permission, organization);
}

/** Whether the principal holds any gateway-scope binding (and may pick "Gateway" as scope). */
export function isGatewayPrincipal(me: Me | null): boolean {
  return Boolean(me?.permissions.gateway?.length);
}

/** The highest role among the principal's bindings, for the user menu. */
export function highestRole(me: Me | null): string | null {
  if (!me || me.bindings.length === 0) return null;
  const order = [
    'gateway_admin',
    'org_admin',
    'gateway_viewer',
    'team_admin',
    'security_auditor',
    'developer',
    'viewer',
  ];
  const roles = me.bindings.map((b) => b.role);
  return [...roles].sort((a, b) => rank(a) - rank(b))[0] ?? null;

  function rank(role: string): number {
    const i = order.indexOf(role);
    return i === -1 ? order.length : i;
  }
}
