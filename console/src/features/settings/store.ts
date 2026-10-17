import { create } from 'zustand';

import type {
  ApplyResult,
  AuditEntry,
  DeploymentHealth,
  DiffResult,
  Health,
  OrganizationResource,
  ProviderResource,
} from '@/lib/api/types';
import { asApiError, emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  type AuditQuery,
  type GovernanceSettings,
  type AuditSinkStatus,
  type ConfigDoc,
  type NodeInfo,
  type ProviderStatus,
  settingsApi,
} from './api';

type SettingsResource = Awaited<ReturnType<typeof settingsApi.authSettings>>;

interface ConfigState {
  document: Slot<{ doc: ConfigDoc; etag: string | null }>;
  draft: string;
  parseError: string | null;
  diff: Slot<DiffResult>;
  applied: Slot<ApplyResult>;
  loadDocument: () => Promise<void>;
  setDraft: (text: string) => void;
  runDiff: () => Promise<void>;
  apply: (reason?: string) => Promise<ApplyResult | null>;
}

function parseDraft(text: string): { doc: ConfigDoc | null; error: string | null } {
  try {
    const value: unknown = JSON.parse(text);
    if (typeof value !== 'object' || value === null || Array.isArray(value))
      return { doc: null, error: 'The document must be a JSON object.' };
    return { doc: value as ConfigDoc, error: null };
  } catch (e) {
    return { doc: null, error: e instanceof Error ? e.message : 'Invalid JSON' };
  }
}

/** The configuration document: export, edit as JSON, diff against the running generation, apply. */
export const useConfigDoc = create<ConfigState>()((set, get) => ({
  document: emptySlot(),
  draft: '',
  parseError: null,
  diff: emptySlot(),
  applied: emptySlot(),

  async loadDocument() {
    await loadSlot(set, get, 'document', settingsApi.exportConfig);
    const doc = get().document.data?.doc;
    if (doc)
      set({ draft: JSON.stringify(doc, null, 2), parseError: null, diff: emptySlot(), applied: emptySlot() });
  },

  setDraft(text) {
    set({ draft: text, parseError: parseDraft(text).error, diff: emptySlot() });
  },

  async runDiff() {
    const { doc, error } = parseDraft(get().draft);
    if (!doc) {
      set({ parseError: error });
      return;
    }
    await loadSlot(set, get, 'diff', () => settingsApi.diff(doc));
  },

  async apply(reason) {
    const { doc, error } = parseDraft(get().draft);
    if (!doc) {
      set({ parseError: error });
      return null;
    }
    set({ applied: { ...emptySlot(), status: 'loading' } });
    try {
      const result = await settingsApi.apply(doc, get().document.data?.etag ?? null, reason);
      set({ applied: { data: result, status: 'ready', error: null, at: Date.now() } });
      await get().loadDocument();
      return result;
    } catch (e) {
      set({ applied: { data: null, status: 'error', error: asApiError(e), at: null } });
      throw e;
    }
  },
}));

interface AuditState {
  query: AuditQuery;
  entries: Slot<AuditEntry[]>;
  cursor: string | null;
  setQuery: (query: AuditQuery) => void;
  load: () => Promise<void>;
  loadMore: () => Promise<void>;
}

/** The audit log with its filters and cursor; reads are left out on the server unless asked for. */
export const useAudit = create<AuditState>()((set, get) => ({
  query: { kind: 'write' },
  entries: emptySlot(),
  cursor: null,
  setQuery(query) {
    set({ query });
  },
  async load() {
    let cursor: string | null = null;
    await loadSlot(set, get, 'entries', async () => {
      const page = await settingsApi.audit(get().query);
      cursor = page.next_cursor ?? null;
      return page.items;
    });
    set({ cursor });
  },
  async loadMore() {
    const cursor = get().cursor;
    if (!cursor) return;
    const page = await settingsApi.audit(get().query, cursor);
    set((s) => ({
      entries: { ...s.entries, data: [...(s.entries.data ?? []), ...page.items] },
      cursor: page.next_cursor ?? null,
    }));
  },
}));

