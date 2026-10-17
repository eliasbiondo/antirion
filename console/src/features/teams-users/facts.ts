import { auditEntryLabel } from '@/lib/audit-words';
import type { AuditEntry, Role, Session } from '@/lib/api/types';

import { scopeLabel, type UserView } from './types';

const DAY = 86_400_000;

/** A user with no activity for this many days is "inactive" and worth a review of their access. */
export const INACTIVE_DAYS = 60;

export type SignInMethod = 'token' | 'oidc' | 'oauth' | 'saml' | 'local';
export type MfaKind = 'totp' | 'webauthn' | 'idp';

/**
 * Inactive: an enabled account whose latest activity (or, never active, its creation) is more than
 * 60 days old. Someone added last week who has not signed in yet is not inactive.
 */
export function isInactive(view: UserView, now: number): boolean {
  if (view.state !== 'active' || !view.user) return false;
  const since = view.lastActive ?? view.user.metadata.created_at;
  if (!since) return false;
  const t = new Date(since).getTime();
  return !Number.isNaN(t) && now - t > INACTIVE_DAYS * DAY;
}

const SIGN_IN_LABEL: Record<SignInMethod, string> = {
  oidc: 'Single sign-on',
  saml: 'Single sign-on (SAML)',
  oauth: 'GitHub',
  local: 'Password',
  token: 'Admin token',
};

export function signInLabel(method: SignInMethod | null): string {
  return method ? SIGN_IN_LABEL[method] : 'Never signed in';
}

export function isSso(method: SignInMethod | null): boolean {
  return method === 'oidc' || method === 'saml' || method === 'oauth';
}

const MFA_LABEL: Record<MfaKind, string> = {
  totp: 'Authenticator app',
  webauthn: 'Security key',
  idp: 'Through single sign-on',
};

export function mfaLabel(kinds: MfaKind[]): string {
  return kinds.length === 0 ? 'Not set up' : kinds.map((k) => MFA_LABEL[k]).join(', ');
}

export interface Coverage {
  /** Active people (service accounts never sign in to the console). */
  people: number;
  /** Of them, those whose last sign-in was single sign-on. */
  sso: number;
  /** Of them, those with a second factor or an IdP-asserted one. */
  mfa: number;
  /** People per factor kind; a person with two kinds counts in both. */
  kinds: Record<MfaKind | 'none', number>;
}

/** SSO and two-step coverage over the active people of the directory. */
export function coverage(users: UserView[]): Coverage {
  const people = users.filter((u) => u.state === 'active' && u.kind === 'human' && u.user);
  const kinds: Coverage['kinds'] = { totp: 0, webauthn: 0, idp: 0, none: 0 };
  for (const u of people) {
    if (u.mfa.length === 0) kinds.none += 1;
    for (const k of u.mfa) kinds[k] += 1;
  }
  return {
    people: people.length,
    sso: people.filter((u) => isSso(u.signIn)).length,
    mfa: people.filter((u) => u.mfa.length > 0).length,
    kinds,
  };
}

/* ---------------------------------------------------------------- permissions */

const RESOURCE_LABEL: Record<string, string> = {
  organizations: 'Organizations',
  teams: 'Teams',
  projects: 'Projects',
  users: 'Users',
  keys: 'API keys',
  role_bindings: 'Roles of users',
  providers: 'Providers',
  deployments: 'Deployments',
  models: 'Models',
  pricing: 'Prices',
  plugins: 'Plugins',
  mcp: 'MCP servers',
  security: 'Security settings',
  policies: 'Policies and limits',
  budgets: 'Budgets',
  health: 'Health',
  cluster: 'Cluster',
  node: 'Nodes',
  config: 'Configuration',
  logs: 'Request log',
  analytics: 'Analytics',
  audit: 'Audit log',
  metrics: 'Metrics',
  cache: 'Cache',
  playground: 'Playground',
  alerts: 'Alerts',
  notifications: 'Notifications',
  safety: 'Safety',
};

const ACTION_VERB: Record<string, string> = {
  read: 'see',
  create: 'create',
  update: 'change',
  write: 'change',
  delete: 'delete',
  rotate: 'rotate',
  grant_permissions: 'grant key permissions',
  adjust: 'raise and approve',
  drain: 'drain',
  apply: 'apply',
  read_content: 'read prompts and responses',
  admin: 'manage',
  send: 'send',
  review: 'review',
};

const ACTION_ORDER = Object.keys(ACTION_VERB);

export function resourceLabel(resource: string): string {
  return RESOURCE_LABEL[resource] ?? resource.replace(/_/g, ' ');
}

