'use client';

import { usePathname, useRouter, useSearchParams } from 'next/navigation';
import { useCallback, useEffect, useEffectEvent, useMemo, useRef } from 'react';

import { SETTINGS_PARAM } from '@/lib/nav';
import {
  decodeCompare,
  decodeRange,
  defaultInterval,
  encodeCompare,
  encodeRange,
  INTERVALS,
  type Interval,
  isPreset,
} from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { hasPermission, isGatewayPrincipal, useSession } from '@/stores/session';
import { decodeScope, encodeScope, LAST_ORGANIZATION_KEY, type Scope, useScope } from '@/stores/scope';
import { useTimeRange } from '@/stores/time-range';

/** Query parameters every console URL carries, so a view can be shared and reloaded. */
export const SHARED_PARAMS = ['org', 'range', 'bucket', 'env', 'cmp'] as const;

function isInterval(value: string | null): value is Interval {
  return value !== null && value in INTERVALS;
}

function defaultScope(): Scope {
  const me = useSession.getState().me;
  if (isGatewayPrincipal(me)) return { kind: 'gateway' };
  const preferred = usePreferences.getState().defaultOrganization;
  if (preferred && hasPermission(me, 'organizations:read', preferred))
    return { kind: 'organization', name: preferred };
  const orgScope = Object.keys(me?.permissions ?? {}).find((s) => s.startsWith('organization/'));
  if (orgScope) return { kind: 'organization', name: orgScope.slice('organization/'.length) };
  let remembered: string | null = null;
  try {
    remembered = localStorage.getItem(LAST_ORGANIZATION_KEY);
  } catch {
    remembered = null;
  }
  if (remembered && hasPermission(me, 'organizations:read', remembered))
    return { kind: 'organization', name: remembered };
  return { kind: 'organization', name: 'default' };
}

/** The shared parameters as the stores hold them now. */
function storeParams(base: URLSearchParams): URLSearchParams {
  const { range, interval, compare } = useTimeRange.getState();
  const { scope, environment } = useScope.getState();
  const next = new URLSearchParams(base.toString());
  next.set('org', encodeScope(scope));
  next.set('range', encodeRange(range));
  if (interval !== defaultInterval(range)) next.set('bucket', interval);
  else next.delete('bucket');
  if (environment) next.set('env', environment);
  else next.delete('env');
  const cmp = encodeCompare(compare);
  if (cmp) next.set('cmp', cmp);
  else next.delete('cmp');
  return next;
}

/**
 * Keeps the scope, environment, time range and bucket in the URL. The URL wins when it changes
 * (load, navigation, a pasted link); a change in the stores rewrites the URL in place.
 */
export function UrlSync() {
  const router = useRouter();
  const pathname = usePathname();
  const params = useSearchParams();

  const range = useTimeRange((s) => s.range);
  const interval = useTimeRange((s) => s.interval);
  const scope = useScope((s) => s.scope);
  const environment = useScope((s) => s.environment);
  const compare = useTimeRange((s) => s.compare);
  const first = useRef(true);
  const shownPath = useRef(pathname);

  const writeUrl = useEffectEvent(() => {
    const current = new URLSearchParams(window.location.search);
    const next = storeParams(current);
    if (next.toString() !== current.toString())
      router.replace(`${pathname}?${next.toString()}`, { scroll: false });
  });

  useEffect(() => {
    const tr = useTimeRange.getState();
    const preferredRange = usePreferences.getState().defaultRange;
    const urlRange =
      decodeRange(params.get('range')) ??
      (first.current && preferredRange && isPreset(preferredRange)
        ? { kind: 'preset' as const, preset: preferredRange }
        : null);
    first.current = false;
    if (urlRange && encodeRange(urlRange) !== encodeRange(tr.range)) tr.setRange(urlRange);
    const urlCompare = decodeCompare(params.get('cmp'));
    if (encodeCompare(urlCompare) !== encodeCompare(useTimeRange.getState().compare))
      useTimeRange.getState().setCompare(urlCompare);
    const bucket = params.get('bucket');
    if (isInterval(bucket) && bucket !== useTimeRange.getState().interval)
      useTimeRange.getState().setInterval(bucket);
    const sc = useScope.getState();
    const nextScope = decodeScope(params.get('org')) ?? (params.has('org') ? sc.scope : defaultScope());
    if (encodeScope(nextScope) !== encodeScope(sc.scope)) sc.setScope(nextScope);
    const env = params.get('env') ?? null;
    if (env !== sc.environment) sc.setEnvironment(env);
    writeUrl();
  }, [params]);

  useEffect(() => {
    writeUrl();
  }, [range, interval, scope, environment, compare]);

  // The window is resolved once and kept while the console stays open, so without this a page
  // opened later would leave out everything since: a request just sent, absent from the log.
  useEffect(() => {
    if (shownPath.current === pathname) return;
    shownPath.current = pathname;
    useTimeRange.getState().pageOpened();
  }, [pathname]);

  return null;
}

