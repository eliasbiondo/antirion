import { create } from 'zustand';

import type { AttemptedChange } from '@/lib/api/client';

export type ToastTone = 'info' | 'success' | 'warn' | 'error';

export interface ToastAction {
  /** A verb: "Undo", "View key". */
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: number;
  tone: ToastTone;
  title: string;
  detail?: string | undefined;
  /** One follow-up, such as Undo for a reversible change. */
  action?: ToastAction | undefined;
}

interface UiState {
  toasts: Toast[];
  paletteOpen: boolean;
  shortcutsOpen: boolean;
  /** The sidebar as a sheet over the page, under 1024px wide. */
  navOpen: boolean;
  /** A write change control refused, waiting to be sent as a change request. */
  changeDraft: AttemptedChange | null;
  notify: (tone: ToastTone, title: string, detail?: string, action?: ToastAction) => void;
  dismiss: (id: number) => void;
  setPaletteOpen: (open: boolean) => void;
  setShortcutsOpen: (open: boolean) => void;
  setNavOpen: (open: boolean) => void;
  setChangeDraft: (draft: AttemptedChange | null) => void;
}

let nextId = 1;

/** Transient interface state: notifications, the command palette and the shortcuts sheet. */
export const useUi = create<UiState>()((set, get) => ({
  toasts: [],
  paletteOpen: false,
  shortcutsOpen: false,
  navOpen: false,
  changeDraft: null,
  notify(tone, title, detail, action) {
    const id = nextId++;
    set((s) => ({ toasts: [...s.toasts.slice(-3), { id, tone, title, detail, action }] }));
    setTimeout(() => get().dismiss(id), tone === 'error' || action ? 8000 : 4500);
  },
  dismiss(id) {
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
  },
  setPaletteOpen(open) {
    set({ paletteOpen: open });
  },
  setShortcutsOpen(open) {
    set({ shortcutsOpen: open });
  },
  setNavOpen(open) {
    set({ navOpen: open });
  },
  setChangeDraft(draft) {
    set({ changeDraft: draft });
  },
}));

export const notify = (tone: ToastTone, title: string, detail?: string, action?: ToastAction) =>
  useUi.getState().notify(tone, title, detail, action);
