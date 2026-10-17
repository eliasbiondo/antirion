import { create } from 'zustand';
import { createJSONStorage, persist } from 'zustand/middleware';

export type RecentKind =
  | 'request'
  | 'key'
  | 'model'
  | 'user'
  | 'team'
  | 'route'
  | 'alert'
  | 'change'
  | 'mcp_server'
  | 'agent'
  | 'plugin';

export interface RecentItem {
  kind: RecentKind;
  /** The id or name the screen selects it by. */
  name: string;
  /** What to show when it differs from the name (an alert's title). */
  label?: string | undefined;
}

export const MAX_RECENT_ITEMS = 8;
export const MAX_RECENT_SEARCHES = 5;

interface RecentsState {
  items: RecentItem[];
  searches: string[];
  visit: (item: RecentItem) => void;
  searched: (query: string) => void;
  clear: () => void;
}

/**
 * What the user opened and searched for lately, for the command palette. Kept in this browser only
 * (names and ids, never content), newest first.
 */
export const useRecents = create<RecentsState>()(
  persist(
    (set, get) => ({
      items: [],
      searches: [],
      visit(item) {
        const rest = get().items.filter((i) => !(i.kind === item.kind && i.name === item.name));
        const previous = get().items.find((i) => i.kind === item.kind && i.name === item.name);
        const label = item.label ?? previous?.label;
        set({ items: [{ ...item, ...(label ? { label } : {}) }, ...rest].slice(0, MAX_RECENT_ITEMS) });
      },
      searched(query) {
        const q = query.trim();
        if (!q) return;
        set({ searches: [q, ...get().searches.filter((s) => s !== q)].slice(0, MAX_RECENT_SEARCHES) });
      },
      clear() {
        set({ items: [], searches: [] });
      },
    }),
    { name: 'antirion.console.recent', storage: createJSONStorage(() => localStorage) },
  ),
);
