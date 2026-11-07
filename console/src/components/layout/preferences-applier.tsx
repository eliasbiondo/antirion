'use client';

import { useEffect } from 'react';

import { useLocale } from '@/lib/locale';
import { resolveTheme, usePreferences } from '@/stores/preferences';

/** Applies theme, density, sidebar, motion and accent to the document whenever they change. */
export function PreferencesApplier() {
  const theme = usePreferences((s) => s.theme);
  const density = usePreferences((s) => s.density);
  const sidebarCollapsed = usePreferences((s) => s.sidebarCollapsed);
  const reducedMotion = usePreferences((s) => s.reducedMotion);
  const accentHue = usePreferences((s) => s.accentHue);
  const { tag, direction } = useLocale();

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
    document.documentElement.lang = tag;
    document.documentElement.dir = direction;
  }, [tag, direction]);

  return null;
}