export function actionVerb(permission: string): string {
  const [resource = '', action = ''] = permission.split(':');
  if (permission === 'playground:send') return 'send requests';
  if (permission === 'logs:delete') return 'erase';
  if (resource === 'cache' && action === 'admin') return 'clear and configure';
  return ACTION_VERB[action] ?? action.replace(/_/g, ' ');
}

export interface PermissionGroup {
  resource: string;
  label: string;
  permissions: string[];
}

/** Permissions grouped by what they act on, in the catalogue's order of actions. */
export function groupPermissions(permissions: Iterable<string>): PermissionGroup[] {
  const by = new Map<string, string[]>();
  for (const p of permissions) {
    const resource = p.split(':')[0] ?? p;
    by.set(resource, [...(by.get(resource) ?? []), p]);
  }
  const order = Object.keys(RESOURCE_LABEL);
  return [...by.entries()]
    .sort(([a], [b]) => rank(order, a) - rank(order, b))
    .map(([resource, list]) => ({
      resource,
      label: resourceLabel(resource),
      permissions: [...new Set(list)].sort(
        (a, b) => rank(ACTION_ORDER, a.split(':')[1] ?? '') - rank(ACTION_ORDER, b.split(':')[1] ?? ''),
      ),
    }));
}

function rank(order: string[], value: string): number {
  const i = order.indexOf(value);
  return i === -1 ? order.length : i;
}

export interface EffectiveScope {
  scope: string;
  label: string;
  roles: string[];
  groups: PermissionGroup[];
  /** A role the gateway does not list (deleted since it was given): its permissions are unknown. */
  unknownRoles: string[];
}

/**
 * What a user may do, per scope: the union of the permissions of the roles bound there. A binding
 * at the whole gateway covers every organization, so lower scopes list only what it does not.
 */
export function effectivePermissions(
  bindings: { role: string; scope: string }[],
  roles: Role[],
): EffectiveScope[] {
  const byName = new Map(roles.map((r) => [r.name, r]));
  const scopes = new Map<string, { roles: string[]; permissions: Set<string>; unknown: string[] }>();
  for (const b of bindings) {
    const entry = scopes.get(b.scope) ?? { roles: [], permissions: new Set<string>(), unknown: [] };
    entry.roles.push(b.role);
    const role = byName.get(b.role);
    if (role) for (const p of role.permissions) entry.permissions.add(p);
    else entry.unknown.push(b.role);
    scopes.set(b.scope, entry);
  }
  const gateway = scopes.get('gateway')?.permissions ?? new Set<string>();
  return [...scopes.entries()]
    .sort(([a], [b]) => (a === 'gateway' ? -1 : b === 'gateway' ? 1 : a.localeCompare(b)))
    .map(([scope, entry]) => ({
      scope,
      label: scopeLabel(scope),
      roles: entry.roles,
      unknownRoles: entry.unknown,
      groups: groupPermissions(
        scope === 'gateway' ? entry.permissions : [...entry.permissions].filter((p) => !gateway.has(p)),
      ),
    }));
}

/** Every permission a custom role may hold: the union of the built-in roles' permissions. */
export function permissionCatalogue(roles: Role[]): string[] {
  return [...new Set(roles.filter((r) => r.builtin).flatMap((r) => r.permissions))];
}

/* ---------------------------------------------------------------- sessions */

/** "Safari on macOS" from a user agent, or the agent itself when it is not a browser we know. */
export function describeAgent(agent: string | undefined): string {
  if (!agent) return 'Unknown browser';
  const has = (...marks: string[]) => marks.some((m) => agent.includes(m));
  const browser = has('Edg/')
    ? 'Edge'
    : has('Firefox/')
      ? 'Firefox'
      : has('Chrome/')
        ? 'Chrome'
        : has('Safari/')
          ? 'Safari'
          : null;
  const os = has('iPhone', 'iPad')
    ? 'iOS'
    : has('Android')
      ? 'Android'
      : has('Mac OS X', 'Macintosh')
        ? 'macOS'
        : has('Windows')
          ? 'Windows'
          : has('Linux')
            ? 'Linux'
            : null;
  if (!browser) return agent.length > 40 ? `${agent.slice(0, 40)}…` : agent;
  return os ? `${browser} on ${os}` : browser;
}

export function sessionMethod(session: Session): string {
  return signInLabel(session.method);
}

/* ---------------------------------------------------------------- activity */

/** The identity events of an organization's activity feed, as audit action prefixes. */
export const ACTIVITY_ACTIONS = [
  'users.*',
  'invitations.*',
  'role_bindings.*',
  'keys.rotate',
  'keys.anomaly_*',
];

