import { useLocale } from '@/lib/locale';

import { en, type Messages } from './en';
import { fa } from './fa';

export function useMessages(): Messages {
  return useLocale().tag === 'fa' ? fa : en;
}
