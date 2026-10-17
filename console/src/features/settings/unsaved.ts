'use client';

import { useEffect, useId } from 'react';
import { create } from 'zustand';

interface UnsavedState {
  /** The forms with edits not saved yet, by an id per form. */
  dirty: Record<string, true>;
  mark: (id: string, dirty: boolean) => void;
}

/** Which Settings forms hold unsaved edits, so the dialog can warn before they are lost. */
export const useUnsaved = create<UnsavedState>()((set) => ({
  dirty: {},
  mark(id, dirty) {
    set((s) => {
      if (dirty === id in s.dirty) return s;
      const next: Record<string, true> = Object.fromEntries(
        Object.keys(s.dirty)
          .filter((k) => k !== id)
          .map((k) => [k, true as const]),
      );
      if (dirty) next[id] = true;
      return { dirty: next };
    });
  },
}));

/** Reports a form's unsaved edits while it is on screen. */
export function useReportUnsaved(dirty: boolean): void {
  const id = useId();
  useEffect(() => {
    useUnsaved.getState().mark(id, dirty);
    return () => useUnsaved.getState().mark(id, false);
  }, [id, dirty]);
}

export function hasUnsaved(): boolean {
  return Object.keys(useUnsaved.getState().dirty).length > 0;
}
