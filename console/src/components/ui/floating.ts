'use client';

import { useCallback, useLayoutEffect, useState, type RefObject } from 'react';

export interface FloatingOptions {
  /** The preferred side; it flips to the other one when that side has more room. */
  side?: 'bottom' | 'top';
  /** Which edges line up: the anchor's start (left), end (right) or centre. */
  align?: 'start' | 'end' | 'center';
  /** The gap between the anchor and the floating element, in px. */
  offset?: number;
  /** Makes the floating element at least as wide as its anchor (a listbox under its trigger). */
  matchWidth?: boolean;
}

export interface Placement {
  top: number;
  left: number;
  side: 'bottom' | 'top';
  /** The room left on the chosen side, for a scrolling list's max height. */
  maxHeight: number;
}

/** Keeps floating elements this far from the viewport's edges. */
const EDGE = 8;

interface Box {
  top: number;
  bottom: number;
  left: number;
  right: number;
  width: number;
}

/**
 * Where a floating element goes next to its anchor: on the preferred side when it fits, else on
 * the side with more room, and shifted horizontally to stay inside the viewport.
 */
export function placeFloating(
  anchor: Box,
  size: { width: number; height: number },
  viewport: { width: number; height: number },
  { side = 'bottom', align = 'start', offset = 6 }: FloatingOptions = {},
): Placement {
  const below = viewport.height - anchor.bottom - offset - EDGE;
  const above = anchor.top - offset - EDGE;
  let chosen = side;
  if (side === 'bottom' && size.height > below && above > below) chosen = 'top';
  if (side === 'top' && size.height > above && below > above) chosen = 'bottom';
  const room = Math.max(0, chosen === 'bottom' ? below : above);
  const height = Math.min(size.height, room);
  const top = chosen === 'bottom' ? anchor.bottom + offset : anchor.top - offset - height;
  let left =
    align === 'end'
      ? anchor.right - size.width
      : align === 'center'
        ? anchor.left + anchor.width / 2 - size.width / 2
        : anchor.left;
  left = Math.min(left, viewport.width - EDGE - size.width);
  left = Math.max(EDGE, left);
  return { top, left, side: chosen, maxHeight: room };
}

/** Measures and moves `f` next to `a` (set through the CSSOM: no inline style in the export). */
function position(a: HTMLElement, f: HTMLElement, options: FloatingOptions & { matchWidth: boolean }): void {
  const rect = a.getBoundingClientRect();
  const s = f.style;
  if (options.matchWidth) s.setProperty('min-width', `${Math.round(rect.width)}px`);
  s.removeProperty('--floating-max-height');
  const box = f.getBoundingClientRect();
  const p = placeFloating(
    rect,
    { width: box.width, height: box.height },
    { width: window.innerWidth, height: window.innerHeight },
    options,
  );
  s.setProperty('top', `${Math.round(p.top)}px`);
  s.setProperty('left', `${Math.round(p.left)}px`);
  s.setProperty('--floating-max-height', `${Math.floor(p.maxHeight)}px`);
  f.dataset.side = p.side;
}

/**
 * Positions `floating` next to `anchor` while `open` (fixed, so scrolling containers and dialogs
 * don't clip it), and again on scroll and resize. The element's scroll area reads
 * `--floating-max-height`.
 */
export function useFloating(
  anchor: RefObject<HTMLElement | null>,
  floating: RefObject<HTMLElement | null>,
  open: boolean,
  { side = 'bottom', align = 'start', offset = 6, matchWidth = false }: FloatingOptions = {},
): void {
  useLayoutEffect(() => {
    if (!open) return;
    let frame = 0;
    const update = () => {
      if (anchor.current && floating.current)
        position(anchor.current, floating.current, { side, align, offset, matchWidth });
    };
    update();
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(update);
    };
    window.addEventListener('resize', schedule);
    window.addEventListener('scroll', schedule, true);
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(schedule);
    if (anchor.current) observer?.observe(anchor.current);
    if (floating.current) observer?.observe(floating.current);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener('resize', schedule);
      window.removeEventListener('scroll', schedule, true);
      observer?.disconnect();
    };
  }, [open, anchor, floating, side, align, offset, matchWidth]);
}

/**
 * Where an overlay mounts: inside the modal dialog that holds it (assistive technology treats
 * everything outside a modal as inert), or the body. Render the returned ref on a hidden marker
 * next to the trigger.
 */
export function usePortalTarget(): [HTMLElement | null, (node: HTMLElement | null) => void] {
  const [target, setTarget] = useState<HTMLElement | null>(null);
  const marker = useCallback((node: HTMLElement | null) => {
    if (node) setTarget(node.closest<HTMLElement>('[aria-modal="true"]') ?? node.ownerDocument.body);
  }, []);
  return [target, marker];
}
