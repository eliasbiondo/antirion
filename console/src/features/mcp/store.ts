import { create } from 'zustand';

import { asApiError, emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  mcpApi,
  type McpGatewayResource,
  type McpGatewaySpec,
  type McpServerResource,
  type McpServerSpec,
  type McpServerStatus,
  type McpSessionStats,
  type McpSettings,
} from './api';

/** One server's status read: the answer, or why there is none (a server that can't be read). */
export interface StatusEntry {
  status: McpServerStatus | null;
  error: string | null;
}

interface McpState {
  servers: Slot<McpServerResource[]>;
  statuses: Record<string, StatusEntry>;
  gateways: Slot<McpGatewayResource[]>;
  sessions: Slot<McpSessionStats[]>;
  settings: Slot<McpSettings>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  loadStatuses: () => Promise<void>;
  loadStatus: (name: string) => Promise<void>;
  loadSessions: (options?: { quiet?: boolean }) => Promise<void>;
  saveServer: (name: string, spec: McpServerSpec, existing: McpServerResource | null) => Promise<void>;
  setServerEnabled: (name: string, enabled: boolean) => Promise<void>;
  removeServer: (name: string, reason?: string) => Promise<void>;
  approveTools: (name: string) => Promise<number>;
  saveGateway: (name: string, spec: McpGatewaySpec, existing: McpGatewayResource | null) => Promise<void>;
  setGatewayEnabled: (name: string, enabled: boolean) => Promise<void>;
  removeGateway: (name: string, reason?: string) => Promise<void>;
}

const byName = <T extends { name: string }>(items: T[]) =>
  [...items].sort((a, b) => a.name.localeCompare(b.name));

function upsert<T extends { name: string }>(items: T[] | null, item: T): T[] {
  return byName([...(items ?? []).filter((x) => x.name !== item.name), item]);
}

/** MCP servers with their status, gateways, session counters and the `mcp` settings. */
export const useMcp = create<McpState>()((set, get) => ({
  servers: emptySlot(),
  statuses: {},
  gateways: emptySlot(),
  sessions: emptySlot(),
  settings: emptySlot(),

  async load(options) {
    await Promise.all([
      loadSlot(set, get, 'servers', mcpApi.servers, options),
      loadSlot(set, get, 'gateways', mcpApi.gateways, options),
    ]);
    await get().loadStatuses();
  },

  async loadStatuses() {
    const names = (get().servers.data ?? []).map((s) => s.name);
    await Promise.all(names.map((n) => get().loadStatus(n)));
  },

  async loadStatus(name) {
    try {
      const status = await mcpApi.status(name);
      set((s) => ({ statuses: { ...s.statuses, [name]: { status, error: null } } }));
    } catch (e) {
      set((s) => ({
        statuses: {
          ...s.statuses,
          [name]: { status: s.statuses[name]?.status ?? null, error: asApiError(e).message },
        },
      }));
    }
  },

  async loadSessions(options) {
    await Promise.all([
      loadSlot(set, get, 'sessions', mcpApi.sessions, options),
      loadSlot(set, get, 'settings', mcpApi.settings, { quiet: true }),
    ]);
  },

  async saveServer(name, spec, existing) {
    const saved = existing
      ? await mcpApi.replaceServer(name, spec, existing.metadata.etag)
      : await mcpApi.createServer(name, spec);
    set((s) => ({ servers: { ...s.servers, data: upsert(s.servers.data, saved) } }));
    void get().loadStatus(name);
  },

  async setServerEnabled(name, enabled) {
    const current = get().servers.data?.find((x) => x.name === name);
    await mcpApi.setServerEnabled(name, enabled, current?.metadata.etag);
    await loadSlot(set, get, 'servers', mcpApi.servers, { quiet: true });
  },

  async removeServer(name, reason) {
    await mcpApi.deleteServer(name, reason);
    set((s) => {
      const { [name]: _gone, ...statuses } = s.statuses;
      return {
        servers: { ...s.servers, data: (s.servers.data ?? []).filter((x) => x.name !== name) },
        statuses,
      };
    });
  },

  async approveTools(name) {
    const approval = await mcpApi.approveTools(name);
    await get().loadStatus(name);
    return approval.tools.length;
  },

  async saveGateway(name, spec, existing) {
    const saved = existing
      ? await mcpApi.replaceGateway(name, spec, existing.metadata.etag)
      : await mcpApi.createGateway(name, spec);
    set((s) => ({ gateways: { ...s.gateways, data: upsert(s.gateways.data, saved) } }));
  },

  async setGatewayEnabled(name, enabled) {
    const current = get().gateways.data?.find((x) => x.name === name);
    await mcpApi.setGatewayEnabled(name, enabled, current?.metadata.etag);
    await loadSlot(set, get, 'gateways', mcpApi.gateways, { quiet: true });
  },

  async removeGateway(name, reason) {
    await mcpApi.deleteGateway(name, reason);
    set((s) => ({
      gateways: { ...s.gateways, data: (s.gateways.data ?? []).filter((x) => x.name !== name) },
    }));
  },
}));
