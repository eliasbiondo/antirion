'use client';

import { createContext, useContext } from 'react';
import { createPortal } from 'react-dom';

import { usePreferences } from '@/stores/preferences';

import { Kbd } from './misc';
import { useTranslate } from '@/lib/i18n';

export type HintKey = 'search' | 'move' | 'open' | 'close' | 'filters' | 'export' | 'new';

const HINTS: Record<HintKey, { keys: string[]; label: string }> = {
  search: { keys: ['/'], label: 'search' },
  move: { keys: ['↑', '↓'], label: 'move' },
  open: { keys: ['⏎'], label: 'open' },
  close: { keys: ['esc'], label: 'close' },
  filters: { keys: ['f'], label: 'filters' },
  export: { keys: ['e'], label: 'export' },
  new: { keys: ['n'], label: 'new' },
};

/**
 * Where the shell wants the strip: a slot after the page, at the bottom of the main area. A screen
 * can render the strip anywhere in its tree; it lands there, so it sits at the bottom of the view
 * however short the list is. Without a slot (a screen rendered alone) it stays where it is.
 */
export const KeyboardHintsSlot = createContext<HTMLElement | null>(null);

/**
 * The strip under a list screen naming the keys that work there, pinned to the bottom of the view
 * so it reads the same wherever the page is scrolled. Hidden under 768px and when the user turned
 * keyboard hints off in their preferences.
 */
export function KeyboardHints({ keys = ['search', 'move', 'open', 'close'] }: { keys?: HintKey[] }) {
  const translate = useTranslate();
  const on = usePreferences((s) => s.keyboardHints);
  const slot = useContext(KeyboardHintsSlot);
  if (!on) return null;
  const strip = (
    <div
      aria-label={translate('Keyboard shortcuts on this page')}
      role="note"
      data-keyboard-hints=""
      className="sticky bottom-0 z-10 flex h-10 items-center gap-x-4 overflow-hidden border-t border-line bg-canvas px-gutter text-caption whitespace-nowrap text-dim max-md:hidden"
    >
      {keys.map((k) => (
        <span key={k} className="flex items-center gap-1.5">
          {HINTS[k].keys.map((key) => (
            <Kbd key={key}>{key}</Kbd>
          ))}
          {HINTS[k].label}
        </span>
      ))}
      <span className="ml-auto flex items-center gap-1.5">
        <Kbd>?</Kbd>
        {' ' + translate('all shortcuts')}
      </span>
    </div>
  );
  return slot ? createPortal(strip, slot) : strip;
}
