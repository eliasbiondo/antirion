'use client';

import { useCallback, useEffect, useRef } from 'react';

/**
 * The open overlays (dialogs, drawers, popovers, listboxes), oldest first. Only the newest one
 * answers Escape, Tab and outside clicks, so a listbox open in a dialog closes before the dialog.
 */
const stack: symbol[] = [];

/** Registers an overlay while `open`; `isTop()` says whether it is the newest one. */
export function useLayer(open: boolean): () => boolean {
  const token = useRef<symbol | null>(null);
  useEffect(() => {
    if (!open) return;
    const t = Symbol('layer');
    token.current = t;
    stack.push(t);
    return () => {
      const i = stack.indexOf(t);
      if (i >= 0) stack.splice(i, 1);
      token.current = null;
    };
  }, [open]);
  return useCallback(() => token.current !== null && stack[stack.length - 1] === token.current, []);
}

/** How many overlays are open (tests and the global shortcuts read it). */
export function openLayers(): number {
  return stack.length;
}
