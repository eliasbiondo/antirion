import { api, listAll } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';

type S = components['schemas'];
export type McpServerResource = S['McpServerResource'];
export type McpServerSpec = McpServerResource['spec'];
export type McpServerStatus = S['McpServerStatus'];
export type McpTool = McpServerStatus['tools'][number];
export type McpGatewayResource = S['McpGatewayResource'];
export type McpGatewaySpec = McpGatewayResource['spec'];
export type McpSessionStats = S['McpSessionStats'];
export type McpSettings = S['McpSettings'];

/**
 * MCP: the servers the gateway reaches (with their discovery status and tool approval), the
 * gateways clients call, their session counters and the `mcp` settings.
 */
export const mcpApi = {
  servers: (): Promise<McpServerResource[]> =>
    listAll((cursor) => api.get('/mcp/servers', { query: { limit: 200, cursor } })),
  status: (name: string): Promise<McpServerStatus> =>
    api.get('/mcp/servers/{name}/status', { path: { name } }),
  createServer: (name: string, spec: McpServerSpec): Promise<McpServerResource> =>
    api.post('/mcp/servers', { body: { name, spec } }),
  replaceServer: (name: string, spec: McpServerSpec, etag: string | undefined): Promise<McpServerResource> =>
    api.put('/mcp/servers/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  setServerEnabled: (name: string, enabled: boolean, etag: string | undefined) =>
    api.patch('/mcp/servers/{name}', { path: { name }, body: { spec: { enabled } }, ifMatch: etag }),
  deleteServer: (name: string, reason?: string) =>
    api.delete('/mcp/servers/{name}', { path: { name }, reason }),
  approveTools: (name: string) => api.post('/mcp/servers/{name}:approve_tools', { path: { name } }),
  gateways: (): Promise<McpGatewayResource[]> =>
    listAll((cursor) => api.get('/mcp/gateways', { query: { limit: 200, cursor } })),
  createGateway: (name: string, spec: McpGatewaySpec): Promise<McpGatewayResource> =>
    api.post('/mcp/gateways', { body: { name, spec } }),
  replaceGateway: (
    name: string,
    spec: McpGatewaySpec,
    etag: string | undefined,
  ): Promise<McpGatewayResource> =>
    api.put('/mcp/gateways/{name}', { path: { name }, body: { spec }, ifMatch: etag }),
  setGatewayEnabled: (name: string, enabled: boolean, etag: string | undefined) =>
    api.patch('/mcp/gateways/{name}', { path: { name }, body: { spec: { enabled } }, ifMatch: etag }),
  deleteGateway: (name: string, reason?: string) =>
    api.delete('/mcp/gateways/{name}', { path: { name }, reason }),
  sessions: (): Promise<McpSessionStats[]> =>
    listAll((cursor) => api.get('/runtime/mcp/sessions', { query: { limit: 200, cursor } })),
  settings: (): Promise<McpSettings> => api.get('/settings/mcp'),
};
