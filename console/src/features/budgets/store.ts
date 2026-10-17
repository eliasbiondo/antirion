import { create } from 'zustand';

import type {
  BudgetCounter,
  BudgetEvent,
  KeyResource,
  ModelResource,
  OrganizationResource,
  OverageRequest,
  ProjectResource,
  TeamResource,
  UserResource,
} from '@/lib/api/types';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';

import { budgetsApi, type Spend } from './api';

interface BudgetsState {
  counters: Slot<BudgetCounter[]>;
  teams: Slot<TeamResource[]>;
  projects: Slot<ProjectResource[]>;
  organizations: Slot<OrganizationResource[]>;
  keys: Slot<KeyResource[]>;
  users: Slot<UserResource[]>;
  models: Slot<ModelResource[]>;
  events: Slot<BudgetEvent[]>;
  overage: Slot<OverageRequest[]>;
  daily: Slot<{ day: string; cost: number }[]>;
  teamDaily: Slot<Map<string, { day: string; cost: number }[]>>;
  /** Spend by Model per `<level>/<owner>`, this month. */
  modelsUsed: Slot<Map<string, Map<string, Spend>>>;
  /** Requests and spend per person, this month. */
  people: Slot<Map<string, Spend>>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  loadDaily: (from: string, to: string, organization: string | null) => Promise<void>;
  adjust: (counter: BudgetCounter, amount: string, reason: string) => Promise<void>;
  reset: (counter: BudgetCounter, reason: string) => Promise<void>;
  grant: (counter: BudgetCounter, amount: string, expiresAt: string, reason: string) => Promise<void>;
  revokeGrant: (counter: BudgetCounter, id: string, reason?: string) => Promise<void>;
  decideOverage: (id: string, approve: boolean) => Promise<void>;
  readDeclaration: (
    level: string,
    owner: string,
  ) => Promise<{ budgets: Record<string, unknown>; etag: string } | null>;
  saveDeclaration: (
    level: string,
    owner: string,
    budgets: Record<string, Record<string, unknown> | null>,
    reason?: string,
  ) => Promise<void>;
}

/**
 * Budget counters with the hierarchy and declarations that place them, people, Models, events,
 * overage requests, and this month's spend (daily, per team, per Model and per person).
 */
export const useBudgets = create<BudgetsState>()((set, get) => ({
  counters: emptySlot(),
  teams: emptySlot(),
  projects: emptySlot(),
  organizations: emptySlot(),
  keys: emptySlot(),
  users: emptySlot(),
  models: emptySlot(),
  events: emptySlot(),
  overage: emptySlot(),
  daily: emptySlot(),
  teamDaily: emptySlot(),
  modelsUsed: emptySlot(),
  people: emptySlot(),

  async load(options) {
    await Promise.all([
      loadSlot(set, get, 'counters', budgetsApi.counters, options),
      loadSlot(set, get, 'teams', budgetsApi.teams, options),
      loadSlot(set, get, 'projects', budgetsApi.projects, options),
      loadSlot(set, get, 'organizations', budgetsApi.organizations, options),
      loadSlot(set, get, 'keys', budgetsApi.keys, options),
      loadSlot(set, get, 'users', budgetsApi.users, options),
      loadSlot(set, get, 'events', budgetsApi.events, options),
      loadSlot(set, get, 'overage', budgetsApi.overageRequests, options),
      options?.quiet ? Promise.resolve() : loadSlot(set, get, 'models', budgetsApi.models),
    ]);
  },

  async loadDaily(from, to, organization) {
    await Promise.all([
      loadSlot(set, get, 'daily', () => budgetsApi.dailySpend(from, to, organization)),
      loadSlot(set, get, 'teamDaily', () => budgetsApi.teamDaily(from, to, organization)),
      loadSlot(set, get, 'modelsUsed', () => budgetsApi.modelsUsed(from, to)),
      loadSlot(set, get, 'people', () => budgetsApi.people(from, to, organization)),
    ]);
  },

  async adjust(counter, amount, reason) {
    await budgetsApi.adjust(counter, amount, reason);
    await loadSlot(set, get, 'counters', budgetsApi.counters, { quiet: true });
  },

  async reset(counter, reason) {
    await budgetsApi.reset(counter, reason);
    await loadSlot(set, get, 'counters', budgetsApi.counters, { quiet: true });
  },

  async grant(counter, amount, expiresAt, reason) {
    await budgetsApi.grant(counter, amount, expiresAt, reason);
    await Promise.all([
      loadSlot(set, get, 'counters', budgetsApi.counters, { quiet: true }),
      loadSlot(set, get, 'events', budgetsApi.events, { quiet: true }),
    ]);
  },

  async revokeGrant(counter, id, reason) {
    await budgetsApi.revokeGrant(counter, id, reason);
    await loadSlot(set, get, 'counters', budgetsApi.counters, { quiet: true });
  },

  async decideOverage(id, approve) {
    await (approve ? budgetsApi.approveOverage(id) : budgetsApi.denyOverage(id));
    set((s) => ({ overage: { ...s.overage, data: (s.overage.data ?? []).filter((o) => o.id !== id) } }));
  },

  readDeclaration: (level, owner) => budgetsApi.declaration(level, owner),

  async saveDeclaration(level, owner, budgets, reason) {
    const current = await budgetsApi.declaration(level, owner);
    await budgetsApi.declare(level, owner, budgets, current?.etag, reason);
    const quiet = { quiet: true };
    const owners =
      level === 'organization'
        ? loadSlot(set, get, 'organizations', budgetsApi.organizations, quiet)
        : level === 'team'
          ? loadSlot(set, get, 'teams', budgetsApi.teams, quiet)
          : level === 'project'
            ? loadSlot(set, get, 'projects', budgetsApi.projects, quiet)
            : loadSlot(set, get, 'keys', budgetsApi.keys, quiet);
    await Promise.all([loadSlot(set, get, 'counters', budgetsApi.counters, quiet), owners]);
  },
}));