/** An in-console href that keeps the shared parameters (scope, range, environment). */
export function useScopedHref(href: string, extra?: Record<string, string>): string {
  const params = useSearchParams();
  return useMemo(() => {
    const next = new URLSearchParams();
    for (const key of SHARED_PARAMS) {
      const value = params.get(key);
      if (value) next.set(key, value);
    }
    for (const [k, v] of Object.entries(extra ?? {})) next.set(k, v);
    const query = next.toString();
    return query ? `${href}?${query}` : href;
  }, [href, params, extra]);
}

/** Reads and writes one screen-specific query parameter (a selected key, a tab). */
/**
 * A screen's tab, as `tab=` like every screen; `view=`, which some screens used, is still read so
 * old links open the same tab, and is dropped at the next change.
 */
export function useTabParam(): [string | null, (value: string | null) => void] {
  const params = useSearchParams();
  const set = useSetQueryParams();
  const value = params.get('tab') ?? params.get('view');
  return [value, (next) => set({ tab: next, view: null })];
}

/** Sets several query parameters in one navigation (`null` removes one): two in a row would race. */
export function useSetQueryParams(): (patch: Record<string, string | null>) => void {
  const router = useRouter();
  const pathname = usePathname();
  return (patch) => {
    const search = new URLSearchParams(window.location.search);
    for (const [name, value] of Object.entries(patch)) {
      if (value === null || value === '') search.delete(name);
      else search.set(name, value);
    }
    const query = search.toString();
    router.replace(query ? `${pathname}?${query}` : pathname, { scroll: false });
  };
}

export function useQueryParam(name: string): [string | null, (value: string | null) => void] {
  const params = useSearchParams();
  const router = useRouter();
  const pathname = usePathname();
  const value = params.get(name);
  const setValue = (next: string | null) => {
    const search = new URLSearchParams(window.location.search);
    if (next === null || next === '') search.delete(name);
    else search.set(name, next);
    const query = search.toString();
    router.replace(query ? `${pathname}?${query}` : pathname, { scroll: false });
  };
  return [value, setValue];
}

/** The current page with the Settings dialog open at `section` (a link to a setting). */
export function useSettingsHref(section = 'general'): string {
  const params = useSearchParams();
  const pathname = usePathname();
  return useMemo(() => {
    const next = new URLSearchParams(params.toString());
    next.set(SETTINGS_PARAM, section);
    return `${pathname}?${next.toString()}`;
  }, [params, pathname, section]);
}

/** Opens the Settings dialog over the current page, at a section. */
export function useOpenSettings(): (section?: string) => void {
  const router = useRouter();
  const pathname = usePathname();
  return useCallback(
    (section = 'general') => {
      const search = new URLSearchParams(window.location.search);
      search.set(SETTINGS_PARAM, section);
      router.replace(`${pathname}?${search.toString()}`, { scroll: false });
    },
    [router, pathname],
  );
}
