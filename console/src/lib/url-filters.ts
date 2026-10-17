'use client';

import { usePathname, useRouter, useSearchParams } from 'next/navigation';
import { useCallback, useMemo } from 'react';

/** A filter value a URL can carry: text, a list of values, or a switch. */
export type FilterValue = string | string[] | boolean;
export type Filters = Record<string, FilterValue>;

/** Reads `defaults`' keys from the query; a key absent or malformed keeps its default. */
export function decodeFilters<T extends Filters>(params: URLSearchParams, defaults: T): T {
  const out: Filters = { ...defaults };
  for (const [key, fallback] of Object.entries(defaults)) {
    if (!params.has(key)) continue;
    if (Array.isArray(fallback)) out[key] = params.getAll(key).filter((v) => v !== '');
    else if (typeof fallback === 'boolean') {
      const v = params.get(key);
      if (v === '1' || v === 'true') out[key] = true;
      else if (v === '0' || v === 'false') out[key] = false;
    } else out[key] = params.get(key) ?? fallback;
  }
  return out as T;
}

function same(a: FilterValue, b: FilterValue): boolean {
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((v, i) => v === b[i]);
  return a === b;
}

/** Writes the filters into `params` (a copy): only values that differ from their default. */
export function encodeFilters<T extends Filters>(
  params: URLSearchParams,
  filters: T,
  defaults: T,
): URLSearchParams {
  const next = new URLSearchParams(params.toString());
  for (const [key, fallback] of Object.entries(defaults)) {
    next.delete(key);
    const value = filters[key] ?? fallback;
    if (same(value, fallback)) continue;
    if (Array.isArray(value)) for (const v of value) next.append(key, v);
    else if (typeof value === 'boolean') next.set(key, value ? '1' : '0');
    else if (value !== '') next.set(key, value);
  }
  return next;
}

export interface UrlFilters<T extends Filters> {
  filters: T;
  /** Changes some filters; the others stay. */
  set: (patch: Partial<T>) => void;
  /**
   * Changes some filters and other query parameters (a tab, a selection; `null` removes one) in one
   * navigation: two navigations in a row would race, the second undoing the first.
   */
  setWith: (patch: Partial<T>, params: Record<string, string | null>) => void;
  /** Back to the defaults. */
  clear: () => void;
  /** Whether any filter differs from its default. */
  active: boolean;
}

/**
 * A list's filters kept in the URL, so a copied link reopens the same filtered view. `defaults`
 * names every filter and its type; it must be stable (a module constant).
 */
export function useUrlFilters<T extends Filters>(defaults: T): UrlFilters<T> {
  const params = useSearchParams();
  const router = useRouter();
  const pathname = usePathname();
  const filters = useMemo(
    () => decodeFilters(new URLSearchParams(params.toString()), defaults),
    [params, defaults],
  );
  const write = useCallback(
    (next: T, params: Record<string, string | null> = {}) => {
      const search = encodeFilters(new URLSearchParams(window.location.search), next, defaults);
      for (const [k, v] of Object.entries(params)) {
        if (v === null || v === '') search.delete(k);
        else search.set(k, v);
      }
      const query = search.toString();
      router.replace(query ? `${pathname}?${query}` : pathname, { scroll: false });
    },
    [router, pathname, defaults],
  );
  const active = Object.entries(defaults).some(([k, d]) => !same(filters[k] ?? d, d));
  return {
    filters,
    set: (patch) => write({ ...filters, ...patch }),
    setWith: (patch, params) => write({ ...filters, ...patch }, params),
    clear: () => write(defaults),
    active,
  };
}