const ACTIVITY_LABEL: Record<string, string> = {
  'users.create': 'Added',
  'users.replace': 'Edited',
  'users.update': 'Edited',
  'users.patch': 'Edited',
  'users.delete': 'Deleted',
  'users.offboard': 'Offboarded',
  'users.reset_mfa': 'Two-step verification reset',
  'users.sessions.revoke': 'Signed out everywhere',
  'users.tokens.create': 'Admin token created',
  'users.tokens.delete': 'Admin token revoked',
  'invitations.create': 'Invited',
  'invitations.resend': 'Invitation sent again',
  'invitations.delete': 'Invitation cancelled',
  'invitations.accept': 'Joined',
  'role_bindings.create': 'Role given',
  'role_bindings.delete': 'Role removed',
  'keys.rotate': 'Key rotated',
  'keys.anomaly_flagged': 'Flagged for unusual key activity',
  'keys.anomaly_dismissed': 'Flag dismissed',
};

export type ActivityTone = 'info' | 'success' | 'warn' | 'error';

/** "Developer", "Team admin": a role id as the console names roles. */
function roleWords(role: string): string {
  const t = role.replace(/_/g, ' ');
  return t.charAt(0).toUpperCase() + t.slice(1);
}

/** "team/data-science" → "team data-science"; the gateway scope reads "the gateway". */
function scopeWords(scope: string): string {
  return scope === 'gateway' ? 'the gateway' : scope.replace('/', ' ');
}

/**
 * The bindings a role write added or removed, from the user's spec before and after (a user's
 * bindings are part of the user), with whose they are.
 */
function bindingChange(e: AuditEntry): { user: string; added: string[]; removed: string[] } | null {
  const changes = e.changes as unknown;
  if (!Array.isArray(changes) || changes.length !== 1) return null;
  const c = changes[0] as { path?: string; before?: unknown; after?: unknown };
  const user = /^users\.(.+)$/.exec(c.path ?? '')?.[1];
  if (!user) return null;
  const list = (v: unknown) =>
    (((v as { role_bindings?: unknown } | null)?.role_bindings ?? []) as { role?: string; scope?: string }[])
      .filter((b) => typeof b.role === 'string' && typeof b.scope === 'string')
      .map((b) => `${roleWords(b.role ?? '')} on ${scopeWords(b.scope ?? '')}`);
  const before = list(c.before);
  const after = list(c.after);
  return {
    user,
    added: after.filter((b) => !before.includes(b)),
    removed: before.filter((b) => !after.includes(b)),
  };
}

export function activityLabel(e: AuditEntry): string {
  if (e.action === 'role_bindings.create' || e.action === 'role_bindings.delete') {
    const b = bindingChange(e);
    const [first] = e.action === 'role_bindings.create' ? (b?.added ?? []) : (b?.removed ?? []);
    if (b && first) {
      const [role, scope] = first.split(' on ');
      return e.action === 'role_bindings.create'
        ? `Role ${role} on ${scope} given to`
        : `Role ${role} on ${scope} removed from`;
    }
  }
  return ACTIVITY_LABEL[e.action] ?? auditEntryLabel(e);
}

export function activityTone(e: AuditEntry): ActivityTone {
  if (e.outcome !== 'success') return 'error';
  if (e.action.startsWith('keys.anomaly_flagged')) return 'warn';
  if (['users.offboard', 'users.delete', 'role_bindings.delete', 'invitations.delete'].includes(e.action))
    return 'error';
  if (['invitations.accept', 'users.create', 'role_bindings.create'].includes(e.action)) return 'success';
  return 'info';
}

/** The object of an entry: a user, an invited email, a key. */
export function activitySubject(e: AuditEntry): { kind: 'user' | 'key' | 'other'; name: string } {
  const resource = e.resource ?? '';
  const user = /^users\/([^/]+)/.exec(resource);
  if (user?.[1]) return { kind: 'user', name: decodeURIComponent(user[1]) };
  const key = /^keys\/([^/]+)/.exec(resource);
  if (key?.[1]) return { kind: 'key', name: key[1] };
  const binding = resource.startsWith('role_bindings/') ? bindingChange(e) : null;
  if (binding) return { kind: 'user', name: binding.user };
  const changes = e.changes as unknown;
  if (Array.isArray(changes)) {
    const after = (changes[0] as { after?: { principal?: { id?: string }; role?: string } } | undefined)
      ?.after;
    if (after?.principal?.id) return { kind: 'user', name: after.principal.id };
  }
  return { kind: 'other', name: resource };
}

/** Users offboarded or deleted in the last `days` days, from the audit log (distinct users). */
export function offboardedSince(entries: AuditEntry[], now: number, days = 30): number {
  const since = now - days * DAY;
  const users = new Set<string>();
  for (const e of entries) {
    if (e.outcome !== 'success') continue;
    if (e.action !== 'users.offboard' && e.action !== 'users.delete') continue;
    if (new Date(e.time).getTime() < since) continue;
    users.add(e.resource ?? e.id);
  }
  return users.size;
}