interface RuntimeState {
  health: Slot<Health>;
  deployments: Slot<DeploymentHealth[]>;
  providerStatus: Slot<ProviderStatus[]>;
  nodes: Slot<NodeInfo[]>;
  providers: Slot<ProviderResource[]>;
  auditSinks: Slot<AuditSinkStatus[]>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  loadProviders: () => Promise<void>;
  resetDeployment: (name: string) => Promise<void>;
  setProviderEnabled: (name: string, enabled: boolean) => Promise<void>;
}

/** Runtime health: dependencies, admission, nodes, deployments and providers. */
export const useRuntime = create<RuntimeState>()((set, get) => ({
  health: emptySlot(),
  deployments: emptySlot(),
  providerStatus: emptySlot(),
  nodes: emptySlot(),
  providers: emptySlot(),
  auditSinks: emptySlot(),
  async load(options) {
    await Promise.all([
      loadSlot(set, get, 'health', settingsApi.health, options),
      loadSlot(set, get, 'deployments', settingsApi.deployments, options),
      loadSlot(set, get, 'providerStatus', settingsApi.providerStatus, options),
      loadSlot(set, get, 'nodes', settingsApi.nodes, options),
    ]);
  },
  async loadProviders() {
    await Promise.all([
      loadSlot(set, get, 'providers', settingsApi.providers),
      loadSlot(set, get, 'providerStatus', settingsApi.providerStatus),
      loadSlot(set, get, 'auditSinks', settingsApi.auditSinks),
    ]);
  },
  async resetDeployment(name) {
    await settingsApi.resetDeployment(name);
    await loadSlot(set, get, 'deployments', settingsApi.deployments, { quiet: true });
  },
  async setProviderEnabled(name, enabled) {
    const p = get().providers.data?.find((x) => x.name === name);
    await settingsApi.setProviderEnabled(name, enabled, p?.metadata.etag);
    await loadSlot(set, get, 'providers', settingsApi.providers, { quiet: true });
  },
}));

interface OrgSettingsState {
  organization: Slot<OrganizationResource>;
  auth: Slot<SettingsResource>;
  observability: Slot<Awaited<ReturnType<typeof settingsApi.observabilitySettings>>>;
  limits: Slot<Awaited<ReturnType<typeof settingsApi.limitsSettings>>>;
  security: Slot<Awaited<ReturnType<typeof settingsApi.securitySettings>>>;
  loadOrganization: (name: string) => Promise<void>;
  saveOrganization: (
    name: string,
    spec: Partial<OrganizationResource['spec']>,
    reason?: string,
  ) => Promise<void>;
  loadSettings: () => Promise<void>;
  saveAuth: (spec: Record<string, unknown>, reason?: string) => Promise<void>;
  saveObservability: (spec: Record<string, unknown>, reason?: string) => Promise<void>;
  saveSecurity: (spec: Record<string, unknown>, reason?: string) => Promise<void>;
}

