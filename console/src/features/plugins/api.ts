import { api, listAll } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';

type S = components['schemas'];
export type PluginResource = S['PluginResource'];
export type PluginSpec = PluginResource['spec'];
export type PluginRuntime = S['PluginRuntime'];
export type PoliciesSettings = S['PoliciesSettings'];
export type Attachment = NonNullable<S['PolicyPlugins']['attach']>[number];

/**
 * Plugins (extensions): their declarations, the answering node's call counters, and the gateway
 * level's attachments in `/settings/policies`.
 */
export const pluginsApi = {
  list: (): Promise<PluginResource[]> =>
    listAll((cursor) => api.get('/plugins', { query: { limit: 200, cursor } })),
  runtime: (): Promise<PluginRuntime[]> =>
    listAll((cursor) => api.get('/runtime/plugins', { query: { limit: 200, cursor } })),
  create: (name: string, spec: PluginSpec): Promise<PluginResource> =>
    api.post('/plugins', { body: { name, spec } }),
  replace: (name: string, spec: PluginSpec, etag: string | undefined): Promise<PluginResource> =>
    api.put('/plugins/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  setEnabled: (name: string, enabled: boolean, etag: string | undefined) =>
    api.patch('/plugins/{name}', { path: { name }, body: { spec: { enabled } }, ifMatch: etag }),
  remove: (name: string, reason?: string) => api.delete('/plugins/{name}', { path: { name }, reason }),
  policies: (): Promise<PoliciesSettings> => api.get('/settings/policies'),
  setAttached: (attach: Attachment[], etag: string | undefined) =>
    api.patch('/settings/policies', { body: { spec: { plugins: { attach } } }, ifMatch: etag }),
};

export function attachmentName(a: Attachment): string {
  return typeof a === 'string' ? a : a.name;
}
