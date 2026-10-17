import { ApiError } from '@/lib/api/client';

import type { LoadStatus } from './collection-store';

/** One read's state inside a feature store: its data, load status, error and time. */
export interface Slot<T> {
  data: T | null;
  status: LoadStatus;
  error: ApiError | null;
  at: number | null;
}

export function emptySlot<T>(): Slot<T> {
  return { data: null, status: 'idle', error: null, at: null };
}

type SlotKeys<S> = { [K in keyof S]: S[K] extends Slot<unknown> ? K : never }[keyof S];
type SlotData<X> = X extends Slot<infer T> ? T : never;

export function asApiError(error: unknown): ApiError {
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
 * Runs `fn` and stores its result in the slot `key` of a Zustand store. A quiet load (a poll)
 * keeps the previous data on screen while it runs.
 */
export async function loadSlot<S, K extends SlotKeys<S>>(
  set: (partial: Partial<S>) => void,
  get: () => S,
  key: K,
  fn: () => Promise<SlotData<S[K]>>,
  options: { quiet?: boolean } = {},
): Promise<void> {
  const previous = get()[key] as Slot<SlotData<S[K]>>;
  if (!options.quiet || previous.data === null) {
    set({ [key]: { ...previous, status: 'loading', error: null } } as Partial<S>);
  }
  try {
    const data = await fn();
    set({ [key]: { data, status: 'ready', error: null, at: Date.now() } } as Partial<S>);
  } catch (error) {
    const current = get()[key] as Slot<SlotData<S[K]>>;
    set({ [key]: { ...current, status: 'error', error: asApiError(error) } } as Partial<S>);
  }
}
