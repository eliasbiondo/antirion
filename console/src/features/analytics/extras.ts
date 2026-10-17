'use client';

import { useEffect, useEffectEvent } from 'react';
import { create } from 'zustand';

import { usePermission } from '@/components/ui/gate';
import type { LogRecord, UsageBucket } from '@/lib/api/types';
import { usePolling } from '@/lib/hooks';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';
import type { Interval } from '@/lib/time';

import { analyticsExtrasApi, type Generation, type ProviderPressure } from './api';
import { hasGatewayPermission, useSession } from '@/stores/session';

export interface LatencySlo {
  name: string;
  thresholdMs: number;
  objective: number;
}

/** The fallback seen most often in a window: from which deployment to which, and how often. */
export interface Transition {
  from: string;
  to: string;
  count: number;
  /** Requests served by `to` after a fallback in the window (the usage figure). */
  served: number;
}

interface ExtrasState {
  pressure: Slot<ProviderPressure>;
  pressurePrevious: Slot<ProviderPressure>;
  generations: Slot<Generation[]>;
  slo: Slot<LatencySlo | null>;
  transition: Slot<Transition | null>;
  loadPressure: (from: string, to: string, interval: Interval | undefined, quiet?: boolean) => Promise<void>;
  loadPressurePrevious: (from: string, to: string, interval: Interval | undefined) => Promise<void>;
  loadGenerations: (quiet?: boolean) => Promise<void>;
  loadSlo: () => Promise<void>;
  loadTransition: (byDeployment: UsageBucket[], from: string, to: string) => Promise<void>;
}

/** The deployments of a record's attempts, in order, without repeats in a row. */
function trail(r: LogRecord): string[] {
  const out: string[] = [];
  for (const a of r.attempts) {
    const d = (a as { deployment?: unknown }).deployment;
    if (typeof d === 'string' && out.at(-1) !== d) out.push(d);
  }
  return out;
}

/**
 * The most common transition: the deployment that served the most requests after a fallback
 * (usage by deployment), and the deployment its newest such records fell back from (their trails).
 */
export function mostCommonTransition(
  byDeployment: UsageBucket[],
  records: (to: string) => Promise<LogRecord[]>,
) {
  const totals = new Map<string, number>();
  for (const b of byDeployment) {
    const d = b.group.deployment;
    if (d && (b.fallbacks ?? 0) > 0) totals.set(d, (totals.get(d) ?? 0) + (b.fallbacks ?? 0));
  }
  const top = [...totals.entries()].sort((a, b) => b[1] - a[1])[0];
  if (!top) return Promise.resolve(null);
  const [to, served] = top;
  return records(to).then((list) => {
    const counts = new Map<string, number>();
    for (const r of list) {
      const t = trail(r);
      if (t.length > 1 && t.at(-1) === to) {
        const from = t[t.length - 2] ?? '';
        counts.set(from, (counts.get(from) ?? 0) + 1);
      }
    }
    const best = [...counts.entries()].sort((a, b) => b[1] - a[1])[0];
    return best ? { from: best[0], to, count: best[1], served } : { from: '', to, count: 0, served };
  });
}

/** Pressure, configuration changes, the latency SLO and the fallback trail for the Analytics screen. */
export const useAnalyticsExtras = create<ExtrasState>()((set, get) => ({
  pressure: emptySlot(),
  pressurePrevious: emptySlot(),
  generations: emptySlot(),
  slo: emptySlot(),
  transition: emptySlot(),
  loadPressure: (from, to, interval, quiet = false) =>
    loadSlot(set, get, 'pressure', () => analyticsExtrasApi.pressure(from, to, interval), { quiet }),
  loadPressurePrevious: (from, to, interval) =>
    loadSlot(set, get, 'pressurePrevious', () => analyticsExtrasApi.pressure(from, to, interval), {
      quiet: true,
    }),
  loadGenerations: (quiet = false) =>
    loadSlot(set, get, 'generations', analyticsExtrasApi.generations, { quiet }),
  loadSlo: () => loadSlot(set, get, 'slo', analyticsExtrasApi.latencySlo, { quiet: true }),
  loadTransition: (byDeployment, from, to) =>
    loadSlot(
      set,
      get,
      'transition',
      () => mostCommonTransition(byDeployment, (d) => analyticsExtrasApi.served(d, from, to)),
      { quiet: true },
    ),
}));

/** Loads the extras the reader may see for the page window, and keeps pressure fresh. */
export function useExtrasLoader(window: {
  from: string;
  to: string;
  interval: Interval;
  previousFrom: string;
  previousTo: string;
  key: string;
}): void {
  const canConfig = usePermission('config:read');
  // Pressure and objectives are gateway-wide reports.
  const canGatewayReports = hasGatewayPermission(
    useSession((s) => s.me),
    'analytics:read',
  );
  const load = (quiet: boolean) => {
    const s = useAnalyticsExtras.getState();
    if (canGatewayReports) {
      void s.loadPressure(window.from, window.to, window.interval, quiet);
      if (!quiet) void s.loadPressurePrevious(window.previousFrom, window.previousTo, undefined);
    }
    if (canConfig) {
      void s.loadGenerations(quiet);
      if (!quiet && canGatewayReports) void s.loadSlo();
    }
  };
  const run = useEffectEvent(() => load(false));
  useEffect(() => {
    run();
  }, [window.key, canConfig, canGatewayReports]);
  usePolling(() => load(true));
}
