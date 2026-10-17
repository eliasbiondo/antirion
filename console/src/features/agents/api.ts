import { api, listAll } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';

type S = components['schemas'];
export type AgentResource = S['A2aAgentResource'];
export type AgentSpec = AgentResource['spec'];

/** The A2A agents the gateway proxies: list, create, replace, turn on or off and delete. */
export const agentsApi = {
  list: (): Promise<AgentResource[]> =>
    listAll((cursor) => api.get('/a2a/agents', { query: { limit: 200, cursor } })),
  create: (name: string, spec: AgentSpec): Promise<AgentResource> =>
    api.post('/a2a/agents', { body: { name, spec } }),
  replace: (name: string, spec: AgentSpec, etag: string | undefined): Promise<AgentResource> =>
    api.put('/a2a/agents/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  setEnabled: (name: string, enabled: boolean, etag: string | undefined) =>
    api.patch('/a2a/agents/{name}', { path: { name }, body: { spec: { enabled } }, ifMatch: etag }),
  remove: (name: string, reason?: string) => api.delete('/a2a/agents/{name}', { path: { name }, reason }),
};
