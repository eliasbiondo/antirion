import type { FieldErrors } from '@/lib/validation';

import type {
  McpGatewayResource,
  McpGatewaySpec,
  McpServerResource,
  McpServerSpec,
  McpServerStatus,
} from './api';
import {
  durationError,
  headerErrors,
  headerMap,
  headerRows,
  type HeaderRow,
  mcpNameError,
  put,
  REDACTED,
  urlError,
} from './fields';

export type Transport = McpServerSpec['transport'];
export type AuthType = '' | 'oauth_client_credentials' | 'token_exchange';

export const TRANSPORT_LABEL: Record<Transport, string> = {
  streamable_http: 'Streamable HTTP',
  sse: 'Server-sent events',
  openapi: 'OpenAPI',
  stdio: 'Local process',
};

export interface ServerForm {
  name: string;
  transport: Transport;
  enabled: boolean;
  url: string;
  protocol: 'auto' | 'legacy' | 'modern';
  prefix: string;
  headers: HeaderRow[];
  clientHeaders: string[];
  authType: AuthType;
  tokenUrl: string;
  clientId: string;
  clientSecret: string;
  clientSecretStored: boolean;
  clientAuth: 'basic' | 'post';
  scopes: string[];
  audience: string;
  toolsInclude: string[];
  toolsExclude: string[];
  promptsInclude: string[];
  promptsExclude: string[];
  resourcesInclude: string[];
  resourcesExclude: string[];
  openapiSpec: string;
  operationsInclude: string[];
  operationsExclude: string[];
  discoveryTtl: string;
  toolCacheTtl: string;
  initTimeout: string;
  requestTimeout: string;
  streamIdleTimeout: string;
  retries: string;
  pin: boolean;
}

export const EMPTY_SERVER: ServerForm = {
  name: '',
  transport: 'streamable_http',
  enabled: true,
  url: '',
  protocol: 'auto',
  prefix: '',
  headers: [],
  clientHeaders: [],
  authType: '',
  tokenUrl: '',
  clientId: '',
  clientSecret: '',
  clientSecretStored: false,
  clientAuth: 'basic',
  scopes: [],
  audience: '',
  toolsInclude: [],
  toolsExclude: [],
  promptsInclude: [],
  promptsExclude: [],
  resourcesInclude: [],
  resourcesExclude: [],
  openapiSpec: '',
  operationsInclude: [],
  operationsExclude: [],
  discoveryTtl: '',
  toolCacheTtl: '',
  initTimeout: '',
  requestTimeout: '',
  streamIdleTimeout: '',
  retries: '',
  pin: false,
};

export function serverForm(r: McpServerResource): ServerForm {
  const s = r.spec;
  const auth = s.auth && s.auth.type !== 'none' ? s.auth : null;
  return {
    name: r.name,
    transport: s.transport,
    enabled: s.enabled !== false,
    url: s.url ?? '',
    protocol: s.protocol ?? 'auto',
    prefix: s.prefix ?? '',
    headers: headerRows(s.headers),
    clientHeaders: s.client_headers ?? [],
    authType: auth ? (auth.type as AuthType) : '',
    tokenUrl: auth?.token_url ?? '',
    clientId: auth?.client_id ?? '',
    clientSecret: '',
    clientSecretStored: Boolean(auth?.client_secret),
    clientAuth: auth?.client_auth ?? 'basic',
    scopes: auth?.scopes ?? [],
    audience: auth?.audience ?? '',
    toolsInclude: s.tools?.include ?? [],
    toolsExclude: s.tools?.exclude ?? [],
    promptsInclude: s.prompts?.include ?? [],
    promptsExclude: s.prompts?.exclude ?? [],
    resourcesInclude: s.resources?.include ?? [],
    resourcesExclude: s.resources?.exclude ?? [],
    openapiSpec: s.openapi?.spec ?? '',
    operationsInclude: s.openapi?.operations?.include ?? [],
    operationsExclude: s.openapi?.operations?.exclude ?? [],
    discoveryTtl: s.discovery_ttl ?? '',
    toolCacheTtl: s.tool_cache_ttl ?? '',
    initTimeout: s.init_timeout ?? '',
    requestTimeout: s.request_timeout ?? '',
    streamIdleTimeout: s.stream_idle_timeout ?? '',
    retries: s.retries === undefined ? '' : String(s.retries),
    pin: s.pin_tool_definitions === true,
  };
}

