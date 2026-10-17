'use client';

import { useEffect } from 'react';

import { resolveTheme, usePreferences } from '@/stores/preferences';

/** Applies theme, density, sidebar, motion and accent to the document whenever they change. */
export function PreferencesApplier() {
  const theme = usePreferences((s) => s.theme);
  const density = usePreferences((s) => s.density);
  const sidebarCollapsed = usePreferences((s) => s.sidebarCollapsed);
  const reducedMotion = usePreferences((s) => s.reducedMotion);
  const accentHue = usePreferences((s) => s.accentHue);
  const locale = usePreferences((s) => s.locale);

  useEffect(() => {
    const root = document.documentElement;
    const apply = () => root.setAttribute('data-theme', resolveTheme(theme));
    apply();
    if (theme !== 'system') return;
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    media.addEventListener('change', apply);
    return () => media.removeEventListener('change', apply);
  }, [theme]);

  useEffect(() => {
    const root = document.documentElement;
    root.setAttribute('data-density', density);
    root.setAttribute('data-sidebar', sidebarCollapsed ? 'collapsed' : 'expanded');
    if (reducedMotion) root.setAttribute('data-motion', 'reduced');
    else root.removeAttribute('data-motion');
    root.style.setProperty('--ant-accent-h', String(accentHue));
  }, [density, sidebarCollapsed, reducedMotion, accentHue]);

  useEffect(() => {
    // English is the only translation so far, so a language without one keeps the page in English.
    document.documentElement.lang = locale?.toLowerCase().startsWith('en') ? locale : 'en';
  }, [locale]);

  return null;
}
