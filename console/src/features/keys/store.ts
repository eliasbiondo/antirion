import { create } from 'zustand';

import type {
  Anomaly,
  AuditEntry,
  BudgetCounter,
  KeyResource,
  KeySpec,
  OrganizationResource,
  ProjectResource,
  TeamResource,
  UserResource,
} from '@/lib/api/types';
import { createCollectionStore } from '@/lib/collection-store';
import { asApiError, emptySlot, loadSlot, type Slot } from '@/lib/slot';

import { type KeyCreateInput, keysApi, keysCollection, type LimitCounter } from './api';
import { hasGatewayPermission, useSession } from '@/stores/session';

function mayOnGateway(permission: string): boolean {
  return hasGatewayPermission(useSession.getState().me, permission);
}

export interface Hourly {
  start: string;
  requests: number;
  cost: number;
}

export interface SourceIp {
  ip: string;
  country: string | null;
  requests: number;
  errors: number;
}

type AuthSettings = Awaited<ReturnType<typeof keysApi.auth>>;

/** The key list and its writes. */
export const useKeys = createCollectionStore<KeyResource, KeyCreateInput, KeySpec>(keysCollection);

interface KeysContextState {
  projects: Slot<ProjectResource[]>;
  teams: Slot<TeamResource[]>;
  users: Slot<UserResource[]>;
  budgets: Slot<BudgetCounter[]>;
  usage: Slot<Map<string, { requests: number; cost: number }>>;
  anomalies: Slot<Anomaly[]>;
  /** Rotations made in this session, for gateways whose key metadata does not report them yet. */
  rotations: Record<string, { previous_kid: string; valid_until: string }>;
  audit: Record<string, Slot<AuditEntry[]>>;
  /** Every key's requests and spend per hour over the last 48 hours. */
  hourly: Slot<Hourly[]>;
  auth: Slot<AuthSettings>;
  organizations: Slot<OrganizationResource[]>;
  activity: Slot<AuditEntry[]>;
  regions: Slot<string[]>;
  /** Per key: its hours, rate-limit counters and source addresses. */
  keyHourly: Record<string, Slot<Hourly[]>>;
  limits: Record<string, Slot<LimitCounter[]>>;
  ips: Record<string, Slot<SourceIp[]>>;
  loadHourly: () => Promise<void>;
  loadSettings: () => Promise<void>;
  loadActivity: () => Promise<void>;
  loadRegions: () => Promise<void>;
  loadKeyFacts: (name: string) => Promise<void>;
  saveKeyPolicy: (organization: string, policy: Record<string, unknown>, reason?: string) => Promise<void>;
  loadDirectory: () => Promise<void>;
  loadBudgets: () => Promise<void>;
  loadUsage: (from: string, to: string) => Promise<void>;
  loadAudit: (name: string) => Promise<void>;
  loadAnomalies: () => Promise<void>;
  dismissAnomaly: (id: string) => Promise<void>;
}

