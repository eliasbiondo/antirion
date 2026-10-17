'use client';

import { cloneElement, isValidElement, type ReactElement, type ReactNode } from 'react';

import { OPERATIONS, type OperationId } from '@/lib/api/operations';
import { noPermissionReason } from '@/lib/permissions';
import { scopeOrganization, useScope } from '@/stores/scope';
import { hasPermission, useSession } from '@/stores/session';

import { EmptyState, midSentence } from './states';

/** Whether the principal may call an operation of the contract, from its declared permission. */
export function useCan(operation: OperationId): boolean {
  const me = useSession((s) => s.me);
  const scope = useScope((s) => s.scope);
  const permission: string = OPERATIONS[operation].permission;
  if (permission === 'public' || permission === 'authenticated') return me !== null;
  return hasPermission(me, permission, scopeOrganization(scope));
}

/** Whether the principal holds a permission (one that no single operation names, such as keys:grant_permissions). */
export function usePermission(permission: string): boolean {
  const me = useSession((s) => s.me);
  const scope = useScope((s) => s.scope);
  return hasPermission(me, permission, scopeOrganization(scope));
}

export function permissionOf(operation: OperationId): string {
  return OPERATIONS[operation].permission;
}

interface GateProps {
  operation: OperationId;
  /** `hide` removes the control; `disable` keeps it, disabled, with the reason as its tooltip. */
  mode?: 'hide' | 'disable';
  children: ReactElement<{ disabled?: boolean; disabledReason?: string }>;
}

/** Hides or disables a control the principal may not use, saying in words what it would need. */
export function Gate({ operation, mode = 'disable', children }: GateProps) {
  const allowed = useCan(operation);
  if (allowed) return children;
  if (mode === 'hide' || !isValidElement(children)) return null;
  const reason = noPermissionReason(permissionOf(operation));
  return cloneElement(children, { disabled: true, disabledReason: reason });
}

/** A whole view the principal may not read. */
export function Forbidden({ what, children }: { what: string; children?: ReactNode }) {
  return (
    <EmptyState
      title={`You don't have access to ${midSentence(what)}`}
      description="Ask a gateway admin for access."
      action={children}
    />
  );
}
