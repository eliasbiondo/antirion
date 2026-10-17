import { create } from 'zustand';

import type { Endpoint } from './body';

/** A request handed to the playground by another screen (a logged body, perhaps edited). */
export interface PlaygroundDraft {
  endpoint: Endpoint;
  body: Record<string, unknown>;
  project: string | null;
  /** The record the body came from. */
  source: string | null;
}

interface DraftState {
  draft: PlaygroundDraft | null;
  set: (draft: PlaygroundDraft | null) => void;
  /** The draft, once: the playground takes it when it opens. */
  take: () => PlaygroundDraft | null;
}

/** In memory only: a body never goes into the URL or browser storage. */
export const usePlaygroundDraft = create<DraftState>()((set, get) => ({
  draft: null,
  set: (draft) => set({ draft }),
  take: () => {
    const d = get().draft;
    set({ draft: null });
    return d;
  },
}));