function filter(include: string[], exclude: string[]) {
  if (include.length === 0 && exclude.length === 0) return undefined;
  return { ...(include.length ? { include } : {}), ...(exclude.length ? { exclude } : {}) };
}

/**
 * The server to write. Members the form doesn't show (identity headers, an auth block's token
 * types and resource) are kept from the read; what the form cleared is removed.
 */
export function serverSpec(f: ServerForm, existing: McpServerResource | null): McpServerSpec {
  const spec: McpServerSpec = existing ? structuredClone(existing.spec) : { transport: f.transport };
  spec.transport = f.transport;
  put(spec, 'enabled', f.enabled ? undefined : false);
  put(spec, 'url', f.url.trim());
  put(spec, 'protocol', f.protocol === 'auto' ? undefined : f.protocol);
  put(spec, 'prefix', f.prefix.trim());
  put(spec, 'headers', headerMap(f.headers));
  put(spec, 'client_headers', f.clientHeaders);
  if (f.authType) {
    const prev = existing?.spec.auth;
    const was = prev?.type === f.authType ? prev : null;
    const auth: NonNullable<McpServerSpec['auth']> = was ? structuredClone(was) : { type: f.authType };
    auth.type = f.authType;
    put(auth, 'token_url', f.tokenUrl.trim());
    put(auth, 'client_id', f.clientId.trim());
    put(auth, 'client_secret', f.clientSecret.trim() || (f.clientSecretStored ? REDACTED : undefined));
    put(auth, 'client_auth', f.clientAuth === 'basic' ? undefined : f.clientAuth);
    put(auth, 'scopes', f.scopes);
    put(auth, 'audience', f.authType === 'token_exchange' ? f.audience.trim() : undefined);
    if (f.authType === 'oauth_client_credentials') {
      delete auth.subject_token_type;
      delete auth.requested_token_type;
    }
    spec.auth = auth;
  } else delete spec.auth;
  put(spec, 'tools', filter(f.toolsInclude, f.toolsExclude));
  put(spec, 'prompts', filter(f.promptsInclude, f.promptsExclude));
  put(spec, 'resources', filter(f.resourcesInclude, f.resourcesExclude));
  if (f.transport === 'openapi') {
    const operations = filter(f.operationsInclude, f.operationsExclude);
    spec.openapi = { spec: f.openapiSpec.trim(), ...(operations ? { operations } : {}) };
  } else delete spec.openapi;
  put(spec, 'discovery_ttl', f.discoveryTtl.trim());
  put(spec, 'tool_cache_ttl', f.toolCacheTtl.trim());
  put(spec, 'init_timeout', f.initTimeout.trim());
  put(spec, 'request_timeout', f.requestTimeout.trim());
  put(spec, 'stream_idle_timeout', f.streamIdleTimeout.trim());
  put(spec, 'retries', f.retries.trim() === '' ? undefined : Number(f.retries));
  put(spec, 'pin_tool_definitions', f.pin ? true : undefined);
  return spec;
}

