import { useCallback, useEffect } from 'react';
import { create } from 'zustand';

import { api, listAll } from '@/lib/api/client';

import { hasGatewayPermission, useSession } from './session';

interface ProviderTypesState {
  /** Provider name → provider type, once read. */
  types: Map<string, string> | null;
  loading: boolean;
  load: () => Promise<void>;
}

/** The type of each configured provider, read once per page load, for provider marks. */
export const useProviderTypes = create<ProviderTypesState>()((set, get) => ({
  types: null,
  loading: false,
  async load() {
    if (get().types || get().loading) return;
    // Without the right to list providers, a name is taken as its type: no refused read.
    if (!hasGatewayPermission(useSession.getState().me, 'providers:read')) {
      set({ types: new Map() });
      return;
    }
    set({ loading: true });
    try {
      const items = await listAll((cursor) => api.get('/providers', { query: { limit: 200, cursor } }));
      set({ types: new Map(items.map((p) => [p.name, p.spec.type])), loading: false });
    } catch {
      set({ types: new Map(), loading: false });
    }
  },
}));

/**
 * A function from a provider's name to its type, for its mark. A name that is not a configured
 * provider is taken as a type, as records of a provider since removed still name it.
 */
export function useProviderType(): (name: string | null | undefined) => string | undefined {
  const types = useProviderTypes((s) => s.types);
  const load = useProviderTypes((s) => s.load);
  useEffect(() => {
    void load();
  }, [load]);
  return useCallback((name) => (name ? (types?.get(name) ?? name) : undefined), [types]);
}

/** The names of the configured providers, or null until they are read (or when they cannot be). */
export function useConfiguredProviders(): Set<string> | null {
  const types = useProviderTypes((s) => s.types);
  const load = useProviderTypes((s) => s.load);
  useEffect(() => {
    void load();
  }, [load]);
  return types && types.size > 0 ? new Set(types.keys()) : null;
}
