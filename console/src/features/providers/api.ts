import { api, listAll } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';
import type { DeploymentHealth, DeploymentResource, ProviderResource, ProviderType } from '@/lib/api/types';

type S = components['schemas'];
export type ProviderSpec = S['Provider'];
export type ProviderStatus = S['ProviderStatus'];

/**
 * The gateway's upstream providers: the list, the supported types, create, enable, test, rotate,
 * delete, and the runtime state of their keys (health and reset).
 */
export const providersApi = {
  list: (): Promise<ProviderResource[]> =>
    listAll((cursor) => api.get('/providers', { query: { limit: 200, cursor } })),
  types: (): Promise<ProviderType[]> =>
    listAll((cursor) => api.get('/provider-types', { query: { limit: 200, cursor } })),
  status: async (): Promise<ProviderStatus[]> => (await api.get('/runtime/providers')).items,
  deployments: (): Promise<DeploymentResource[]> =>
    listAll((cursor) => api.get('/deployments', { query: { limit: 200, cursor } })),
  create: (name: string, spec: ProviderSpec): Promise<ProviderResource> =>
    api.post('/providers', { body: { name, spec } }),
  setEnabled: (name: string, enabled: boolean, etag: string | undefined) =>
    api.patch('/providers/{name}', { path: { name }, body: { spec: { enabled } }, ifMatch: etag }),
  test: (name: string) => api.post('/providers/{name}:test', { path: { name } }),
  rotateCredential: (name: string, secret: string, overlap: string, key?: string) =>
    api.post('/providers/{name}:rotate_credential', {
      path: { name },
      body: { credential: secret, retire_after: overlap, ...(key ? { key } : {}) },
    }),
  /** Deployment health, for the state of each provider key on each deployment. */
  health: (): Promise<DeploymentHealth[]> =>
    listAll((cursor) => api.get('/runtime/deployments', { query: { limit: 200, cursor } })),
  resetKey: (provider: string, key: string) =>
    api.post('/runtime/providers/{provider}/keys/{key}:reset', { path: { provider, key } }),
  remove: (name: string, reason?: string) => api.delete('/providers/{name}', { path: { name }, reason }),
};
