'use client';

import { useMemo, useState } from 'react';

import { useQueryParam, useSetQueryParams } from '@/components/layout/url-sync';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Combobox, Input, Select } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { FilterBar, PageHeader, SummaryStrip, type SummaryItem, Tabs } from '@/components/ui/layout';
import { StarredFilter, useFavouriteNames, useStarredFilter } from '@/components/ui/favourite';
import { Icon } from '@/components/ui/icon';
import { KeyboardHints } from '@/components/ui/keyboard-hints';
import { MasterDetail, SelectableList } from '@/components/ui/list-detail';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { Avatar, UpdatedAt } from '@/components/ui/misc';
import { RevealOnceDialog } from '@/components/ui/reveal-once';
import { EmptyAction, EmptyState, ErrorState, LoadingRows, NoMatch } from '@/components/ui/states';
import { CountNote } from '@/components/ui/table';
import { downloadCsv } from '@/lib/csv';
import { formatPercent, plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';
import { formatRelative } from '@/lib/time';

import { coverage, INACTIVE_DAYS, isInactive } from '../facts';
import { type Seats, useDirectoryData, useSeats, useTeamViews, useUserViews } from '../hooks';
import { useDirectory, useUsers } from '../store';
import {
  invitationExpired,
  roleLabel,
  STATE_LABEL,
  type TeamView,
  topRole,
  type UserState,
  type UserView,
} from '../types';
import { AccessPanel } from './access-panel';
import { ActivityPanel } from './activity-panel';
import { InviteDialog, TeamFormDrawer, UserFormDrawer } from './forms';
import { OrganizationsPanel } from './organizations-panel';
import { RolesPanel } from './roles-panel';
import { TeamsPanel } from './teams-panel';
import { UserDetail } from './user-detail';

type Tab = 'people' | 'teams' | 'organizations' | 'projects' | 'roles' | 'access' | 'activity';
const TABS: Tab[] = ['people', 'teams', 'organizations', 'projects', 'roles', 'access', 'activity'];

/** A state of the status filter: the gateway's state, or flagged and inactive, which cut across it. */
type StatusFilter = UserState | 'flagged' | 'inactive';
const STATUS_FILTERS: StatusFilter[] = ['active', 'invited', 'disabled', 'flagged', 'inactive'];
const STATUS_LABEL: Record<StatusFilter, string> = {
  ...STATE_LABEL,
  flagged: 'Flagged',
  inactive: `Inactive ${INACTIVE_DAYS}+ days`,
};

interface PeopleFilters {
  role: string;
  states: StatusFilter[];
  team: string;
  query: string;
}

function matchesStatus(u: UserView, states: StatusFilter[], now: number): boolean {
  if (states.length === 0) return true;
  return states.some((st) =>
    st === 'flagged' ? u.flagged : st === 'inactive' ? isInactive(u, now) : u.state === st,
  );
}

const EMPTY_PEOPLE: PeopleFilters = { role: '', states: [], team: '', query: '' };

/** The people list's filters as URL parameters (with the Starred chip's), so a copied link keeps them. */
const PEOPLE_URL = { q: '', role: '', state: [] as string[], team: '', starred: false };

function peopleOf(u: typeof PEOPLE_URL): PeopleFilters {
  return {
    query: u.q,
    role: u.role,
    states: u.state.filter((s): s is StatusFilter => (STATUS_FILTERS as string[]).includes(s)),
    team: u.team,
  };
}

function peopleToUrl(f: PeopleFilters): Omit<typeof PEOPLE_URL, 'starred'> {
  return { q: f.query, role: f.role, state: f.states, team: f.team };
}

function PersonRow({ view, starred, now }: { view: UserView; starred: boolean; now: number }) {
  const role = topRole(view);
  const inactive = isInactive(view, now);
  const expired = view.invitation !== null && invitationExpired(view.invitation, now);
  return (
    <div className="flex items-center gap-3">
      <Avatar name={view.displayName} />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-body font-semibold text-fg">{view.displayName}</span>
          {starred ? (
            <span className="text-star" aria-label="Starred">
              <Icon name="starFilled" size={12} />
            </span>
          ) : null}
          <span className="mono truncate text-caption text-muted">
            {view.name !== view.displayName ? view.name : ''}
          </span>
        </div>
        <div className="mt-0.5 truncate text-caption text-muted">
          {view.kind === 'service' ? 'Service account' : view.email || 'No email'}
          {view.primaryTeam ? ` · ${view.primaryTeam}` : ''}
          {view.invitation
            ? ` · invited ${formatRelative(view.invitation.created_at)} by ${view.invitation.invited_by}, ${expired ? 'link expired' : 'expires'} ${formatRelative(view.invitation.expires_at)}`
            : view.user
              ? ` · ${view.lastActive ? `active ${formatRelative(view.lastActive)}` : 'never active'}`
              : ''}
        </div>
      </div>
      <div className="flex shrink-0 flex-col items-end gap-1">
        {role ? <Badge tone="warn">{roleLabel(role)}</Badge> : null}
        <span className="flex gap-1">
          {view.flagged ? (
            <Badge tone="warn" dot>
              Flagged
            </Badge>
          ) : null}
          {inactive ? (
            <Badge dot title={`No activity for more than ${INACTIVE_DAYS} days`}>
              Inactive
            </Badge>
          ) : (
            <Badge
              tone={
                expired
                  ? 'warn'
                  : view.state === 'active'
                    ? 'success'
                    : view.state === 'invited'
                      ? 'info'
                      : 'neutral'
              }
              dot
            >
              {expired ? 'Invitation expired' : STATE_LABEL[view.state]}
            </Badge>
          )}
        </span>
      </div>
    </div>
  );
}

function DirectorySummary({
  users,
  teams,
  seats,
  now,
  onFilter,
}: {
  users: UserView[];
  teams: TeamView[];
  seats: Seats;
  now: number;
  onFilter: (f: Partial<PeopleFilters>) => void;
}) {
  const active = users.filter((u) => u.state === 'active').length;
  const invited = users.filter((u) => u.state === 'invited').length;
  const flagged = users.filter((u) => u.flagged).length;
  const inactive = users.filter((u) => isInactive(u, now)).length;
  const cov = coverage(users);
  const nextExpiry =
    users
      .map((u) => u.invitation?.expires_at)
      .filter((t): t is string => Boolean(t) && new Date(t ?? '').getTime() > now)
      .sort()[0] ?? null;
  const kinds = [
    cov.kinds.totp ? `${cov.kinds.totp} app` : null,
    cov.kinds.webauthn ? `${cov.kinds.webauthn} security key` : null,
    cov.kinds.idp ? `${cov.kinds.idp} SSO` : null,
    `${cov.kinds.none} none`,
  ].filter(Boolean);
  const items: SummaryItem[] = [
    {
      label: 'Active users',
      value: active,
      tone: seats.full ? 'warn' : 'neutral',
      sub:
        seats.cap !== null
          ? seats.full
            ? `All ${seats.cap} seats in use`
            : `${seats.left} of ${seats.cap} seats left`
          : undefined,
      title: seats.cap !== null ? 'Service accounts take no seat' : undefined,
      onClick: () => onFilter({ states: ['active'] }),
    },
    {
      label: 'Invited, not joined yet',
      value: invited,
      tone: invited > 0 ? 'info' : 'neutral',
      sub: nextExpiry ? `Next link expires ${formatRelative(nextExpiry)}` : 'No open invitations',
      onClick: () => onFilter({ states: ['invited'] }),
    },
    {
      label: 'Need review',
      value: flagged + inactive,
      tone: flagged > 0 ? 'warn' : 'neutral',
      sub: `${flagged} flagged · ${inactive} inactive`,
      title: `Flagged: a key they own has unusual activity. Inactive: no activity for more than ${INACTIVE_DAYS} days.`,
      onClick: () => onFilter({ states: ['flagged', 'inactive'] }),
    },
    {
      label: 'Teams',
      value: teams.length,
      sub: plural(
        teams.reduce((a, t) => a + t.projects.length, 0),
        'project',
      ),
    },
    {
      label: 'Sign in with SSO',
      value: cov.people > 0 ? formatPercent(cov.sso / cov.people, 0) : '—',
      sub: cov.people > 0 ? `${cov.sso} of ${plural(cov.people, 'person', 'people')}` : 'No active people',
      title: 'Active people whose last sign-in was single sign-on',
    },
    {
      label: 'Two-step verification',
      value: cov.people > 0 ? formatPercent(cov.mfa / cov.people, 0) : '—',
      tone: cov.people > 0 && cov.mfa < cov.people ? 'warn' : 'neutral',
      sub: cov.people > 0 ? kinds.join(' · ') : 'No active people',
      title:
        'Active people with an authenticator app, a security key, or two-step asserted by single sign-on',
    },
  ];
  return <SummaryStrip label="Directory summary" items={items} />;
}

/** Teams and users: people with roles, teams with budgets, and the organizations and projects. */
export function TeamsUsersScreen() {
  useDirectoryData();
  const status = useUsers((s) => s.status);
  const error = useUsers((s) => s.error);
  const loadedAt = useUsers((s) => s.loadedAt);
  const fetch = useUsers((s) => s.fetch);
  const roles = useDirectory((s) => s.roles.data);
  const users = useUserViews();
  const teams = useTeamViews(users);
  const [tabParam, setTab] = useQueryParam('tab');
  const tab: Tab = TABS.includes(tabParam as Tab) ? (tabParam as Tab) : 'people';
  const canCreateUser = useCan('createUser');
  const canCreateTeam = useCan('createTeam');
  const canInvite = useCan('createInvitation');
  const [selected, setSelected] = useQueryParam('user');
  const setParams = useSetQueryParams();
  const url = useUrlFilters(PEOPLE_URL);
  const filters = useMemo(() => peopleOf(url.filters), [url.filters]);
  const setFilters = (next: PeopleFilters) => url.set(peopleToUrl(next));
  const [drawer, setDrawer] = useState<'user' | 'service' | 'edit-user' | 'team' | 'invite' | null>(null);
  const [secret, setSecret] = useState<{ title: string; value: string; label?: string } | null>(null);
  const now = useNow(60_000);
  const seats = useSeats();

  const filteredUsers = useMemo(() => {
    const q = filters.query.trim().toLowerCase();
    return users.filter((u) => {
      if (filters.role && !u.bindings.some((b) => b.role === filters.role)) return false;
      if (!matchesStatus(u, filters.states, now)) return false;
      if (
        filters.team &&
        u.primaryTeam !== filters.team &&
        !u.bindings.some((b) => b.scope === `team/${filters.team}`)
      )
        return false;
      return !q || [u.name, u.displayName, u.email].join(' ').toLowerCase().includes(q);
    });
  }, [users, filters, now]);
  const starred = useStarredFilter('user');
  const starredNames = useFavouriteNames('user');
  const visible = starred.apply(filteredUsers, (u) => u.name);
  const current = users.find((u) => u.name === selected) ?? visible[0];

  usePageShortcut('n', () => setDrawer(tab === 'teams' ? 'team' : seats.full ? null : 'invite'));
  const seatsFull = seats.full
    ? `All ${seats.cap ?? ''} seats are in use. Disable someone or raise the seat cap to add people.`
    : undefined;
  usePageShortcut('e', () => exportDirectory(users));

  const more: MenuItem[] = [];
  if (canCreateUser) {
    more.push({
      label: 'Add user without inviting',
      icon: 'plus',
      disabled: seats.full,
      disabledReason: seatsFull,
      onSelect: () => setDrawer('user'),
    });
    more.push({ label: 'Create service account', icon: 'server', onSelect: () => setDrawer('service') });
  }
  // The primary action follows the tab: Create team on Teams, Invite people elsewhere; the other
  // one stays in More.
  if (canCreateTeam && tab !== 'teams')
    more.push({ label: 'Create team', icon: 'users', onSelect: () => setDrawer('team') });
  if (tab === 'teams' && canInvite)
    more.unshift({
      label: 'Invite people',
      icon: 'mail',
      disabled: seats.full,
      disabledReason: seatsFull,
      onSelect: () => setDrawer('invite'),
    });
  more.push({ label: 'Export users (CSV)', icon: 'download', onSelect: () => exportDirectory(users) });

  return (
    <>
      <PageHeader
        title="Teams & users"
        description="Who can sign in to this console, with which roles, and the teams they work in"
        actions={
          <>
            <UpdatedAt at={loadedAt} />
            <MenuButton items={more} label="More" />
            {tab === 'teams' ? (
              <Gate operation="createTeam" mode="hide">
                <Button icon="plus" variant="primary" onClick={() => setDrawer('team')}>
                  Create team
                </Button>
              </Gate>
            ) : (
              <Gate operation="createInvitation" mode="hide">
                <Button
                  icon="mail"
                  variant="primary"
                  disabled={seats.full}
                  disabledReason={seatsFull}
                  onClick={() => setDrawer('invite')}
                >
                  Invite people
                </Button>
              </Gate>
            )}
          </>
        }
      />
      {users.length > 0 ? (
        <DirectorySummary
          users={users}
          teams={teams}
          seats={seats}
          now={now}
          onFilter={(f) =>
            url.setWith({ ...peopleToUrl({ ...EMPTY_PEOPLE, ...f }), starred: false }, { tab: null })
          }
        />
      ) : null}
      <Tabs<Tab>
        label="Directory views"
        value={tab}
        onChange={(t) =>
          // A person selected on Users does not follow to the other tabs.
          setParams({ tab: t === 'people' ? null : t, ...(t !== 'people' ? { user: null } : {}) })
        }
        items={[
          { value: 'people', label: 'Users', count: users.length, icon: 'users' },
          { value: 'teams', label: 'Teams', count: teams.length, icon: 'cube' },
          { value: 'organizations', label: 'Organizations', icon: 'globe' },
          { value: 'projects', label: 'Projects', icon: 'list' },
          { value: 'roles', label: 'Roles', icon: 'shield' },
          { value: 'access', label: 'Access', icon: 'lock' },
          { value: 'activity', label: 'Activity', icon: 'clock' },
        ]}
      />
      {tab === 'teams' ? (
        <TeamsPanel
          teams={teams}
          onNew={() => setDrawer('team')}
          onTeam={(team) =>
            url.setWith({ ...peopleToUrl({ ...EMPTY_PEOPLE, team }), starred: false }, { tab: null })
          }
        />
      ) : null}
      {tab === 'organizations' || tab === 'projects' ? <OrganizationsPanel show={tab} /> : null}
      {tab === 'roles' ? <RolesPanel users={users} /> : null}
      {tab === 'access' ? <AccessPanel users={users} /> : null}
      {tab === 'activity' ? (
        <ActivityPanel
          onUser={(name) => {
            setTab(null);
            setSelected(name);
          }}
        />
      ) : null}
      {tab === 'people' ? (
        <>
          {users.length > 0 ? (
            <FilterBar>
              <div className="w-64 max-sm:w-full">
                <Input
                  data-page-search
                  aria-label="Search users"
                  icon="search"
                  value={filters.query}
                  onChange={(e) => setFilters({ ...filters, query: e.target.value })}
                  placeholder="Search users"
                />
              </div>
              <div className="w-48 max-sm:min-w-[40%] max-sm:flex-1">
                <Select
                  aria-label="Role"
                  value={filters.role}
                  onChange={(v) => setFilters({ ...filters, role: v })}
                  emptyOption="Any role"
                  options={(roles ?? []).map((r) => ({
                    value: r.name,
                    label: `${roleLabel(r.name)} (${users.filter((u) => u.bindings.some((b) => b.role === r.name)).length})`,
                  }))}
                />
              </div>
              <div className="w-52 max-sm:min-w-[40%] max-sm:flex-1">
                <Select
                  aria-label="Status"
                  value={filters.states.join(',')}
                  onChange={(v) =>
                    setFilters({ ...filters, states: v ? (v.split(',') as StatusFilter[]) : [] })
                  }
                  emptyOption="Any status"
                  options={[
                    ...STATUS_FILTERS.map((st) => ({
                      value: st,
                      label: `${STATUS_LABEL[st]} (${users.filter((u) => matchesStatus(u, [st], now)).length})`,
                    })),
                    {
                      value: 'flagged,inactive',
                      label: `Need review (${users.filter((u) => matchesStatus(u, ['flagged', 'inactive'], now)).length})`,
                    },
                  ]}
                />
              </div>
              {teams.length > 1 ? (
                <div className="w-40 max-sm:min-w-[40%] max-sm:flex-1">
                  <Combobox
                    aria-label="Team"
                    value={filters.team}
                    onChange={(v) => setFilters({ ...filters, team: v })}
                    emptyOption="All teams"
                    options={teams.map((t) => ({ value: t.name, label: t.displayName }))}
                  />
                </div>
              ) : null}
              <StarredFilter kind="user" pressed={starred.on} onChange={starred.set} />
              {url.active ? (
                <Button size="sm" variant="ghost" onClick={() => url.clear()}>
                  Clear filters
                </Button>
              ) : null}
              <span className="ml-auto">
                <CountNote shown={visible.length} total={users.length} noun="users" />
              </span>
            </FilterBar>
          ) : null}
          {status === 'loading' && users.length === 0 ? <LoadingRows /> : null}
          {status === 'error' ? (
            <ErrorState error={error} subject="Users" onRetry={() => void fetch()} />
          ) : null}
          {status === 'ready' && users.length === 0 ? (
            <EmptyState
              title="No users yet"
              description="People you invite sign in with the role you choose."
              action={
                <Gate operation="createInvitation" mode="hide">
                  <EmptyAction onClick={() => setDrawer('invite')}>Invite people</EmptyAction>
                </Gate>
              }
              secondaryAction={
                <Gate operation="createUser" mode="hide">
                  <EmptyAction onClick={() => setDrawer('user')}>Add user without inviting</EmptyAction>
                </Gate>
              }
            />
          ) : null}
          {users.length > 0 ? (
            <MasterDetail
              sticky
              narrow={{
                open: Boolean(selected && current),
                onClose: () => setSelected(null),
                title: current?.displayName ?? '',
              }}
              list={
                visible.length === 0 ? (
                  <NoMatch onClear={() => url.clear()} />
                ) : (
                  <SelectableList
                    label="People"
                    items={visible}
                    keyOf={(u) => u.name}
                    selectedKey={current?.name ?? null}
                    onSelect={(name) => setSelected(name)}
                    toneOf={(u) =>
                      u.state === 'active' ? 'success' : u.state === 'invited' ? 'info' : 'neutral'
                    }
                    renderItem={(u) => <PersonRow view={u} starred={starredNames.has(u.name)} now={now} />}
                  />
                )
              }
              detail={
                current ? (
                  <UserDetail
                    key={current.name}
                    view={current}
                    teams={teams}
                    onEdit={() => setDrawer('edit-user')}
                    onSecret={(title, value) => setSecret({ title, value })}
                  />
                ) : (
                  <EmptyState title="Select a user to see their details" />
                )
              }
            />
          ) : null}
          {users.length > 0 ? <KeyboardHints keys={['search', 'move', 'open', 'export', 'new']} /> : null}
        </>
      ) : null}
      {drawer === 'user' || drawer === 'service' ? (
        <UserFormDrawer
          kind={drawer === 'service' ? 'service' : 'human'}
          onClose={() => setDrawer(null)}
          onSaved={(n) => setSelected(n)}
        />
      ) : null}
      {drawer === 'edit-user' && current?.user ? (
        <UserFormDrawer editing={current.user} onClose={() => setDrawer(null)} onSaved={() => undefined} />
      ) : null}
      {drawer === 'team' ? (
        <TeamFormDrawer onClose={() => setDrawer(null)} onSaved={() => setTab('teams')} />
      ) : null}
      {drawer === 'invite' ? (
        <InviteDialog
          onClose={() => setDrawer(null)}
          onLink={(link) => setSecret({ title: 'Invitation link', value: link, label: 'Link' })}
        />
      ) : null}
      <RevealOnceDialog
        secret={secret?.value ?? null}
        onClose={() => setSecret(null)}
        title={secret?.title ?? ''}
        label={secret?.label ?? 'Secret'}
      />
    </>
  );
}

function exportDirectory(users: UserView[]) {
  downloadCsv(
    'directory.csv',
    ['user', 'display_name', 'email', 'kind', 'state', 'primary_team', 'roles', 'keys_owned'],
    users.map((u) => [
      u.name,
      u.displayName,
      u.email,
      u.kind,
      u.state,
      u.primaryTeam,
      u.bindings.map((b) => `${b.role}@${b.scope}`).join(' '),
      u.keys,
    ]),
  );
}
