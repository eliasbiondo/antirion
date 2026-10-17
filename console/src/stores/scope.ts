import { create } from 'zustand';

/** The console's scope: the whole gateway, or one organization; plus an environment tag value. */
export type Scope = { kind: 'gateway' } | { kind: 'organization'; name: string };

interface ScopeState {
  scope: Scope;
  /** A value of the environment tag, or null for all environments. */
  environment: string | null;
  setScope: (scope: Scope) => void;
  setEnvironment: (environment: string | null) => void;
}

/** The organization last used on this device, a convenience for the sign-in form. */
export const LAST_ORGANIZATION_KEY = 'antirion.console.organization';

export const useScope = create<ScopeState>()((set) => ({
  scope: { kind: 'gateway' },
  environment: null,
  setScope(scope) {
    set({ scope });
    try {
      if (scope.kind === 'organization') localStorage.setItem(LAST_ORGANIZATION_KEY, scope.name);
    } catch {
      return;
    }
  },
  setEnvironment(environment) {
    set({ environment });
  },
}));

export function scopeOrganization(scope: Scope): string | null {
  return scope.kind === 'organization' ? scope.name : null;
}

export function encodeScope(scope: Scope): string {
  return scope.kind === 'organization' ? scope.name : 'gateway';
}

export function decodeScope(value: string | null): Scope | null {
  if (!value) return null;
  if (value === 'gateway') return { kind: 'gateway' };
  return /^[a-z0-9][a-z0-9_-]{0,62}$/.test(value) ? { kind: 'organization', name: value } : null;
}

/** The analytics filters of the scope: the organization and the environment tag. */
export function scopeFilters(
  scope: Scope,
  environment: string | null,
): { organization?: string; tag?: string } {
  const out: { organization?: string; tag?: string } = {};
  if (scope.kind === 'organization') out.organization = scope.name;
  if (environment) out.tag = `env:${environment}`;
  return out;
}