/** What the keys screen shows around a key: its project and team, owners, budgets, usage, audit. */
export const useKeysContext = create<KeysContextState>()((set, get) => ({
  projects: emptySlot(),
  teams: emptySlot(),
  users: emptySlot(),
  budgets: emptySlot(),
  usage: emptySlot(),
  anomalies: emptySlot(),
  rotations: {},
  audit: {},
  hourly: emptySlot(),
  auth: emptySlot(),
  organizations: emptySlot(),
  activity: emptySlot(),
  regions: emptySlot(),
  keyHourly: {},
  limits: {},
  ips: {},

  loadHourly: () => {
    const to = new Date();
    const from = new Date(to.getTime() - 48 * 3_600_000);
    return loadSlot(set, get, 'hourly', () => keysApi.hourly(from.toISOString(), to.toISOString()), {
      quiet: true,
    });
  },
  async loadSettings() {
    await Promise.all([
      // The gateway's sign-in settings are read on the whole gateway only.
      mayOnGateway('security:read') ? loadSlot(set, get, 'auth', keysApi.auth) : null,
      loadSlot(set, get, 'organizations', keysApi.organizations),
    ]);
  },
  loadActivity: () => loadSlot(set, get, 'activity', keysApi.activity),
  loadRegions: () => loadSlot(set, get, 'regions', keysApi.regions),
  async loadKeyFacts(name) {
    const to = new Date();
    const from48 = new Date(to.getTime() - 48 * 3_600_000).toISOString();
    const from24 = new Date(to.getTime() - 24 * 3_600_000).toISOString();
    await Promise.all([
      loadRecord(set, 'keyHourly', name, () => keysApi.hourly(from48, to.toISOString(), name)),
      loadRecord(set, 'limits', name, () => keysApi.limits(name)),
      loadRecord(set, 'ips', name, () => keysApi.topIps(name, from24, to.toISOString())),
    ]);
  },
  async saveKeyPolicy(organization, policy, reason) {
    const current = await keysApi.organization(organization);
    await keysApi.setKeyPolicy(organization, policy, current.metadata.etag, reason);
    await loadSlot(set, get, 'organizations', keysApi.organizations, { quiet: true });
  },

  async loadDirectory() {
    await Promise.all([
      loadSlot(set, get, 'projects', keysApi.projects),
      loadSlot(set, get, 'teams', keysApi.teams),
      mayOnGateway('users:read') ? loadSlot(set, get, 'users', keysApi.users) : null,
    ]);
  },
  loadBudgets: () => loadSlot(set, get, 'budgets', keysApi.budgets),
  loadAnomalies: () => loadSlot(set, get, 'anomalies', keysApi.anomalies),
  async dismissAnomaly(id) {
    await keysApi.dismissAnomaly(id);
    set((s) => ({
      anomalies: { ...s.anomalies, data: (s.anomalies.data ?? []).filter((a) => a.id !== id) },
    }));
  },
  loadUsage: (from, to) => loadSlot(set, get, 'usage', () => keysApi.usage24h(from, to), { quiet: true }),
  async loadAudit(name) {
    set((s) => ({ audit: { ...s.audit, [name]: { ...(s.audit[name] ?? emptySlot()), status: 'loading' } } }));
    try {
      const data = await keysApi.audit(name);
      set((s) => ({ audit: { ...s.audit, [name]: { data, status: 'ready', error: null, at: Date.now() } } }));
    } catch (error) {
      set((s) => ({
        audit: { ...s.audit, [name]: { data: null, status: 'error', error: asApiError(error), at: null } },
      }));
    }
  },
}));

type RecordKey = 'keyHourly' | 'limits' | 'ips';

/** Loads one key's entry of a per-key record of slots. */
async function loadRecord<K extends RecordKey>(
  set: (fn: (s: KeysContextState) => Partial<KeysContextState>) => void,
  key: K,
  name: string,
  read: () => Promise<NonNullable<KeysContextState[K][string]['data']>>,
): Promise<void> {
  set((s) => ({
    [key]: { ...s[key], [name]: { ...(s[key][name] ?? emptySlot()), status: 'loading' } },
  }));
  try {
    const data = await read();
    set((s) => ({
      [key]: { ...s[key], [name]: { data, status: 'ready', error: null, at: Date.now() } },
    }));
  } catch (error) {
    set((s) => ({
      [key]: { ...s[key], [name]: { data: null, status: 'error', error: asApiError(error), at: null } },
    }));
  }
}

/** Rotation and enable/disable, which refresh the key in the list. */
export async function rotateKey(name: string, gracePeriod: string, reason?: string): Promise<string> {
  const rotated = await keysApi.rotate(name, gracePeriod, reason);
  const previous = rotated.previous;
  if (previous)
    useKeysContext.setState((s) => ({
      rotations: {
        ...s.rotations,
        [name]: { previous_kid: previous.kid, valid_until: previous.valid_until },
      },
    }));
  await useKeys.getState().fetch({ quiet: true });
  return rotated.secret;
}

export async function setKeyEnabled(name: string, enabled: boolean, reason?: string): Promise<void> {
  const current = useKeys.getState().byName(name);
  const updated = await keysApi.setEnabled(name, enabled, current?.metadata.etag, reason);
  useKeys.setState((s) => ({ items: s.items.map((k) => (k.name === name ? updated : k)) }));
}

/** Creates a key and returns its secret, which the caller shows once and drops. */
export async function createKey(input: KeyCreateInput): Promise<string> {
  const created = await keysCollection.createWithSecret(input);
  useKeys.setState((s) => ({
    items: [
      ...s.items.filter((k) => k.name !== created.name),
      { name: created.name, spec: created.spec, metadata: created.metadata },
    ].sort((a, b) => a.name.localeCompare(b.name)),
  }));
  return created.secret ?? '';
}
