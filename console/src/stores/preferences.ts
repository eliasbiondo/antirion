import { create } from 'zustand';
import { createJSONStorage, persist } from 'zustand/middleware';

import { api } from '@/lib/api/client';
import { useSession } from '@/stores/session';
import type { Preferences as ServerPreferences } from '@/lib/api/types';

export type Theme = 'light' | 'dark' | 'system';
export type Density = 'compact' | 'comfortable';
export type PriceUnit = '1M' | '1K';

export const FAVOURITE_KINDS = [
  'page',
  'model',
  'key',
  'route',
  'team',
  'user',
  'mcp_server',
  'agent',
  'plugin',
] as const;
export type FavouriteKind = (typeof FAVOURITE_KINDS)[number];

/** A starred item: a resource by name, or a page by its path. */
export interface Favourite {
  kind: FavouriteKind;
  name: string;
}

/** At most this many favourites are kept, newest first. */
export const MAX_FAVOURITES = 50;

export interface PreferenceValues {
  theme: Theme;
  density: Density;
  sidebarCollapsed: boolean;
  accentHue: number;
  reducedMotion: boolean;
  compactNumbers: boolean;
  wideKpis: boolean;
  weekStart: 'monday' | 'sunday';
  timeZone: string;
  /** Seconds between refreshes of live views; 0 turns polling off. */
  pollSeconds: number;
  /** Per-token prices per 1M or per 1K tokens. */
  priceUnit: PriceUnit;
  /** Whether the request log's filter rail starts open. */
  filterRail: boolean;
  /** Whether list screens show the keyboard hints strip. */
  keyboardHints: boolean;
  favourites: Favourite[];
  /** Whether the sidebar's Favourites group is open. */
  favouritesOpen: boolean;
  /** The organization a link without one opens in (user accounts only). */
  defaultOrganization: string | null;
  /** The time range preset a link without one opens with. */
  defaultRange: string | null;
  /** The request log's columns in order; null for the standard set. */
  logColumns: string[] | null;
  /** Whether the API keys screen shows the unusual-activity notice over the list. */
  keyAnomalyNotice: boolean;
  /** The console's language (a BCP 47 tag); null follows the browser. */
  locale: string | null;
  /** The user's photo as a small image `data:` URL (user accounts only); null shows initials. */
  photo: string | null;
}

interface PreferencesState extends PreferenceValues {
  set: (patch: Partial<PreferenceValues>) => void;
  toggleTheme: () => void;
  toggleSidebar: () => void;
  /** Stars or unstars an item. */
  toggleFavourite: (item: Favourite) => void;
  /** Loads the user's stored preferences; the browser's last values stay if the call fails. */
  pull: () => Promise<void>;
}

export const DEFAULT_PREFERENCES: PreferenceValues = {
  theme: 'dark',
  density: 'compact',
  sidebarCollapsed: false,
  accentHue: 190,
  reducedMotion: false,
  compactNumbers: true,
  wideKpis: false,
  weekStart: 'monday',
  timeZone: 'UTC',
  pollSeconds: 5,
  priceUnit: '1M',
  filterRail: true,
  keyboardHints: true,
  favourites: [],
  favouritesOpen: true,
  defaultOrganization: null,
  defaultRange: null,
  logColumns: null,
  keyAnomalyNotice: true,
  locale: null,
  photo: null,
};

/** Storage key; `public/theme-init.js` reads the same key before the first paint. */
export const PREFERENCES_KEY = 'antirion.console.preferences';

/** Members of `ui` this console does not know, kept as read so a write never drops them. */
let otherUi: Record<string, unknown> = {};
const KNOWN_UI = [
  'wide_kpis',
  'poll_seconds',
  'price_unit',
  'filter_rail',
  'keyboard_hints',
  'favourites',
  'favourites_open',
  'log_columns',
  'key_anomaly_notice',
  'photo',
];

function toServer(values: PreferenceValues): ServerPreferences {
  return {
    theme: values.theme,
    accent: String(values.accentHue),
    density: values.density,
    sidebar_collapsed: values.sidebarCollapsed,
    reduced_motion: values.reducedMotion,
    compact_numbers: values.compactNumbers,
    week_start: values.weekStart,
    timezone: values.timeZone,
    ...(values.defaultOrganization ? { default_organization: values.defaultOrganization } : {}),
    ...(values.defaultRange ? { time_range: values.defaultRange } : {}),
    ...(values.locale ? { locale: values.locale } : {}),
    ui: {
      ...otherUi,
      wide_kpis: values.wideKpis,
      poll_seconds: values.pollSeconds,
      price_unit: values.priceUnit,
      filter_rail: values.filterRail,
      keyboard_hints: values.keyboardHints,
      favourites: values.favourites,
      favourites_open: values.favouritesOpen,
      ...(values.logColumns ? { log_columns: values.logColumns } : {}),
      key_anomaly_notice: values.keyAnomalyNotice,
      ...(values.photo ? { photo: values.photo } : {}),
    },
  };
}

function isFavourite(value: unknown): value is Favourite {
  if (typeof value !== 'object' || value === null) return false;
  const v = value as { kind?: unknown; name?: unknown };
  return (
    typeof v.name === 'string' && v.name !== '' && (FAVOURITE_KINDS as readonly unknown[]).includes(v.kind)
  );
}

