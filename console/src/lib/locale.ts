import { useSyncExternalStore } from 'react';

import { usePreferences } from '@/stores/preferences';

export const LOCALES = [
  { tag: 'en', name: 'English', direction: 'ltr' },
  { tag: 'fa', name: 'فارسی', direction: 'rtl' },
] as const;

export type Locale = (typeof LOCALES)[number];

export function matchLocale(tag: string | null): Locale | undefined {
  const language = tag?.toLowerCase().split('-')[0];
  return LOCALES.find((option) => option.tag === language);
}

/** A regional tag such as fa-IR selects the matching base language. */
export function resolveLocale(preference: string | null, browser: readonly string[] = []): Locale {
  const preferred = matchLocale(preference);
  if (preferred) return preferred;
  for (const tag of browser) {
    const locale = matchLocale(tag);
    if (locale) return locale;
  }
  return LOCALES[0];
}

function subscribe(listener: () => void): () => void {
  window.addEventListener('languagechange', listener);
  return () => window.removeEventListener('languagechange', listener);
}

/** The browser's current selection for messages created outside a React render. */
export function currentLocale(): Locale {
  const browser = typeof window === 'undefined' ? [] : navigator.languages;
  return resolveLocale(usePreferences.getState().locale, browser);
}

export function useLocale(): Locale {
  const preference = usePreferences((s) => s.locale);
  const browser = useSyncExternalStore(
    subscribe,
    () => navigator.languages.join(','),
    () => '',
  );
  return resolveLocale(preference, browser.split(','));
}
