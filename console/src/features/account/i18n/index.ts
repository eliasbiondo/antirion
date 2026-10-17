import { usePreferences } from '@/stores/preferences';

import { en, type Messages } from './en';

export interface Locale {
  /** A BCP 47 tag, as stored in the `locale` preference. */
  tag: string;
  /** The language's name in itself, as the picker shows it. */
  name: string;
  messages: Messages;
}

/** The languages the console is translated into; English is the reference. */
export const LOCALES: readonly Locale[] = [{ tag: 'en', name: 'English', messages: en }];

const FALLBACK: Locale = { tag: 'en', name: 'English', messages: en };

/** The locale to use: the preference when translated, else the browser's first translated language. */
export function resolveLocale(preference: string | null, browser: readonly string[] = []): Locale {
  const wanted = [...(preference ? [preference] : []), ...browser];
  for (const tag of wanted) {
    const lower = tag.toLowerCase();
    const found =
      LOCALES.find((l) => l.tag.toLowerCase() === lower) ??
      LOCALES.find((l) => l.tag.toLowerCase() === lower.split('-')[0]);
    if (found) return found;
  }
  return FALLBACK;
}

function browserLanguages(): readonly string[] {
  return typeof navigator === 'undefined' ? [] : navigator.languages;
}

/** The strings of the user's language. */
export function useMessages(): Messages {
  const preference = usePreferences((s) => s.locale);
  return resolveLocale(preference, browserLanguages()).messages;
}
