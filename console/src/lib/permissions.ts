/** What a permission's resource is, in words that follow a verb ("change the configuration"). */
const RESOURCES: Record<string, string> = {
  analytics: 'analytics',
  alerts: 'alerts',
  audit: 'the audit log',
  cache: 'the cache',
  cluster: 'the nodes',
  deployments: 'deployments',
  config: 'the configuration',
  guardrails: 'guardrails',
  health: 'the gateway health',
  keys: 'API keys',
  logs: 'the request log',
  mcp: 'MCP servers and agents',
  models: 'Models',
  node: 'nodes',
  organizations: 'organizations',
  playground: 'the playground',
  plugins: 'plugins',
  policies: 'policies and budgets',
  pricing: 'prices',
  projects: 'projects',
  providers: 'providers',
  role_bindings: 'roles',
  security: 'security settings',
  teams: 'teams',
  users: 'people',
};

/** Whole permissions whose words don't follow from their parts. */
const EXACT: Record<string, string> = {
  'config:apply': 'apply configuration changes',
  'config:read': 'see the configuration',
  'cache:admin': 'manage the cache',
  'node:drain': 'drain nodes',
  'playground:send': 'use the playground',
  'logs:read_content': 'see prompts and responses',
  'logs:delete': 'delete requests from the log',
  'keys:grant_permissions': 'grant key permissions',
  'budgets:adjust': 'adjust budgets',
  'safety:review': 'review safety events',
  'notifications:send': 'send notifications',
};

const ACTIONS: Record<string, string> = {
  read: 'see',
  write: 'change',
  create: 'create',
  update: 'edit',
  delete: 'delete',
  admin: 'manage',
  apply: 'apply',
  rotate: 'rotate',
};

/**
 * A permission id (`config:apply`) as what it lets someone do ("apply configuration changes"), for
 * a disabled control's reason; the id itself is never shown.
 */
export function permissionPhrase(permission: string): string {
  const exact = EXACT[permission];
  if (exact) return exact;
  const [resource = '', action = ''] = permission.split(':');
  const noun = RESOURCES[resource] ?? resource.replace(/_/g, ' ');
  const verb = ACTIONS[action] ?? action.replace(/_/g, ' ');
  return `${verb} ${noun}`.trim();
}

/** A disabled control's reason: "You don't have permission to apply configuration changes." */
export function noPermissionReason(permission: string): string {
  if (permission.startsWith('role:')) return `Only a ${permission.slice(5).replace(/_/g, ' ')} can do this.`;
  return `You don't have permission to ${permissionPhrase(permission)}.`;
}
