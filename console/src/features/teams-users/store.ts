import { create } from 'zustand';

import type {
  AdminToken,
  Anomaly,
  BudgetCounter,
  AuditEntry,
  Invitation,
  KeyResource,
  OrganizationResource,
  ProjectResource,
  Role,
  RoleBinding,
  ScimStatus,
  Session,
  TeamResource,
  TeamSpec,
  UserResource,
  UserSpec,
} from '@/lib/api/types';
import { createCollectionStore } from '@/lib/collection-store';
import { asApiError, emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  type AuthSettings,
  type CustomRoleSpec,
  directoryApi,
  type InvitationCreate,
  type OffboardRequest,
  organizationsCollection,
  projectsCollection,
  type RoleBindingCreate,
  teamsCollection,
  type UserUsage,
  usersCollection,
} from './api';
import type { UserView } from './types';

export const useUsers = createCollectionStore<UserResource, { name: string; spec: UserSpec }, UserSpec>(
  usersCollection,
);
export const useTeams = createCollectionStore<TeamResource, { name: string; spec: TeamSpec }, TeamSpec>(
  teamsCollection,
);
export const useProjects = createCollectionStore<
  ProjectResource,
  { name: string; spec: ProjectResource['spec'] },
  ProjectResource['spec']
>(projectsCollection);
export const useOrganizations = createCollectionStore<
  OrganizationResource,
  { name: string; spec: OrganizationResource['spec'] },
  OrganizationResource['spec']
>(organizationsCollection);

function keyedSlot<T>(data: T | null, error: unknown, status: Slot<T>['status']): Slot<T> {
  return {
    data,
    status,
    error: error === null ? null : asApiError(error),
    at: status === 'ready' ? Date.now() : null,
  };
}

interface DirectoryState {
  bindings: Slot<RoleBinding[]>;
  roles: Slot<Role[]>;
  invitations: Slot<Invitation[]>;
  keys: Slot<KeyResource[]>;
  teamSpend: Slot<BudgetCounter[]>;
  tokens: Record<string, Slot<AdminToken[]>>;
  audit: Record<string, Slot<AuditEntry[]>>;
  sessions: Record<string, Slot<Session[]>>;
  anomalies: Record<string, Slot<Anomaly[]>>;
  usage: Record<string, Slot<UserUsage>>;
  auth: Slot<AuthSettings>;
  scim: Slot<ScimStatus>;
  activity: Slot<AuditEntry[]>;
  offboardings: Slot<AuditEntry[]>;
  load: () => Promise<void>;
  loadSessions: (user: string) => Promise<void>;
  loadAnomalies: (user: string) => Promise<void>;
  loadUsage: (user: string, team: string | null) => Promise<void>;
  loadAuth: () => Promise<void>;
  loadScim: () => Promise<void>;
  loadActivity: (options?: { quiet?: boolean }) => Promise<void>;
  loadOffboardings: () => Promise<void>;
  dismissAnomaly: (user: string, id: string) => Promise<void>;
  /** Creates, replaces or (with `null`) removes a custom role, reading the settings' ETag first. */
  saveRole: (name: string, spec: CustomRoleSpec | null, reason?: string) => Promise<void>;
  loadTeamSpend: () => Promise<void>;
  loadTokens: (user: string) => Promise<void>;
  loadAudit: (user: string) => Promise<void>;
  addBinding: (body: RoleBindingCreate) => Promise<void>;
  removeBinding: (id: string, reason?: string) => Promise<void>;
  invite: (body: InvitationCreate) => Promise<string | null>;
  cancelInvitation: (id: string) => Promise<void>;
  resendInvitation: (id: string) => Promise<void>;
  createToken: (user: string, description: string, days: number | null) => Promise<string>;
  deleteToken: (user: string, kid: string) => Promise<void>;
}

