'use client';

import { useEffect, useMemo } from 'react';

import type { BudgetCounter, UserResource } from '@/lib/api/types';
import { useNow, usePolling } from '@/lib/hooks';

import { useKeys, useKeysContext } from './store';
import {
  KEY_STATUSES,
  KEY_TYPES,
  type KeyStatus,
  type KeyType,
  type KeyView,
  keyStatus,
  keyType,
} from './types';

/** Loads the keys screen's data once and keeps the list fresh while the tab is visible. */
export function useKeysScreenData(): void {
  const fetch = useKeys((s) => s.fetch);
  const loadDirectory = useKeysContext((s) => s.loadDirectory);
  const loadBudgets = useKeysContext((s) => s.loadBudgets);
  const loadUsage = useKeysContext((s) => s.loadUsage);
  const loadAnomalies = useKeysContext((s) => s.loadAnomalies);
  const loadHourly = useKeysContext((s) => s.loadHourly);
  const loadSettings = useKeysContext((s) => s.loadSettings);
  useEffect(() => {
    void fetch();
    void loadAnomalies();
    void loadHourly();
    void loadSettings();
    void loadDirectory();
    void loadBudgets();
    const to = new Date();
    void loadUsage(new Date(to.getTime() - 86_400_000).toISOString(), to.toISOString());
  }, [fetch, loadDirectory, loadBudgets, loadUsage, loadAnomalies, loadHourly, loadSettings]);
  usePolling(() => void fetch({ quiet: true }));
}

/** Every key with its derived type and status. */
export function useKeyViews(): KeyView[] {
  const items = useKeys((s) => s.items);
  const users = useKeysContext((s) => s.users.data);
  const rotations = useKeysContext((s) => s.rotations);
  const now = useNow(60_000);
  return useMemo(() => {
    const byName = new Map<string, UserResource>((users ?? []).map((u) => [u.name, u]));
    return items.map((raw) => {
      const local = rotations[raw.name];
      const key =
        !raw.metadata.rotation && local ? { ...raw, metadata: { ...raw.metadata, rotation: local } } : raw;
      return {
        key,
        name: key.name,
        type: keyType(key, byName),
        status: keyStatus(key, now),
        project: key.spec.project ?? 'default',
        owner: key.spec.owner ?? null,
      };
    });
  }, [items, users, rotations, now]);
}

/** The owner filter's value for keys no one owns. */
export const NO_OWNER = '-';

export interface KeyFilters {
  type: KeyType | 'all';
  statuses: KeyStatus[];
  team: string;
  owner: string;
  query: string;
}

export const EMPTY_FILTERS: KeyFilters = { type: 'all', statuses: [], team: '', owner: '', query: '' };

/** The key list's filters as URL parameters (with the Starred chip's), so a copied link keeps them. */
export const KEY_URL_FILTERS = {
  q: '',
  type: 'all',
  status: [] as string[],
  team: '',
  owner: '',
  starred: false,
};
export type KeyUrlFilters = typeof KEY_URL_FILTERS;

/** The filters a URL carries; a type or status the list does not know is ignored. */
export function keyFiltersOf(u: KeyUrlFilters): KeyFilters {
  return {
    query: u.q,
    type: (KEY_TYPES as readonly string[]).includes(u.type) ? (u.type as KeyType) : 'all',
    statuses: u.status.filter((s): s is KeyStatus => (KEY_STATUSES as readonly string[]).includes(s)),
    team: u.team,
    owner: u.owner,
  };
}

/** The URL parameters of some filters (the Starred chip is left as it is). */
export function keyFiltersToUrl(f: KeyFilters): Omit<KeyUrlFilters, 'starred'> {
  return { q: f.query, type: f.type, status: f.statuses, team: f.team, owner: f.owner };
}

export function filterKeys(
  views: KeyView[],
  filters: KeyFilters,
  teamOf: (project: string) => string | undefined,
): KeyView[] {
  const q = filters.query.trim().toLowerCase();
  return views.filter((v) => {
    if (filters.type !== 'all' && v.type !== filters.type) return false;
    if (filters.statuses.length > 0 && !filters.statuses.includes(v.status)) return false;
    if (filters.team && teamOf(v.project) !== filters.team) return false;
    if (filters.owner && (filters.owner === NO_OWNER ? v.owner !== null : v.owner !== filters.owner))
      return false;
    if (!q) return true;
    const hay = [
      v.name,
      v.owner ?? '',
      v.key.metadata.kid ?? '',
      v.key.metadata.hint ?? '',
      v.key.spec.description ?? '',
      v.project,
    ]
      .join(' ')
      .toLowerCase();
    return hay.includes(q);
  });
}

/** The project → team mapping, for "team derived from the project". */
export function useTeamOfProject(): (project: string) => string | undefined {
  const projects = useKeysContext((s) => s.projects.data);
  return useMemo(() => {
    const map = new Map((projects ?? []).map((p) => [p.name, p.spec.team]));
    return (project: string) => map.get(project);
  }, [projects]);
}

/** The budgets that apply to a key: its own, its project's, team's, organization's and the gateway's. */
export function budgetsFor(
  view: KeyView,
  counters: BudgetCounter[],
  team: string | undefined,
  organization: string | undefined,
): BudgetCounter[] {
  const mine = (c: BudgetCounter) =>
    c.partition === null ||
    c.partition === `key=${view.name}` ||
    (view.owner !== null && c.partition === `user=${view.owner}`);
  return counters.filter(
    (c) =>
      mine(c) &&
      ((c.level === 'key' && c.owner === view.name) ||
        (c.level === 'project' && c.owner === view.project) ||
        (team !== undefined && c.level === 'team' && c.owner === team) ||
        (organization !== undefined && c.level === 'organization' && c.owner === organization) ||
        c.level === 'gateway'),
  );
}

/** A budget's spent share of its amount (0 for a budget without an amount). */
export function budgetRatio(c: BudgetCounter): number {
  const amount = Number(c.amount);
  return amount > 0 ? Number(c.spent) / amount : 0;
}

/** The tightest budget that applies to a key: the one with the largest share spent. */
export function tightestBudget(
  view: KeyView,
  counters: BudgetCounter[],
  team: string | undefined,
  organization: string | undefined,
): BudgetCounter | null {
  return budgetsFor(view, counters, team, organization).reduce<BudgetCounter | null>(
    (best, c) => (!best || budgetRatio(c) > budgetRatio(best) ? c : best),
    null,
  );
}
