import { currentLocale, useLocale, type Locale } from '@/lib/locale';

import { fa } from './fa';

type MessageValue = string | number;

function format(message: string, values: readonly MessageValue[]): string {
  if (values.length === 0) return message;
  return message.replace(/\{(\d+)\}/g, (placeholder, index: string) => {
    const value = values[Number(index)];
    return value === undefined ? placeholder : String(value);
  });
}

function persian(message: string, ...values: MessageValue[]): string {
  const key = message.trim();
  const translated = Object.hasOwn(fa, key) ? fa[key] : undefined;
  return format(translated === undefined ? message : message.replace(key, () => translated), values);
}

function english(message: string, ...values: MessageValue[]): string {
  return format(message, values);
}

/** Keep English source text as the fallback and pass identifiers as separate values. */
export function translator(locale: Locale['tag']): typeof english {
  return locale === 'fa' ? persian : english;
}

export function useTranslate(): typeof english {
  return translator(useLocale().tag);
}

/** For interpolated notifications created in callbacks and stores. */
export function translateNow(message: string, ...values: MessageValue[]): string {
  return translator(currentLocale().tag)(message, ...values);
}
