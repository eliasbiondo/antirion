'use client';

import { useCallback, useEffect, useMemo } from 'react';

import type { UserResource } from '@/lib/api/types';
import { useNow, usePolling } from '@/lib/hooks';
import { scopeOrganization, useScope } from '@/stores/scope';

import type { Spend } from './api';
import { type Person, peopleFacts } from './people';
import { useBudgets } from './store';
import { type BudgetRow, buildTree } from './types';

/** The first of this month (UTC), the start of the month's spend reads. */
export function monthStart(now = new Date()): Date {
  return new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1));
}

export function useBudgetsData(): void {
  const load = useBudgets((s) => s.load);
  const loadDaily = useBudgets((s) => s.loadDaily);
  const scope = useScope((s) => s.scope);
  useEffect(() => {
    void load();
    const now = new Date();
    void loadDaily(monthStart(now).toISOString(), now.toISOString(), scopeOrganization(scope));
  }, [load, loadDaily, scope]);
  usePolling(() => void load({ quiet: true }));
}

/** A budget's declaration as its owner states it (from the loaded owners). */
export function useDeclarations(): (
  level: string,
  owner: string,
  name: string,
) => Record<string, unknown> | undefined {
  const organizations = useBudgets((s) => s.organizations.data);
  const teams = useBudgets((s) => s.teams.data);
  const projects = useBudgets((s) => s.projects.data);
  const keys = useBudgets((s) => s.keys.data);
  return useMemo(() => {
    const maps: Record<string, Map<string, Record<string, unknown>>> = {};
    const add = (
      level: string,
      list: { name: string; spec: { policies?: { budgets?: unknown } } }[] | null,
    ) => {
      maps[level] = new Map(
        (list ?? []).map((r) => [r.name, (r.spec.policies?.budgets ?? {}) as Record<string, unknown>]),
      );
    };
    add('organization', organizations);
    add('team', teams);
    add('project', projects);
    add('key', keys);
    return (level: string, owner: string, name: string) => {
      const d = maps[level]?.get(owner)?.[name];
      return d && typeof d === 'object' ? (d as Record<string, unknown>) : undefined;
    };
  }, [organizations, teams, projects, keys]);
}

/** People by name. */
export function useUsersByName(): Map<string, UserResource> {
  const users = useBudgets((s) => s.users.data);
  return useMemo(() => new Map((users ?? []).map((u) => [u.name, u])), [users]);
}

/** The budget tree within the current scope. */
export function useBudgetTree(): BudgetRow[] {
  const counters = useBudgets((s) => s.counters.data);
  const teams = useBudgets((s) => s.teams.data);
  const projects = useBudgets((s) => s.projects.data);
  const keys = useBudgets((s) => s.keys.data);
  const users = useUsersByName();
  const declaration = useDeclarations();
  const scope = useScope((s) => s.scope);
  const now = useNow(60_000);
  const teamOfUser = useCallback((user: string) => users.get(user)?.spec.primary_team, [users]);
  return useMemo(() => {
    const teamOrg = new Map((teams ?? []).map((t) => [t.name, t.spec.organization]));
    const projectTeam = new Map((projects ?? []).map((p) => [p.name, p.spec.team]));
    const keyProject = new Map((keys ?? []).map((k) => [k.name, k.spec.project ?? 'default']));
    const orgOf = (level: string, owner: string): string | null => {
      if (level === 'organization') return owner;
      if (level === 'team') return teamOrg.get(owner) ?? null;
      if (level === 'project') return orgOf('team', projectTeam.get(owner) ?? '');
      if (level === 'key') return orgOf('project', keyProject.get(owner) ?? '');
      return null;
    };
    const parentOf = (level: string, owner: string) => {
      if (level === 'key') {
        const project = keyProject.get(owner);
        return project ? { level: 'project', owner: project } : null;
      }
      if (level === 'project') {
        const team = projectTeam.get(owner);
        return team ? { level: 'team', owner: team } : null;
      }
      if (level === 'team') {
        const org = teamOrg.get(owner);
        return org ? { level: 'organization', owner: org } : null;
      }
      return null;
    };
    const org = scopeOrganization(scope);
    const inScope = (counters ?? []).filter((c) => {
      if (!org) return true;
      const of = orgOf(c.level, c.owner);
      return of === org || (c.level === 'key' && of === null);
    });
    return buildTree(inScope, { parentOf, declaration, teamOfUser }, now);
  }, [counters, teams, projects, keys, scope, now, declaration, teamOfUser]);
}

/** People with this month's figures, within the scope (their team's organization, or any spend). */
export function usePeople(): Person[] | null {
  const users = useBudgets((s) => s.users.data);
  const usage = useBudgets((s) => s.people.data);
  const teams = useBudgets((s) => s.teams.data);
  const scope = useScope((s) => s.scope);
  return useMemo(() => {
    if (!users) return null;
    const org = scopeOrganization(scope);
    const teamOrg = new Map((teams ?? []).map((t) => [t.name, t.spec.organization]));
    const all = peopleFacts(users, usage ?? new Map<string, Spend>());
    if (!org) return all;
    return all.filter((p) => p.cost > 0 || (p.team !== null && teamOrg.get(p.team) === org));
  }, [users, usage, teams, scope]);
}