function fromServer(p: ServerPreferences): Partial<PreferenceValues> {
  const out: Partial<PreferenceValues> = {};
  if (p.theme) out.theme = p.theme;
  if (p.density) out.density = p.density;
  if (typeof p.sidebar_collapsed === 'boolean') out.sidebarCollapsed = p.sidebar_collapsed;
  if (typeof p.reduced_motion === 'boolean') out.reducedMotion = p.reduced_motion;
  if (typeof p.compact_numbers === 'boolean') out.compactNumbers = p.compact_numbers;
  if (p.week_start) out.weekStart = p.week_start;
  if (p.timezone) out.timeZone = p.timezone;
  const hue = Number(p.accent);
  if (p.accent && Number.isFinite(hue)) out.accentHue = hue;
  out.defaultOrganization = p.default_organization ?? null;
  out.defaultRange = p.time_range ?? null;
  out.locale = p.locale ?? null;
  const ui: Record<string, unknown> = p.ui ?? {};
  otherUi = Object.fromEntries(Object.entries(ui).filter(([k]) => !KNOWN_UI.includes(k)));
  if (typeof ui.wide_kpis === 'boolean') out.wideKpis = ui.wide_kpis;
  if (typeof ui.poll_seconds === 'number') out.pollSeconds = ui.poll_seconds;
  if (ui.price_unit === '1M' || ui.price_unit === '1K') out.priceUnit = ui.price_unit;
  if (typeof ui.filter_rail === 'boolean') out.filterRail = ui.filter_rail;
  if (typeof ui.keyboard_hints === 'boolean') out.keyboardHints = ui.keyboard_hints;
  if (Array.isArray(ui.favourites))
    out.favourites = ui.favourites.filter(isFavourite).slice(0, MAX_FAVOURITES);
  if (typeof ui.favourites_open === 'boolean') out.favouritesOpen = ui.favourites_open;
  if (typeof ui.key_anomaly_notice === 'boolean') out.keyAnomalyNotice = ui.key_anomaly_notice;
  out.photo = typeof ui.photo === 'string' && ui.photo.startsWith('data:image/') ? ui.photo : null;
  out.logColumns = Array.isArray(ui.log_columns)
    ? ui.log_columns.filter((c): c is string => typeof c === 'string').slice(0, 32)
    : null;
  return out;
}

let pushTimer: ReturnType<typeof setTimeout> | null = null;
let serverSync = false;

function schedulePush(values: PreferenceValues): void {
  if (!serverSync) return;
  if (pushTimer) clearTimeout(pushTimer);
  pushTimer = setTimeout(() => {
    api.put('/auth/me/preferences', { body: toServer(values) }).catch(() => {
      serverSync = false;
    });
  }, 800);
}

function values(state: PreferencesState): PreferenceValues {
  return {
    theme: state.theme,
    density: state.density,
    sidebarCollapsed: state.sidebarCollapsed,
    accentHue: state.accentHue,
    reducedMotion: state.reducedMotion,
    compactNumbers: state.compactNumbers,
    wideKpis: state.wideKpis,
    weekStart: state.weekStart,
    timeZone: state.timeZone,
    pollSeconds: state.pollSeconds,
    priceUnit: state.priceUnit,
    filterRail: state.filterRail,
    keyboardHints: state.keyboardHints,
    favourites: state.favourites,
    favouritesOpen: state.favouritesOpen,
    defaultOrganization: state.defaultOrganization,
    defaultRange: state.defaultRange,
    logColumns: state.logColumns,
    keyAnomalyNotice: state.keyAnomalyNotice,
    locale: state.locale,
    photo: state.photo,
  };
}

/**
 * Display preferences. They follow the user through `/auth/me/preferences` and are cached in
 * `localStorage` (they hold no secret), so the last values apply before that call returns.
 */
export const usePreferences = create<PreferencesState>()(
  persist(
    (set, get) => ({
      ...DEFAULT_PREFERENCES,
      set(patch) {
        set(patch);
        schedulePush(values(get()));
      },
      toggleTheme() {
        const current = resolveTheme(get().theme);
        get().set({ theme: current === 'dark' ? 'light' : 'dark' });
      },
      toggleSidebar() {
        get().set({ sidebarCollapsed: !get().sidebarCollapsed });
      },
      toggleFavourite(item) {
        const list = get().favourites;
        const starred = list.some((f) => f.kind === item.kind && f.name === item.name);
        get().set({
          favourites: starred
            ? list.filter((f) => !(f.kind === item.kind && f.name === item.name))
            : [item, ...list].slice(0, MAX_FAVOURITES),
        });
      },
      async pull() {
        // Admin tokens have no account to keep preferences: the gateway would answer 403 and log it.
        if (useSession.getState().me?.principal.type !== 'user') {
          serverSync = false;
          return;
        }
        try {
          const prefs = await api.get('/auth/me/preferences');
          set(fromServer(prefs));
          serverSync = true;
        } catch {
          serverSync = false;
        }
      },
    }),
    {
      name: PREFERENCES_KEY,
      storage: createJSONStorage(() => localStorage),
      // Version 0 stored the old default, `system`, as if chosen: the console is dark by default.
      version: 1,
      migrate: (stored, version) => {
        const state = (stored ?? {}) as Partial<PreferenceValues>;
        return version < 1 && state.theme === 'system' ? { ...state, theme: 'dark' } : state;
      },
      partialize: (state) => values(state),
    },
  ),
);

export function resolveTheme(theme: Theme): 'light' | 'dark' {
  if (theme !== 'system') return theme;
  if (typeof window === 'undefined') return 'dark';
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

/** Whether an item is starred, kept up to date. */
export function useIsFavourite(kind: FavouriteKind, name: string): boolean {
  return usePreferences((s) => s.favourites.some((f) => f.kind === kind && f.name === name));
}
