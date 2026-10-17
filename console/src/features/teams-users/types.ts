import type {
  Invitation,
  KeyResource,
  ProjectResource,
  RoleBinding,
  TeamResource,
  UserResource,
} from '@/lib/api/types';

import type { MfaKind, SignInMethod } from './facts';

export type UserState = 'active' | 'invited' | 'disabled';

export const ROLE_DESCRIPTIONS: Record<string, string> = {
  gateway_admin: 'Full access to everything on the gateway',
  gateway_viewer: 'Can see everything on the gateway, except prompts and responses',
  security_auditor: 'Can see the audit log and security settings',
  org_admin: 'Manages one organization',
  team_admin: 'Manages one team',
  developer: 'Creates keys in a team or project',
  viewer: 'Can see what is in their organization, team or project',
};

const ROLE_NAMES: Record<string, string> = {
  gateway_admin: 'Gateway admin',
  gateway_viewer: 'Gateway viewer',
  security_auditor: 'Security auditor',
  org_admin: 'Organization admin',
  team_admin: 'Team admin',
  developer: 'Developer',
  viewer: 'Viewer',
};

/** "gateway_admin" → "Gateway admin"; custom roles keep their id with spaces. */
export function roleLabel(role: string): string {
  const known = ROLE_NAMES[role];
  if (known) return known;
  const spaced = role.replace(/_/g, ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/** "team/support-ai" → "Team support-ai", "gateway" → "Whole gateway". */
export function scopeLabel(scope: string): string {
  if (scope === 'gateway') return 'Whole gateway';
  const [kind, ...rest] = scope.split('/');
  const name = rest.join('/');
  const kinds: Record<string, string> = { organization: 'Organization', team: 'Team', project: 'Project' };
  return name ? `${kinds[kind ?? ''] ?? kind} ${name}` : scope;
}

export const STATE_LABEL: Record<UserState, string> = {
  active: 'Active',
  invited: 'Invited',
  disabled: 'Disabled',
};

export interface UserView {
  user: UserResource | null;
  name: string;
  displayName: string;
  email: string;
  kind: 'human' | 'service';
  state: UserState;
  primaryTeam: string | null;
  bindings: { id: string | null; role: string; scope: string }[];
  keys: number;
  invitation: Invitation | null;
  /** The latest sign-in, session, admin token or owned key use; null when never active. */
  lastActive: string | null;
  /** A key they own has an open unusual-activity flag. */
  flagged: boolean;
  /** How they last signed in to the console. */
  signIn: SignInMethod | null;
  /** Their second factors: authenticator app, security key, or asserted by single sign-on. */
  mfa: MfaKind[];
}

export function userViews(
  users: UserResource[],
  bindings: RoleBinding[],
  invitations: Invitation[],
  keys: KeyResource[],
): UserView[] {
  const views: UserView[] = users.map((u) => {
    const fromApi = bindings.filter((b) => b.principal.type === 'user' && b.principal.id === u.name);
    const declared = u.spec.role_bindings ?? [];
    const merged = declared.map((d) => ({
      id: fromApi.find((b) => b.role === d.role && b.scope === d.scope)?.id ?? null,
      role: d.role,
      scope: d.scope,
    }));
    for (const b of fromApi)
      if (!merged.some((m) => m.role === b.role && m.scope === b.scope))
        merged.push({ id: b.id, role: b.role, scope: b.scope });
    return {
      user: u,
      name: u.name,
      displayName: u.spec.display_name ?? u.name,
      email: u.spec.email ?? '',
      kind: u.spec.kind ?? 'human',
      state: stateOf(u),
      // Without a primary team, the one team a binding names is theirs (an invitation to a team).
      primaryTeam:
        u.spec.primary_team ??
        (() => {
          const teams = [
            ...new Set(merged.flatMap((b) => (b.scope.startsWith('team/') ? [b.scope.slice(5)] : []))),
          ];
          return teams.length === 1 ? (teams[0] ?? null) : null;
        })(),
      bindings: merged,
      keys: keys.filter((k) => k.spec.owner === u.name).length,
      invitation:
        stateOf(u) === 'invited'
          ? (invitations.find((i) => i.user === u.name || i.email === u.spec.email) ?? null)
          : null,
      lastActive: u.metadata.last_active_at ?? null,
      flagged: u.metadata.flagged === true,
      signIn: u.metadata.last_login_method ?? null,
      mfa: u.metadata.mfa_methods ?? [],
    };
  });
  for (const inv of invitations) {
    if (views.some((v) => v.name === inv.user)) continue;
    views.push({
      user: null,
      name: inv.user,
      displayName: inv.email,
      email: inv.email,
      kind: 'human',
      state: 'invited',
      primaryTeam: null,
      bindings: (inv.role_bindings ?? []).map((b) => ({ id: null, role: b.role, scope: b.scope })),
      keys: 0,
      invitation: inv,
      lastActive: null,
      flagged: false,
      signIn: null,
      mfa: [],
    });
  }
  return views.sort((a, b) => a.displayName.localeCompare(b.displayName));
}

const ROLE_RANK = [
  'gateway_admin',
  'org_admin',
  'gateway_viewer',
  'team_admin',
  'security_auditor',
  'developer',
  'viewer',
];

/**
 * A user's state: the gateway reports `invited` for an account made by an invitation that was not
 * accepted yet (it is held disabled until then), which must not read as "disabled".
 */
function stateOf(u: UserResource): UserState {
  const reported = (u.metadata as { state?: string }).state;
  if (reported === 'invited') return 'invited';
  return u.spec.enabled === false ? 'disabled' : 'active';
}

/** The invitation's link no longer works: the gateway says so, or its expiry has passed. */
export function invitationExpired(inv: Invitation, now: number): boolean {
  if (typeof inv.expired === 'boolean') return inv.expired;
  return new Date(inv.expires_at).getTime() <= now;
}

export function topRole(view: UserView): string | null {
  const roles = view.bindings.map((b) => b.role);
  return roles.sort((a, b) => rank(a) - rank(b))[0] ?? null;
  function rank(role: string) {
    const i = ROLE_RANK.indexOf(role);
    return i === -1 ? ROLE_RANK.length : i;
  }
}

export interface TeamView {
  team: TeamResource;
  name: string;
  displayName: string;
  color: string | undefined;
  organization: string;
  lead: string | null;
  members: string[];
  /** The members with their display names, for avatars. */
  people: { name: string; displayName: string }[];
  projects: string[];
  keys: number;
  monthlyBudget: string | null;
}

export function teamViews(
  teams: TeamResource[],
  users: UserView[],
  projects: ProjectResource[],
  keys: KeyResource[],
): TeamView[] {
  return teams.map((t) => {
    const projectNames = projects.filter((p) => p.spec.team === t.name).map((p) => p.name);
    const memberViews = users.filter(
      (u) => u.primaryTeam === t.name || u.bindings.some((b) => b.scope === `team/${t.name}`),
    );
    const members = memberViews.map((u) => u.name);
    const budgets = (t.spec.policies?.budgets ?? {}) as Record<
      string,
      { amount?: string | number; period?: string; unit?: string } | undefined
    >;
    const monthly = Object.values(budgets).find((b) => b?.period === 'monthly' && b.unit !== 'tokens');
    return {
      team: t,
      name: t.name,
      displayName: t.spec.display_name ?? t.name,
      color: t.spec.color,
      organization: t.spec.organization,
      lead: t.spec.lead ?? null,
      members,
      people: memberViews.map((u) => ({ name: u.name, displayName: u.displayName })),
      projects: projectNames,
      keys: keys.filter((k) => projectNames.includes(k.spec.project ?? 'default')).length,
      monthlyBudget: monthly?.amount !== undefined ? String(monthly.amount) : null,
    };
  });
}
