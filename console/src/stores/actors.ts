import { useEffect } from 'react';
import { create } from 'zustand';

import { api } from '@/lib/api/client';

import { hasGatewayPermission, useSession } from './session';

/**
 * The tokens an audit entry or a key may name by their id (a kid), as words: whose token it is.
 * Read once per session, the first time a screen shows an actor: the reader's own tokens, and every
 * user's when the reader may list people.
 */
interface ActorNamesState {
  /** kid → "ada's token". */
  names: Record<string, string>;
  status: 'idle' | 'loading' | 'done';
  load: () => Promise<void>;
}

function tokenWords(owner: string, description: string | null | undefined): string {
  return description ? `${owner}'s token “${description}”` : `${owner}'s token`;
}

export const useActorNames = create<ActorNamesState>()((set, get) => ({
  names: {},
  status: 'idle',
  async load() {
    if (get().status !== 'idle') return;
    set({ status: 'loading' });
    const me = useSession.getState().me;
    const names: Record<string, string> = {};
    const own = me?.principal.type === 'user' ? me.principal.id : null;
    const reads: Promise<void>[] = [];
    if (own)
      reads.push(
        api.get('/auth/me/tokens').then(
          (page) => {
            for (const t of page.items) names[t.kid] = tokenWords(own, t.description);
          },
          () => undefined,
        ),
      );
    if (hasGatewayPermission(me, 'users:read'))
      reads.push(
        api.get('/users', { query: { limit: 200 } }).then(
          (users) =>
            Promise.all(
              users.items.map((u) =>
                api.get('/users/{name}/tokens', { path: { name: u.name } }).then(
                  (page) => {
                    for (const t of page.items) names[t.kid] = tokenWords(u.name, t.description);
                  },
                  () => undefined,
                ),
              ),
            ).then(() => undefined),
          () => undefined,
        ),
      );
    await Promise.all(reads);
    set({ names, status: 'done' });
  },
}));

/** The token names for actor labels, read the first time a screen asks for them. */
export function useTokenNames(): Record<string, string> {
  const names = useActorNames((s) => s.names);
  const load = useActorNames((s) => s.load);
  useEffect(() => {
    void load();
  }, [load]);
  return names;
}
