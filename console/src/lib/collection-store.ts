import { create, type StoreApi, type UseBoundStore } from 'zustand';

import { ApiError } from '@/lib/api/client';

export type LoadStatus = 'idle' | 'loading' | 'ready' | 'error';

export interface Named {
  name: string;
  metadata: { etag: string };
}

export interface CollectionApi<T extends Named, C, U> {
  list: () => Promise<T[]>;
  create: (body: C) => Promise<T>;
  update: (name: string, body: U, etag: string | undefined) => Promise<T>;
  remove: (
    name: string,
    options: { reason?: string | undefined; etag?: string | undefined },
  ) => Promise<void>;
}

export interface CollectionState<T extends Named, C, U> {
  items: T[];
  status: LoadStatus;
  error: ApiError | null;
  loadedAt: number | null;
  fetch: (options?: { quiet?: boolean }) => Promise<void>;
  create: (body: C) => Promise<T>;
  update: (name: string, body: U) => Promise<T>;
  remove: (name: string, reason?: string) => Promise<void>;
  byName: (name: string) => T | undefined;
}

function toApiError(error: unknown): ApiError {
  if (error instanceof ApiError) return error;
  return new ApiError({
    status: 0,
    code: 'client_error',
    message: error instanceof Error ? error.message : 'Unexpected error',
    requestId: null,
    operation: 'client',
  });
}

/**
 * A store for one Admin API collection (`/keys`, `/models`, …): the list with its load state, and
 * create, update and delete that keep the list current and send the resource's ETag so a
 * concurrent edit fails instead of being overwritten.
 */
export function createCollectionStore<T extends Named, C, U>(
  client: CollectionApi<T, C, U>,
): UseBoundStore<StoreApi<CollectionState<T, C, U>>> {
  return create<CollectionState<T, C, U>>()((set, get) => ({
    items: [],
    status: 'idle',
    error: null,
    loadedAt: null,

    async fetch(options) {
      if (!options?.quiet) set({ status: 'loading', error: null });
      try {
        const items = await client.list();
        set({ items, status: 'ready', error: null, loadedAt: Date.now() });
      } catch (error) {
        set({ status: 'error', error: toApiError(error) });
      }
    },

    async create(body) {
      const created = await client.create(body);
      set((s) => ({ items: sortByName([...s.items.filter((i) => i.name !== created.name), created]) }));
      return created;
    },

    async update(name, body) {
      const current = get().byName(name);
      const updated = await client.update(name, body, current?.metadata.etag);
      set((s) => ({ items: s.items.map((i) => (i.name === name ? updated : i)) }));
      return updated;
    },

    async remove(name, reason) {
      const current = get().byName(name);
      await client.remove(name, { reason, etag: current?.metadata.etag });
      set((s) => ({ items: s.items.filter((i) => i.name !== name) }));
    },

    byName(name) {
      return get().items.find((i) => i.name === name);
    },
  }));
}

function sortByName<T extends Named>(items: T[]): T[] {
  return [...items].sort((a, b) => a.name.localeCompare(b.name));
}
