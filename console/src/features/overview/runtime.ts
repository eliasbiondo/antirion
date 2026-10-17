'use client';

import { useEffect } from 'react';
import { create } from 'zustand';

import { usePermission } from '@/components/ui/gate';
import type { BudgetCounter } from '@/lib/api/types';
import { usePolling } from '@/lib/hooks';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';
import type { Scope } from '@/stores/scope';

import { budgetsApi } from '@/features/budgets/api';
import { providersApi, type ProviderStatus } from '@/features/providers/api';

interface RuntimeState {
  budgets: Slot<BudgetCounter[]>;
  providers: Slot<ProviderStatus[]>;
  loadBudgets: (quiet?: boolean) => Promise<void>;
  loadProviders: (quiet?: boolean) => Promise<void>;
}

/** The Overview's runtime readings: budget counters and the providers' upstream rate limits. */
export const useOverviewRuntime = create<RuntimeState>()((set, get) => ({
  budgets: emptySlot(),
  providers: emptySlot(),
  loadBudgets: (quiet = false) => loadSlot(set, get, 'budgets', budgetsApi.counters, { quiet }),
  loadProviders: (quiet = false) => loadSlot(set, get, 'providers', providersApi.status, { quiet }),
}));

/** Loads the readings the reader may see, and keeps them fresh. */
export function useRuntimeReadings(): void {
  const canBudgets = usePermission('policies:read');
  const canProviders = usePermission('providers:read');
  const loadBudgets = useOverviewRuntime((s) => s.loadBudgets);
  const loadProviders = useOverviewRuntime((s) => s.loadProviders);
  useEffect(() => {
    if (canBudgets) void loadBudgets();
    if (canProviders) void loadProviders();
  }, [canBudgets, canProviders, loadBudgets, loadProviders]);
  usePolling(() => {
    if (canBudgets) void loadBudgets(true);
    if (canProviders) void loadProviders(true);
  });
}

const DAY = 86_400_000;

/** Whether a counter's period is a calendar month (28 to 31 days), so it reads as month to date. */
export function isMonthly(c: BudgetCounter): boolean {
  if (!c.resets_at) return false;
  const days = (new Date(c.resets_at).getTime() - new Date(c.period_start).getTime()) / DAY;
  return days >= 27.5 && days <= 31.5;
}

/** The scope's own monthly dollar budget: the gateway's, or the organization's. */
export function scopeBudget(counters: BudgetCounter[] | null, scope: Scope): BudgetCounter | null {
  const list = (counters ?? []).filter((c) => c.unit === 'usd' && c.partition === null && isMonthly(c));
  return (
    (scope.kind === 'gateway'
      ? list.find((c) => c.level === 'gateway')
      : list.find((c) => c.level === 'organization' && c.owner === scope.name)) ?? null
  );
}

/** Each team's monthly dollar budget, by team name. */
export function teamBudgets(counters: BudgetCounter[] | null): Map<string, BudgetCounter> {
  const out = new Map<string, BudgetCounter>();
  for (const c of counters ?? [])
    if (c.level === 'team' && c.unit === 'usd' && c.partition === null && isMonthly(c) && !out.has(c.owner))
      out.set(c.owner, c);
  return out;
}

/** The month so far as a fraction of the month (the projection's basis). */
export function monthElapsed(now: number): number {
  const d = new Date(now);
  const start = Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1);
  const end = Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1);
  return Math.min(1, Math.max(1 / (end - start), (now - start) / (end - start)));
}

/** A provider's request headroom from the latest upstream reading, when the provider reports one. */
export interface Headroom {
  used: number;
  limit: number;
  share: number;
  unit: 'requests' | 'tokens';
  tier: string | null;
  stale: boolean;
}

export function headroom(p: ProviderStatus): Headroom | null {
  const rl = p.rate_limits;
  if (!rl) return null;
  for (const unit of ['requests', 'tokens'] as const) {
    const r = rl[unit];
    if (r?.limit != null && r.limit > 0 && r.remaining != null) {
      const used = Math.max(0, r.limit - r.remaining);
      return {
        used,
        limit: r.limit,
        share: used / r.limit,
        unit,
        tier: p.account_tier ?? null,
        stale: rl.stale === true,
      };
    }
  }
  return null;
}