/** The directory around users and teams: role bindings, roles, invitations, owned keys, tokens, activity. */
export const useDirectory = create<DirectoryState>()((set, get) => ({
  bindings: emptySlot(),
  roles: emptySlot(),
  invitations: emptySlot(),
  keys: emptySlot(),
  teamSpend: emptySlot(),
  tokens: {},
  audit: {},
  sessions: {},
  anomalies: {},
  usage: {},
  auth: emptySlot(),
  scim: emptySlot(),
  activity: emptySlot(),
  offboardings: emptySlot(),

  async load() {
    await Promise.all([
      loadSlot(set, get, 'bindings', directoryApi.bindings),
      loadSlot(set, get, 'roles', directoryApi.roles),
      loadSlot(set, get, 'invitations', directoryApi.invitations),
      loadSlot(set, get, 'keys', directoryApi.keys),
    ]);
  },

  loadTeamSpend: () => loadSlot(set, get, 'teamSpend', directoryApi.teamSpend),

  loadSessions: (user) =>
    loadKeyed(
      (f) => set((s) => ({ sessions: f(s.sessions) })),
      user,
      () => directoryApi.sessions(user),
    ),
  loadAnomalies: (user) =>
    loadKeyed(
      (f) => set((s) => ({ anomalies: f(s.anomalies) })),
      user,
      () => directoryApi.anomalies(user),
    ),
  loadUsage: (user, team) =>
    loadKeyed(
      (f) => set((s) => ({ usage: f(s.usage) })),
      user,
      () => directoryApi.usage(user, team, Date.now()),
    ),
  loadAuth: () => loadSlot(set, get, 'auth', directoryApi.authSettings),
  loadScim: () => loadSlot(set, get, 'scim', directoryApi.scimStatus),
  loadActivity: (options) => loadSlot(set, get, 'activity', () => directoryApi.activity(), options),
  loadOffboardings: () => loadSlot(set, get, 'offboardings', () => directoryApi.offboardings(Date.now())),

  async dismissAnomaly(user, id) {
    await directoryApi.dismissAnomaly(id);
    set((s) => ({
      anomalies: {
        ...s.anomalies,
        [user]: keyedSlot(
          (s.anomalies[user]?.data ?? []).filter((a) => a.id !== id),
          null,
          'ready',
        ),
      },
    }));
    await useUsers.getState().fetch({ quiet: true });
  },

  async saveRole(name, spec, reason) {
    const current = await directoryApi.authSettings();
    const saved = await directoryApi.setCustomRoles({ [name]: spec }, current.metadata.etag, reason);
    set({ auth: { data: saved, status: 'ready', error: null, at: Date.now() } });
    await loadSlot(set, get, 'roles', directoryApi.roles);
  },

  async loadTokens(user) {
    set((s) => ({
      tokens: { ...s.tokens, [user]: keyedSlot(s.tokens[user]?.data ?? null, null, 'loading') },
    }));
    try {
      const data = await directoryApi.tokens(user);
      set((s) => ({ tokens: { ...s.tokens, [user]: keyedSlot(data, null, 'ready') } }));
    } catch (error) {
      set((s) => ({ tokens: { ...s.tokens, [user]: keyedSlot<never[]>(null, error, 'error') } }));
    }
  },

  async loadAudit(user) {
    set((s) => ({ audit: { ...s.audit, [user]: keyedSlot(s.audit[user]?.data ?? null, null, 'loading') } }));
    try {
      const data = await directoryApi.audit(user);
      set((s) => ({ audit: { ...s.audit, [user]: keyedSlot(data, null, 'ready') } }));
    } catch (error) {
      set((s) => ({ audit: { ...s.audit, [user]: keyedSlot<never[]>(null, error, 'error') } }));
    }
  },

  async addBinding(body) {
    const created = await directoryApi.createBinding(body);
    set((s) => ({ bindings: { ...s.bindings, data: [...(s.bindings.data ?? []), created] } }));
    if (body.principal.type === 'user') await useUsers.getState().fetch({ quiet: true });
  },

  async removeBinding(id, reason) {
    await directoryApi.deleteBinding(id, reason);
    set((s) => ({ bindings: { ...s.bindings, data: (s.bindings.data ?? []).filter((b) => b.id !== id) } }));
    await useUsers.getState().fetch({ quiet: true });
  },

  async invite(body) {
    const created = await directoryApi.invite(body);
    await loadSlot(set, get, 'invitations', directoryApi.invitations);
    return created.link ?? null;
  },

  async cancelInvitation(id) {
    await directoryApi.cancelInvitation(id);
    set((s) => ({
      invitations: { ...s.invitations, data: (s.invitations.data ?? []).filter((i) => i.id !== id) },
    }));
  },

  async resendInvitation(id) {
    await directoryApi.resendInvitation(id);
    await loadSlot(set, get, 'invitations', directoryApi.invitations);
  },

  async createToken(user, description, days) {
    const created = await directoryApi.createToken(user, {
      ...(description ? { description } : {}),
      ...(days
        ? { expires_at: new Date(Date.now() + days * 86_400_000).toISOString().replace(/\.\d{3}Z$/, 'Z') }
        : {}),
    });
    const { token, ...listed } = created;
    set((s) => ({
      tokens: { ...s.tokens, [user]: keyedSlot([...(s.tokens[user]?.data ?? []), listed], null, 'ready') },
    }));
    return token;
  },

  async deleteToken(user, kid) {
    await directoryApi.deleteToken(user, kid);
    set((s) => ({
      tokens: {
        ...s.tokens,
        [user]: keyedSlot(
          (s.tokens[user]?.data ?? []).filter((t) => t.kid !== kid),
          null,
          'ready',
        ),
      },
    }));
  },
}));

