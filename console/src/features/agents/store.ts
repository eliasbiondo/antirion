import { create } from 'zustand';

import { emptySlot, loadSlot, type Slot } from '@/lib/slot';

import { type AgentResource, type AgentSpec, agentsApi } from './api';

interface AgentsState {
  agents: Slot<AgentResource[]>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  save: (name: string, spec: AgentSpec, existing: AgentResource | null) => Promise<void>;
  setEnabled: (name: string, enabled: boolean) => Promise<void>;
  remove: (name: string, reason?: string) => Promise<void>;
}

/** The A2A agents. */
export const useAgents = create<AgentsState>()((set, get) => ({
  agents: emptySlot(),

  async load(options) {
    await loadSlot(set, get, 'agents', agentsApi.list, options);
  },

  async save(name, spec, existing) {
    const saved = existing
      ? await agentsApi.replace(name, spec, existing.metadata.etag)
      : await agentsApi.create(name, spec);
    set((s) => ({
      agents: {
        ...s.agents,
        data: [...(s.agents.data ?? []).filter((a) => a.name !== name), saved].sort((a, b) =>
          a.name.localeCompare(b.name),
        ),
      },
    }));
  },

  async setEnabled(name, enabled) {
    const current = get().agents.data?.find((a) => a.name === name);
    await agentsApi.setEnabled(name, enabled, current?.metadata.etag);
    await loadSlot(set, get, 'agents', agentsApi.list, { quiet: true });
  },

  async remove(name, reason) {
    await agentsApi.remove(name, reason);
    set((s) => ({ agents: { ...s.agents, data: (s.agents.data ?? []).filter((a) => a.name !== name) } }));
  },
}));
