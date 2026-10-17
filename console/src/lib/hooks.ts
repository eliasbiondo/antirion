'use client';

import { useEffect, useEffectEvent, useState, useSyncExternalStore } from 'react';

import { formatCostPerTokens, formatTokenPrice, type TokenUnit } from '@/lib/format';
import { usePreferences } from '@/stores/preferences';

/** The current time, refreshed every `everyMs`, for "x ago" labels. */
export function useNow(everyMs = 5000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), everyMs);
    return () => clearInterval(t);
  }, [everyMs]);
  return now;
}

/**
 * Runs `tick` every poll interval of the user's preferences (at most every 5 s), and not while
 * the tab is hidden; a tab that becomes visible again refreshes at once.
 */
export function usePolling(tick: () => void, enabled = true): void {
  const seconds = usePreferences((s) => s.pollSeconds);
  const run = useEffectEvent(tick);
  useEffect(() => {
    if (!enabled || seconds <= 0) return;
    const ms = Math.max(5, seconds) * 1000;
    let timer: ReturnType<typeof setInterval> | null = null;
    const start = () => {
      timer ??= setInterval(() => run(), ms);
    };
    const stop = () => {
      if (timer !== null) clearInterval(timer);
      timer = null;
    };
    const onVisibility = () => {
      if (document.hidden) stop();
      else {
        run();
        start();
      }
    };
    if (!document.hidden) start();
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      stop();
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [enabled, seconds]);
}

/** Whether a media query matches, kept up to date (false where there is no window). */
export function useMediaQuery(query: string): boolean {
  return useSyncExternalStore(
    (onChange) => {
      if (typeof window.matchMedia !== 'function') return () => undefined;
      const mq = window.matchMedia(query);
      mq.addEventListener('change', onChange);
      return () => mq.removeEventListener('change', onChange);
    },
    () => typeof window.matchMedia === 'function' && window.matchMedia(query).matches,
    () => false,
  );
}

export interface TokenPrice {
  unit: TokenUnit;
  /** "/ 1M" or "/ 1K", for a column header. */
  label: string;
  /** A price the API states per 1M tokens, in the user's unit with it. */
  format: (perMillion: string | number | null | undefined) => string;
  /** A cost per token in the user's unit, without it. */
  perTokens: (costPerToken: number) => string;
}

/** Per-token prices in the user's unit (per 1M or per 1K tokens). */
export function useTokenPrice(): TokenPrice {
  const unit = usePreferences((s) => s.priceUnit);
  return {
    unit,
    label: `/ ${unit}`,
    format: (v) => formatTokenPrice(v ?? null, unit),
    perTokens: (c) => formatCostPerTokens(c, unit),
  };
}

/**
 * Whether a form's values differ from what they were when it opened (`key` changes when another
 * item opens or the form reopens), so its drawer or dialog can ask before discarding them.
 */
export function useEdited(value: unknown, key: unknown = null): boolean {
  const json = JSON.stringify(value);
  const [base, setBase] = useState({ key, json });
  if (base.key !== key) {
    setBase({ key, json });
    return false;
  }
  return json !== base.json;
}