/** Loads one entry of a keyed slot record (a user's sessions, flags or usage). */
async function loadKeyed<T>(
  update: (f: (record: Record<string, Slot<T>>) => Record<string, Slot<T>>) => void,
  key: string,
  fn: () => Promise<T>,
): Promise<void> {
  update((r) => ({ ...r, [key]: keyedSlot(r[key]?.data ?? null, null, 'loading') }));
  try {
    const data = await fn();
    update((r) => ({ ...r, [key]: keyedSlot(data, null, 'ready') }));
  } catch (error) {
    update((r) => ({ ...r, [key]: keyedSlot<T>(r[key]?.data ?? null, error, 'error') }));
  }
}

/** The viewer role that keeps a binding's reach: `gateway_viewer` at the gateway, else `viewer`. */
export function viewerFor(scope: string): string {
  return scope === 'gateway' ? 'gateway_viewer' : 'viewer';
}

/** User administration actions that are not plain edits. */
export const userActions = {
  revokeSessions: (user: string) => directoryApi.revokeSessions(user),
  resetMfa: (user: string, reason?: string) => directoryApi.resetMfa(user, reason),
  async offboard(user: string, body: OffboardRequest) {
    const result = await directoryApi.offboard(user, body);
    await useUsers.getState().fetch({ quiet: true });
    return result;
  },
  /**
   * Replaces each of a user's roles by the viewer role at the same scope (a user's bindings are
   * entries of the user, so each change is one binding created or deleted).
   */
  async downgradeToViewer(view: UserView, reason?: string): Promise<void> {
    const directory = useDirectory.getState();
    const held = new Set(view.bindings.map((b) => `${b.role}@${b.scope}`));
    for (const b of view.bindings) {
      const role = viewerFor(b.scope);
      if (b.role === role || !b.id) continue;
      if (!held.has(`${role}@${b.scope}`)) {
        await directory.addBinding({ principal: { type: 'user', id: view.name }, role, scope: b.scope });
        held.add(`${role}@${b.scope}`);
      }
      await directory.removeBinding(b.id, reason);
    }
  },
  /** Rotates every key the user owns, one call per key; each new secret is returned once. */
  async rotateOwnedKeys(user: string): Promise<{ key: string; secret: string }[]> {
    const keys = (useDirectory.getState().keys.data ?? []).filter((k) => k.spec.owner === user);
    const out: { key: string; secret: string }[] = [];
    for (const k of keys) {
      const rotated = await directoryApi.rotateKey(k.name, '24h');
      out.push({ key: k.name, secret: rotated.secret });
    }
    return out;
  },
};

/** Disabling or enabling a level of the hierarchy, as a merge patch of `enabled`. */
export async function setLevelEnabled(
  kind: 'organization' | 'project',
  name: string,
  enabled: boolean,
  reason?: string,
): Promise<void> {
  if (kind === 'organization') {
    const current = useOrganizations.getState().byName(name);
    const updated = await directoryApi.setOrganizationEnabled(name, enabled, current?.metadata.etag, reason);
    useOrganizations.setState((s) => ({ items: s.items.map((o) => (o.name === name ? updated : o)) }));
  } else {
    const current = useProjects.getState().byName(name);
    const updated = await directoryApi.setProjectEnabled(name, enabled, current?.metadata.etag, reason);
    useProjects.setState((s) => ({ items: s.items.map((p) => (p.name === name ? updated : p)) }));
  }
}
