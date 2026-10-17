import { create } from 'zustand';

import type { DeploymentHealth, DeploymentResource, ProviderResource, ProviderType } from '@/lib/api/types';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';

import { providersApi, type ProviderSpec, type ProviderStatus } from './api';

interface ProvidersState {
  providers: Slot<ProviderResource[]>;
  status: Slot<ProviderStatus[]>;
  types: Slot<ProviderType[]>;
  deployments: Slot<DeploymentResource[]>;
  health: Slot<DeploymentHealth[]>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  loadTypes: () => Promise<void>;
  create: (name: string, spec: ProviderSpec) => Promise<ProviderResource>;
  setEnabled: (name: string, enabled: boolean) => Promise<void>;
  remove: (name: string, reason?: string) => Promise<void>;
  resetKey: (provider: string, key: string) => Promise<void>;
}

/**
 * Providers with their credential state, the types a gateway supports, the deployments on each and
 * their health (the state of each provider key).
 */
export const useProviders = create<ProvidersState>()((set, get) => ({
  providers: emptySlot(),
  status: emptySlot(),
  types: emptySlot(),
  deployments: emptySlot(),
  health: emptySlot(),

  async load(options) {
    await Promise.all([
      loadSlot(set, get, 'providers', providersApi.list, options),
      loadSlot(set, get, 'status', providersApi.status, options),
      loadSlot(set, get, 'deployments', providersApi.deployments, options),
      loadSlot(set, get, 'health', providersApi.health, options),
    ]);
  },

  async loadTypes() {
    if (get().types.data) return;
    await loadSlot(set, get, 'types', providersApi.types);
  },

  async create(name, spec) {
    const created = await providersApi.create(name, spec);
    set((s) => ({
      providers: {
        ...s.providers,
        data: [...(s.providers.data ?? []), created].sort((a, b) => a.name.localeCompare(b.name)),
      },
    }));
    void loadSlot(set, get, 'status', providersApi.status, { quiet: true });
    return created;
  },

  async setEnabled(name, enabled) {
    const p = get().providers.data?.find((x) => x.name === name);
    await providersApi.setEnabled(name, enabled, p?.metadata.etag);
    await loadSlot(set, get, 'providers', providersApi.list, { quiet: true });
  },

  async resetKey(provider, key) {
    await providersApi.resetKey(provider, key);
    await loadSlot(set, get, 'health', providersApi.health, { quiet: true });
  },

  async remove(name, reason) {
    await providersApi.remove(name, reason);
    set((s) => ({
      providers: { ...s.providers, data: (s.providers.data ?? []).filter((p) => p.name !== name) },
    }));
  },
}));
