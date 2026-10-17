'use client';

import { useRouter, useSearchParams } from 'next/navigation';
import { useEffect } from 'react';

import { SETTINGS_PARAM } from '@/lib/nav';

import { MOVED_SECTIONS } from './sections';

/**
 * The old Settings page: `/settings/?section=<id>` opens the Settings dialog at that section over
 * the Overview (old section ids included), and a section that became a page opens the page.
 */
export function SettingsRedirect() {
  const router = useRouter();
  const params = useSearchParams();
  useEffect(() => {
    const next = new URLSearchParams(params.toString());
    const section = next.get('section') ?? next.get(SETTINGS_PARAM) ?? 'general';
    next.delete('section');
    next.delete(SETTINGS_PARAM);
    const moved = MOVED_SECTIONS[section];
    if (moved) {
      const query = next.toString();
      router.replace(query ? `${moved}?${query}` : moved);
      return;
    }
    next.set(SETTINGS_PARAM, section);
    router.replace(`/?${next.toString()}`);
  }, [params, router]);
  return null;
}