/** The settings singletons and the scope's organization, as the Settings sections read and write them. */
export const useOrgSettings = create<OrgSettingsState>()((set, get) => ({
  organization: emptySlot(),
  auth: emptySlot(),
  observability: emptySlot(),
  limits: emptySlot(),
  security: emptySlot(),
  loadOrganization: (name) => loadSlot(set, get, 'organization', () => settingsApi.organization(name)),
  async saveOrganization(name, spec, reason) {
    const current = get().organization.data;
    await settingsApi.patchOrganization(
      name,
      spec,
      current?.name === name ? current.metadata.etag : undefined,
      reason,
    );
    await get().loadOrganization(name);
  },
  async loadSettings() {
    await Promise.all([
      loadSlot(set, get, 'auth', settingsApi.authSettings),
      loadSlot(set, get, 'observability', settingsApi.observabilitySettings),
      loadSlot(set, get, 'limits', settingsApi.limitsSettings),
      loadSlot(set, get, 'security', settingsApi.securitySettings),
    ]);
  },
  async saveAuth(spec, reason) {
    await settingsApi.patchAuthSettings(spec, get().auth.data?.metadata.etag, reason);
    await loadSlot(set, get, 'auth', settingsApi.authSettings, { quiet: true });
  },
  async saveObservability(spec, reason) {
    await settingsApi.patchObservabilitySettings(spec, get().observability.data?.metadata.etag, reason);
    await loadSlot(set, get, 'observability', settingsApi.observabilitySettings, { quiet: true });
  },
  async saveSecurity(spec, reason) {
    await settingsApi.patchSecurity(spec, get().security.data?.metadata.etag, reason);
    await loadSlot(set, get, 'security', settingsApi.securitySettings, { quiet: true });
  },
}));

type Roles = Awaited<ReturnType<typeof settingsApi.roles>>;
type Named = { name: string }[];

interface DirectoryState {
  users: Slot<Awaited<ReturnType<typeof settingsApi.users>>>;
  roles: Slot<Roles>;
  /** Every scope a role binding can name: the gateway, organizations, teams and projects. */
  scopes: Slot<string[]>;
  loadUsers: () => Promise<void>;
  loadRoles: () => Promise<void>;
}

const names = async (read: () => Promise<Named>, kind: string): Promise<string[]> => {
  try {
    return (await read()).map((r) => `${kind}/${r.name}`);
  } catch {
    return [];
  }
};

/** People (for sign-in coverage and counts per provider), roles and binding scopes. */
export const useDirectory = create<DirectoryState>()((set, get) => ({
  users: emptySlot(),
  roles: emptySlot(),
  scopes: emptySlot(),
  loadUsers: () => loadSlot(set, get, 'users', settingsApi.users),
  async loadRoles() {
    await Promise.all([
      loadSlot(set, get, 'roles', settingsApi.roles),
      loadSlot(set, get, 'scopes', async () => [
        'gateway',
        ...(await names(settingsApi.organizations, 'organization')),
        ...(await names(settingsApi.teams, 'team')),
        ...(await names(settingsApi.projects, 'project')),
      ]),
    ]);
  },
}));

type Smtp = Awaited<ReturnType<typeof settingsApi.smtp>>;

interface LinksState {
  smtp: Slot<Smtp>;
  governance: Slot<GovernanceSettings>;
  /** The newest configuration writes, for "changed by … 2 h ago" on each connection. */
  writes: Slot<AuditEntry[]>;
  loadSmtp: () => Promise<void>;
  saveSmtp: (spec: Record<string, unknown>) => Promise<void>;
  loadGovernance: () => Promise<void>;
  saveGovernance: (spec: Record<string, unknown>, reason?: string) => Promise<void>;
  loadWrites: () => Promise<void>;
}

/** Outgoing mail, audit copies (in the governance settings) and recent writes, for the integrations pages. */
export const useLinks = create<LinksState>()((set, get) => ({
  smtp: emptySlot(),
  governance: emptySlot(),
  writes: emptySlot(),
  loadSmtp: () => loadSlot(set, get, 'smtp', settingsApi.smtp),
  async saveSmtp(spec) {
    await settingsApi.patchSmtp(spec, get().smtp.data?.metadata.etag);
    await loadSlot(set, get, 'smtp', settingsApi.smtp, { quiet: true });
  },
  loadGovernance: () => loadSlot(set, get, 'governance', settingsApi.governance),
  async saveGovernance(spec, reason) {
    await settingsApi.patchGovernance(spec, get().governance.data?.metadata.etag, reason);
    await loadSlot(set, get, 'governance', settingsApi.governance, { quiet: true });
  },
  loadWrites: () => loadSlot(set, get, 'writes', settingsApi.recentWrites),
}));
