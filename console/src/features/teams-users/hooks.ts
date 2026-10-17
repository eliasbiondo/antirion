'use client';

import { useEffect, useMemo } from 'react';

import { usePolling } from '@/lib/hooks';
import { useShell } from '@/stores/shell';

import { useDirectory, useOrganizations, useProjects, useTeams, useUsers } from './store';
import { type TeamView, teamViews, type UserView, userViews } from './types';

export function useDirectoryData(): void {
  const fetchUsers = useUsers((s) => s.fetch);
  const fetchTeams = useTeams((s) => s.fetch);
  const fetchProjects = useProjects((s) => s.fetch);
  const fetchOrgs = useOrganizations((s) => s.fetch);
  const load = useDirectory((s) => s.load);
  useEffect(() => {
    void fetchUsers();
    void fetchTeams();
    void fetchProjects();
    void fetchOrgs();
    void load();
  }, [fetchUsers, fetchTeams, fetchProjects, fetchOrgs, load]);
  usePolling(() => {
    void fetchUsers({ quiet: true });
    void fetchTeams({ quiet: true });
  });
}

export function useUserViews(): UserView[] {
  const users = useUsers((s) => s.items);
  const bindings = useDirectory((s) => s.bindings.data);
  const invitations = useDirectory((s) => s.invitations.data);
  const keys = useDirectory((s) => s.keys.data);
  return useMemo(
    () => userViews(users, bindings ?? [], invitations ?? [], keys ?? []),
    [users, bindings, invitations, keys],
  );
}

export function useTeamViews(users: UserView[]): TeamView[] {
  const teams = useTeams((s) => s.items);
  const projects = useProjects((s) => s.items);
  const keys = useDirectory((s) => s.keys.data);
  return useMemo(() => teamViews(teams, users, projects, keys ?? []), [teams, users, projects, keys]);
}

export interface Seats {
  /** The seat cap, or null when the gateway has none. */
  cap: number | null;
  /** Active people as the gateway counts them (service accounts take no seat). */
  active: number | null;
  left: number | null;
  /** Every seat is taken: inviting, adding or enabling a person would be refused. */
  full: boolean;
}

/** Used and remaining seats from the gateway's status. */
export function useSeats(): Seats {
  const cap = useShell((s) => s.status?.users?.cap ?? null);
  const active = useShell((s) => s.status?.users?.active ?? null);
  const left = cap !== null && active !== null ? Math.max(0, cap - active) : null;
  return { cap, active, left, full: left === 0 };
}
