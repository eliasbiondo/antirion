import type { Tone } from '@/components/ui/badge';
import type { AttemptedChange, ChangeRequest } from '@/lib/api/client';

import type { ChangeRequestState } from './api';

export const STATE_LABEL: Record<ChangeRequestState, string> = {
  open: 'Waiting for review',
  approved: 'Approved',
  applied: 'Applied',
  rejected: 'Rejected',
  cancelled: 'Cancelled',
  expired: 'Expired',
  conflicted: 'Conflicts',
};

export function stateTone(state: ChangeRequestState): Tone {
  switch (state) {
    case 'open':
      return 'warn';
    case 'approved':
      return 'info';
    case 'applied':
      return 'success';
    case 'rejected':
    case 'conflicted':
      return 'error';
    default:
      return 'neutral';
  }
}

/** Collections and settings by their path segment, as the sentence names them. */
const NOUNS: Record<string, string> = {
  routes: 'route',
  models: 'model',
  deployments: 'deployment',
  providers: 'provider',
  guardrails: 'guardrail',
  budgets: 'budget',
  organizations: 'organization',
  teams: 'team',
  projects: 'project',
  'alerts/rules': 'alert rule',
  'alerts/channels': 'alert channel',
  'cache/invalidation_rules': 'cache rule',
  'routing/tiers': 'tier',
};

const VERBS: Record<AttemptedChange['method'], string> = {
  POST: 'Create',
  PUT: 'Replace',
  PATCH: 'Change',
  DELETE: 'Delete',
};

/** A title for a change request opened from a refused write: "Change route default". */
export function titleFor(change: Pick<AttemptedChange, 'method' | 'path' | 'body'>): string {
  const [path = ''] = change.path.split('?');
  const segs = path.split('/').filter(Boolean).map(decodeURIComponent);
  const verb = VERBS[change.method];
  if (segs[0] === 'settings' && segs[1]) return `Change ${segs[1].replace(/_/g, ' ')} settings`;
  const two = segs.slice(0, 2).join('/');
  const [collection, name] = NOUNS[two] ? [two, segs[2]] : [segs[0] ?? '', segs[1]];
  const noun = NOUNS[collection] ?? collection.replace(/_/g, ' ');
  const bodyName =
    typeof change.body === 'object' && change.body !== null
      ? (change.body as { name?: unknown }).name
      : undefined;
  const target = name ?? (typeof bodyName === 'string' ? bodyName : undefined);
  return `${verb} ${noun}${target ? ` ${target}` : ''}`.trim();
}

/** "PATCH /routes/default" for the changes list. */
export function changeLine(change: { method: string; path: string }): string {
  return `${change.method} ${change.path}`;
}

/** How many approvals the request still needs. */
export function approvalsLeft(cr: ChangeRequest): number {
  return Math.max(0, cr.approvals_required - cr.approvals.length);
}
