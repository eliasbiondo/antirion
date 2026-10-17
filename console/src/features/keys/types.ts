import type { KeyResource, UserResource } from '@/lib/api/types';

export type KeyType = 'service' | 'user' | 'ephemeral';
export type KeyStatus = 'active' | 'disabled' | 'rotating' | 'expiring' | 'expired';

export const KEY_STATUSES: KeyStatus[] = ['active', 'rotating', 'expiring', 'expired', 'disabled'];
export const KEY_TYPES: KeyType[] = ['service', 'user', 'ephemeral'];

export const ENDPOINT_FAMILIES = [
  'chat',
  'responses',
  'messages',
  'embeddings',
  'images',
  'audio',
  'moderations',
  'rerank',
  'files',
  'batches',
  'realtime',
  'models',
  'mcp',
  'a2a',
] as const;

export type EndpointFamily = (typeof ENDPOINT_FAMILIES)[number];

/** The endpoint families in groups an operator recognises, with plain names; the id is the tooltip. */
export const ENDPOINT_GROUPS: { label: string; items: { id: EndpointFamily; label: string }[] }[] = [
  {
    label: 'Text',
    items: [
      { id: 'chat', label: 'Chat completions' },
      { id: 'responses', label: 'Responses' },
      { id: 'messages', label: 'Messages' },
      { id: 'embeddings', label: 'Embeddings' },
      { id: 'moderations', label: 'Moderations' },
      { id: 'rerank', label: 'Rerank' },
    ],
  },
  {
    label: 'Media and live',
    items: [
      { id: 'images', label: 'Images' },
      { id: 'audio', label: 'Audio' },
      { id: 'realtime', label: 'Realtime' },
    ],
  },
  {
    label: 'Files and other',
    items: [
      { id: 'files', label: 'Files' },
      { id: 'batches', label: 'Batches' },
      { id: 'models', label: 'Model list' },
      { id: 'mcp', label: 'MCP tools' },
      { id: 'a2a', label: 'Agent-to-agent' },
    ],
  },
];

export function endpointLabel(id: string): string {
  for (const g of ENDPOINT_GROUPS) {
    const found = g.items.find((i) => i.id === id);
    if (found) return found.label;
  }
  return id;
}

export const KEY_PERMISSIONS = [
  { id: 'route:pin', label: 'Choose a specific deployment', group: 'Routing' },
  { id: 'route:debug', label: 'See routing details in responses', group: 'Routing' },
  { id: 'route:decide', label: 'Ask which deployment would answer', group: 'Routing' },
  { id: 'route:fallbacks', label: 'Send its own fallback list', group: 'Routing' },
  { id: 'models:direct_provider', label: "Call a provider's model directly", group: 'Providers' },
  { id: 'byok', label: 'Use its own provider key', group: 'Providers' },
  { id: 'tags:request', label: 'Tag requests', group: 'Requests' },
  { id: 'override:relax', label: 'Relax policies per request', group: 'Requests' },
  { id: 'debug', label: 'Get debug output', group: 'Requests' },
  { id: 'limits:read_inherited', label: 'Read the limits it inherits', group: 'Requests' },
] as const;

export function permissionLabel(id: string): string {
  return KEY_PERMISSIONS.find((p) => p.id === id)?.label ?? id;
}

export const TYPE_LABEL: Record<KeyType, string> = {
  service: 'Service',
  user: 'User',
  ephemeral: 'Ephemeral',
};

export const STATUS_LABEL: Record<KeyStatus, string> = {
  active: 'Active',
  rotating: 'Rotating',
  expiring: 'Expiring soon',
  expired: 'Expired',
  disabled: 'Disabled',
};

export interface KeyView {
  key: KeyResource;
  name: string;
  type: KeyType;
  status: KeyStatus;
  project: string;
  owner: string | null;
}

const DAY = 86_400_000;

/**
 * The key's type as the gateway reports it (`metadata.type`): `service` when a service
 * account owns the key, `ephemeral` for a playground key or one that lives at most 7 days, `service`
 * when no one owns it, else `user`. A gateway that does not report it gets the same derivation here.
 */
export function keyType(key: KeyResource, usersByName: Map<string, UserResource>): KeyType {
  if (key.metadata.type) return key.metadata.type;
  const owner = key.spec.owner ? usersByName.get(key.spec.owner) : undefined;
  if (owner?.spec.kind === 'service') return 'service';
  if (key.metadata.created_by === 'playground') return 'ephemeral';
  const created = key.metadata.created_at ? new Date(key.metadata.created_at).getTime() : NaN;
  const expires = key.spec.expires_at ? new Date(key.spec.expires_at).getTime() : NaN;
  if (Number.isFinite(created) && Number.isFinite(expires) && expires - created <= 7 * DAY)
    return 'ephemeral';
  if (!key.spec.owner) return 'service';
  return 'user';
}

/** Whether a key was made by the playground: shown as ephemeral and not editable. */
export function isPlaygroundKey(key: KeyResource): boolean {
  return key.metadata.created_by === 'playground';
}

/** The derived status, from `enabled`, the rotation grace window and the expiry. */
export function keyStatus(key: KeyResource, now: number): KeyStatus {
  const expires = key.spec.expires_at ? new Date(key.spec.expires_at).getTime() : null;
  if (expires !== null && expires <= now) return 'expired';
  if (key.spec.enabled === false) return 'disabled';
  const grace = key.metadata.rotation?.valid_until;
  if (grace && new Date(grace).getTime() > now) return 'rotating';
  if (expires !== null && expires - now <= 7 * DAY) return 'expiring';
  return 'active';
}

export function statusTone(status: KeyStatus): 'success' | 'info' | 'warn' | 'error' | 'neutral' {
  switch (status) {
    case 'active':
      return 'success';
    case 'rotating':
      return 'info';
    case 'expiring':
      return 'warn';
    case 'expired':
      return 'error';
    case 'disabled':
      return 'neutral';
  }
}
