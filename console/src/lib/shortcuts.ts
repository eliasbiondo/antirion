'use client';

import { useEffect, useEffectEvent } from 'react';

type Handler = () => void;

const handlers = new Map<string, Handler>();

/**
 * Registers a single-key shortcut for the current screen (`f` filters, `e` export, `n` new).
 * The shell's key listener calls it unless focus is in a text field.
 */
export function usePageShortcut(key: string, handler: Handler, enabled = true): void {
  const run = useEffectEvent(handler);
  useEffect(() => {
    if (!enabled) return;
    const h = () => run();
    handlers.set(key, h);
    return () => {
      if (handlers.get(key) === h) handlers.delete(key);
    };
  }, [key, enabled]);
}

export function runPageShortcut(key: string): boolean {
  const handler = handlers.get(key);
  if (!handler) return false;
  handler();
  return true;
}

export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target.isContentEditable;
}

/** Focuses the current screen's search box (`/`). */
export function focusPageSearch(): boolean {
  const el = document.querySelector<HTMLInputElement>('[data-page-search]');
  if (!el) return false;
  el.focus();
  el.select();
  return true;
}

export const SHORTCUTS: { keys: string[]; description: string }[] = [
  { keys: ['?'], description: 'Show keyboard shortcuts' },
  { keys: ['⌘', 'K'], description: 'Search keys, models, people, requests and settings' },
  { keys: ['/'], description: "Focus the page's search" },
  { keys: ['↑', '↓'], description: 'Move in lists' },
  { keys: ['Enter'], description: 'Open the selected item' },
  { keys: ['Esc'], description: 'Close drawers and dialogs' },
  { keys: ['f'], description: 'Show or hide filters' },
  { keys: ['e'], description: 'Export the table as CSV' },
  { keys: ['n'], description: 'Create (a key, a model, a team…) on pages that offer it' },
  {
    keys: ['g', 'then', 'k'],
    description: 'Go to API keys (o overview, l log, m models, t teams, b budgets, c cache, s settings)',
  },
];
