import { api } from '@/lib/api/client';
import type { components } from '@/lib/api/schema';
import type { Me } from '@/lib/api/types';

export type ResetScope = NonNullable<components['schemas']['TestReset']['scopes']>[number];

export const RESET_SCOPES: { value: ResetScope; label: string }[] = [
  { value: 'counters', label: 'Rate-limit counters' },
  { value: 'budgets', label: 'Budget counters' },
  { value: 'health', label: 'Deployment health and circuits' },
  { value: 'affinity', label: 'Session affinity' },
  { value: 'cache', label: 'Cached responses' },
  { value: 'sessions', label: 'MCP sessions' },
  { value: 'logs', label: 'Request log' },
  { value: 'audit', label: 'Audit log' },
];

/** The test-mode endpoints; they exist only on a gateway started with `--test-mode`. */
export const testModeApi = {
  clock: () => api.get('/test/clock'),
  advance: (advance: string) => api.post('/test/clock', { body: { advance } }),
  setClock: (at: string) => api.post('/test/clock', { body: { set: at } }),
  reset: (scopes: ResetScope[]) => api.post('/test/reset', { body: { scopes } }),
  seed: (seed: number) => api.put('/test/seed', { body: { seed } }),
};

/** The test endpoints need the gateway_admin role at gateway scope, not a permission. */
export function isGatewayAdmin(me: Me | null): boolean {
  return Boolean(me?.bindings.some((b) => b.role === 'gateway_admin' && b.scope === 'gateway'));
}
