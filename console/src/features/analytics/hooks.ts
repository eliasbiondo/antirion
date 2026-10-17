'use client';

import { useEffect, useEffectEvent, useMemo } from 'react';

import { usePolling } from '@/lib/hooks';
import { scopeOrganization, useScope } from '@/stores/scope';
import { useQueryWindow, useResolvedRange } from '@/stores/time-range';

import type { UsageQuery } from './api';
import { useAnalytics } from './store';

/** The page window as usage queries: the current range by bucket, and the previous equal window. */
export function useUsageQueries(): { current: UsageQuery; previous: UsageQuery; key: string } {
  const window = useQueryWindow();
  const range = useResolvedRange();
  const scope = useScope((s) => s.scope);
  const environment = useScope((s) => s.environment);
  return useMemo(() => {
    const base = { organization: scopeOrganization(scope), tag: environment ? `env:${environment}` : null };
    return {
      current: { ...base, from: window.from, to: window.to, interval: window.interval },
      previous: {
        ...base,
        from: range.previous.from.toISOString(),
        to: range.previous.to.toISOString(),
        interval: window.interval,
      },
      key: `${window.key}|${scopeOrganization(scope) ?? ''}|${environment ?? ''}`,
    };
  }, [window, range, scope, environment]);
}

/** Loads named usage queries whenever the page window or scope changes, and polls them. */
export function useUsage(queries: Record<string, UsageQuery>, key: string): void {
  const load = useAnalytics((s) => s.loadUsage);
  const run = useEffectEvent(() => {
    for (const [name, q] of Object.entries(queries)) void load(name, q);
  });
  useEffect(() => {
    run();
  }, [key]);
  usePolling(() => {
    for (const [name, q] of Object.entries(queries)) void load(name, q, { quiet: true });
  });
}
