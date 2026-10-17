'use client';

import { usePathname, useSearchParams } from 'next/navigation';
import { useEffect } from 'react';

import { api, ApiError } from '@/lib/api/client';
import { type RecentKind, useRecents } from '@/stores/recents';
import { logsApi } from '@/features/request-log/api';

/** The query parameter that selects an item on each screen, and the kind of item it is. */
const SELECTIONS: { path: string; param: string; kind: RecentKind }[] = [
  { path: '/request-log/', param: 'request', kind: 'request' },
  { path: '/', param: 'request', kind: 'request' },
  { path: '/keys/', param: 'key', kind: 'key' },
  { path: '/models/', param: 'model', kind: 'model' },
  { path: '/teams/', param: 'user', kind: 'user' },
  { path: '/teams/', param: 'team', kind: 'team' },
  { path: '/routing/', param: 'route', kind: 'route' },
  { path: '/alerts/', param: 'alert', kind: 'alert' },
  { path: '/change-requests/', param: 'request', kind: 'change' },
];

const clock = (iso: string) =>
  new Date(iso).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });

/**
 * What a recent item reads as: its title, or the name itself (`undefined`); `null` when it does not
 * exist, so a stale or mistyped link is never remembered.
 */
export async function recentLabel(kind: RecentKind, name: string): Promise<string | null | undefined> {
  try {
    switch (kind) {
      case 'request': {
        const r = await logsApi.get(name);
        return `${r.model ?? r.requested_model ?? 'Request'} · ${r.status_code} · ${clock(r.started_at)}`;
      }
      case 'alert':
        return (await api.get('/alerts/{alert_id}', { path: { alert_id: name } })).summary || undefined;
      case 'key':
        await api.get('/keys/{name}', { path: { name } });
        return undefined;
      case 'model':
        await api.get('/models/{name}', { path: { name } });
        return undefined;
      case 'user':
        await api.get('/users/{name}', { path: { name } });
        return undefined;
      case 'team':
        await api.get('/teams/{name}', { path: { name } });
        return undefined;
      case 'route':
        await api.get('/routes/{name}', { path: { name } });
        return undefined;
      case 'change':
        return (
          (await api.get('/change_requests/{change_request_id}', { path: { change_request_id: name } }))
            .title || undefined
        );
      default:
        return undefined;
    }
  } catch (error) {
    return error instanceof ApiError && error.status === 404 ? null : undefined;
  }
}

/** Remembers the items the user opens (by any link that selects one) for the palette's Recent list. */
export function RecentTracker() {
  const pathname = usePathname();
  const params = useSearchParams();
  const visit = useRecents((s) => s.visit);
  useEffect(() => {
    let cancelled = false;
    const path = pathname.endsWith('/') ? pathname : `${pathname}/`;
    for (const s of SELECTIONS) {
      const value = params.get(s.param);
      if (path !== s.path || !value) continue;
      void recentLabel(s.kind, value).then((label) => {
        if (cancelled || label === null) return;
        visit({ kind: s.kind, name: value, ...(label ? { label } : {}) });
      });
    }
    return () => {
      cancelled = true;
    };
  }, [pathname, params, visit]);
  return null;
}