export function serverErrors(f: ServerForm, taken: string[], creating: boolean): FieldErrors {
  const e: FieldErrors = {};
  const name = creating ? mcpNameError(f.name, taken) : undefined;
  if (name) e.name = name;
  const url = urlError(f.url, f.transport !== 'stdio');
  if (url) e.url = url;
  if (f.prefix.trim() && !/^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$/.test(f.prefix.trim()))
    e.prefix = 'Lowercase letters, digits and -, up to 32 characters';
  const headers = headerErrors(f.headers);
  if (headers) e.headers = headers;
  if (f.authType) {
    const token = urlError(f.tokenUrl, true);
    if (token) e.tokenUrl = token;
    if (!f.clientId.trim()) e.clientId = 'Required';
    if (f.authType === 'oauth_client_credentials' && !f.clientSecret.trim() && !f.clientSecretStored)
      e.clientSecret = 'Required';
  }
  if (f.transport === 'openapi' && !f.openapiSpec.trim()) e.openapiSpec = 'Required';
  for (const key of [
    'discoveryTtl',
    'toolCacheTtl',
    'initTimeout',
    'requestTimeout',
    'streamIdleTimeout',
  ] as const) {
    const d = durationError(f[key]);
    if (d) e[key] = d;
  }
  if (f.retries.trim() && !/^[0-3]$/.test(f.retries.trim())) e.retries = 'A whole number from 0 to 3';
  return e;
}

/** What a tool's approval state means, in words and tone. */
export function toolState(tool: McpServerStatus['tools'][number]): {
  label: string;
  tone: 'success' | 'warn' | 'error' | 'neutral';
} {
  if (tool.approved === true) return { label: 'Approved', tone: 'success' };
  if (tool.hidden)
    return { label: tool.approved === false ? 'Changed, hidden' : 'Not approved, hidden', tone: 'error' };
  if (tool.approved === false) return { label: 'Changed since approval', tone: 'warn' };
  return { label: 'Not approved', tone: 'neutral' };
}

/** Tools that need an operator: pinned and not approved, or changed since the approval. */
export function waitingTools(status: McpServerStatus | null | undefined): number {
  return (status?.tools ?? []).filter((t) => t.hidden || t.approved === false).length;
}

export type ServerHealth = 'off' | 'failing' | 'working' | 'unlisted' | 'unknown';

export function serverHealth(r: McpServerResource, status: McpServerStatus | null | undefined): ServerHealth {
  if (r.spec.enabled === false) return 'off';
  if (!status) return 'unknown';
  if (status.last_contact_error) return 'failing';
  return status.last_contact_at || status.tools.length > 0 ? 'working' : 'unlisted';
}

export const HEALTH_LABEL: Record<ServerHealth, string> = {
  off: 'Off',
  failing: 'Failing',
  working: 'Working',
  unlisted: 'Not used yet',
  unknown: 'Unknown',
};

export const HEALTH_TONE: Record<ServerHealth, 'success' | 'warn' | 'error' | 'neutral'> = {
  off: 'neutral',
  failing: 'error',
  working: 'success',
  unlisted: 'neutral',
  unknown: 'neutral',
};

// ---------------------------------------------------------------- gateways

export interface GatewayForm {
  name: string;
  servers: string[];
  enabled: boolean;
  prefixMode: 'auto' | 'always' | 'never';
  sessionMode: 'stateful' | 'stateless';
  onServerFailure: 'partial' | 'fail';
  toolSearch: boolean;
  serverInfo: 'gateway' | 'upstream' | 'custom';
  infoName: string;
  infoVersion: string;
  infoTitle: string;
  allowedOrigins: string[];
  maxBody: string;
  oauth: boolean;
  oauthResource: string;
  authorizationServers: string[];
  scopes: string[];
  audiences: string[];
  proxyRegistration: boolean;
}

export const EMPTY_GATEWAY: GatewayForm = {
  name: '',
  servers: [],
  enabled: true,
  prefixMode: 'auto',
  sessionMode: 'stateful',
  onServerFailure: 'partial',
  toolSearch: false,
  serverInfo: 'gateway',
  infoName: '',
  infoVersion: '',
  infoTitle: '',
  allowedOrigins: [],
  maxBody: '',
  oauth: false,
  oauthResource: '',
  authorizationServers: [],
  scopes: [],
  audiences: [],
  proxyRegistration: false,
};

