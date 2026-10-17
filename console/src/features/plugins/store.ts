import { create } from 'zustand';

import { emptySlot, loadSlot, type Slot } from '@/lib/slot';

import {
  attachmentName,
  type PluginResource,
  type PluginRuntime,
  type PluginSpec,
  pluginsApi,
  type PoliciesSettings,
} from './api';

interface PluginsState {
  plugins: Slot<PluginResource[]>;
  runtime: Slot<PluginRuntime[]>;
  policies: Slot<PoliciesSettings>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  save: (name: string, spec: PluginSpec, existing: PluginResource | null) => Promise<void>;
  setEnabled: (name: string, enabled: boolean) => Promise<void>;
  setAttached: (name: string, attached: boolean) => Promise<void>;
  remove: (name: string, reason?: string) => Promise<void>;
}

/** Plugins, their runtime counters and the gateway level's attachments. */
export const usePlugins = create<PluginsState>()((set, get) => ({
  plugins: emptySlot(),
  runtime: emptySlot(),
  policies: emptySlot(),

  async load(options) {
    await Promise.all([
      loadSlot(set, get, 'plugins', pluginsApi.list, options),
      loadSlot(set, get, 'runtime', pluginsApi.runtime, options),
      loadSlot(set, get, 'policies', pluginsApi.policies, { quiet: true }),
    ]);
  },

  async save(name, spec, existing) {
    const saved = existing
      ? await pluginsApi.replace(name, spec, existing.metadata.etag)
      : await pluginsApi.create(name, spec);
    set((s) => ({
      plugins: {
        ...s.plugins,
        data: [...(s.plugins.data ?? []).filter((p) => p.name !== name), saved].sort((a, b) =>
          a.name.localeCompare(b.name),
        ),
      },
    }));
  },

  async setEnabled(name, enabled) {
    const current = get().plugins.data?.find((p) => p.name === name);
    await pluginsApi.setEnabled(name, enabled, current?.metadata.etag);
    await loadSlot(set, get, 'plugins', pluginsApi.list, { quiet: true });
  },

  async setAttached(name, attached) {
    // Read just before writing, so the attach list written is the current one.
    const policies = await pluginsApi.policies();
    const attach = policies.spec.plugins?.attach ?? [];
    const has = attach.some((a) => attachmentName(a) === name);
    if (has === attached) return;
    const next = attached ? [...attach, name] : attach.filter((a) => attachmentName(a) !== name);
    await pluginsApi.setAttached(next, policies.metadata.etag);
    await loadSlot(set, get, 'policies', pluginsApi.policies, { quiet: true });
  },

  async remove(name, reason) {
    await pluginsApi.remove(name, reason);
    set((s) => ({ plugins: { ...s.plugins, data: (s.plugins.data ?? []).filter((p) => p.name !== name) } }));
  },
}));