export function gatewayForm(r: McpGatewayResource): GatewayForm {
  const s = r.spec;
  const info = s.server_info;
  return {
    name: r.name,
    servers: s.servers,
    enabled: s.enabled !== false,
    prefixMode: s.prefix_mode ?? 'auto',
    sessionMode: s.session_mode ?? 'stateful',
    onServerFailure: s.on_server_failure ?? 'partial',
    toolSearch: s.tool_search === true,
    serverInfo: info === 'upstream' ? 'upstream' : info ? 'custom' : 'gateway',
    infoName: typeof info === 'object' ? (info.name ?? '') : '',
    infoVersion: typeof info === 'object' ? (info.version ?? '') : '',
    infoTitle: typeof info === 'object' ? (info.title ?? '') : '',
    allowedOrigins: s.allowed_origins ?? [],
    maxBody: s.max_body ?? '',
    oauth: Boolean(s.oauth),
    oauthResource: s.oauth?.resource ?? '',
    authorizationServers: s.oauth?.authorization_servers ?? [],
    scopes: s.oauth?.scopes_supported ?? [],
    audiences: s.oauth?.audiences ?? [],
    proxyRegistration: s.oauth?.proxy_registration === true,
  };
}

export function gatewaySpec(f: GatewayForm, existing: McpGatewayResource | null): McpGatewaySpec {
  const spec: McpGatewaySpec = existing ? structuredClone(existing.spec) : { servers: f.servers };
  spec.servers = f.servers;
  put(spec, 'enabled', f.enabled ? undefined : false);
  put(spec, 'prefix_mode', f.prefixMode === 'auto' ? undefined : f.prefixMode);
  put(spec, 'session_mode', f.sessionMode === 'stateful' ? undefined : f.sessionMode);
  put(spec, 'on_server_failure', f.onServerFailure === 'partial' ? undefined : f.onServerFailure);
  put(spec, 'tool_search', f.toolSearch ? true : undefined);
  if (f.serverInfo === 'upstream') spec.server_info = 'upstream';
  else if (f.serverInfo === 'custom') {
    const info: Exclude<NonNullable<McpGatewaySpec['server_info']>, 'upstream'> = {};
    put(info, 'name', f.infoName.trim());
    put(info, 'version', f.infoVersion.trim());
    put(info, 'title', f.infoTitle.trim());
    spec.server_info = info;
  } else delete spec.server_info;
  put(spec, 'allowed_origins', f.allowedOrigins);
  put(spec, 'max_body', f.maxBody.trim());
  if (f.oauth) {
    const oauth: NonNullable<McpGatewaySpec['oauth']> = existing?.spec.oauth
      ? structuredClone(existing.spec.oauth)
      : { authorization_servers: f.authorizationServers };
    oauth.authorization_servers = f.authorizationServers;
    put(oauth, 'resource', f.oauthResource.trim());
    put(oauth, 'scopes_supported', f.scopes);
    put(oauth, 'audiences', f.audiences);
    put(oauth, 'proxy_registration', f.proxyRegistration ? true : undefined);
    spec.oauth = oauth;
  } else delete spec.oauth;
  return spec;
}

export function gatewayErrors(f: GatewayForm, taken: string[], creating: boolean): FieldErrors {
  const e: FieldErrors = {};
  const name = creating ? mcpNameError(f.name, taken) : undefined;
  if (name) e.name = name;
  if (f.servers.length === 0) e.servers = 'Pick at least one server';
  if (f.serverInfo === 'upstream' && f.servers.length !== 1)
    e.serverInfo = "Only a gateway with one server can show that server's own name";
  if (f.maxBody.trim() && !/^[0-9]+(B|KiB|MiB|GiB|KB|MB|GB)$/.test(f.maxBody.trim()))
    e.maxBody = 'A size such as 512KiB or 8MiB';
  for (const o of f.allowedOrigins)
    if (!/^[A-Za-z][A-Za-z0-9+.-]*:\/\/[^/\s]+$/.test(o)) {
      e.allowedOrigins = `${o} isn't an origin (scheme and host, no path)`;
      break;
    }
  if (f.oauth && f.authorizationServers.length === 0) e.authorizationServers = 'Add at least one';
  return e;
}

/** The paths a gateway is served at on the data plane. */
export function gatewayPaths(name: string): string[] {
  return name === 'default' ? ['/mcp', '/mcp/default'] : [`/mcp/${name}`];
}
