'use client';

import { useRouter, useSearchParams } from 'next/navigation';
import { Fragment, useEffect, useMemo, useRef, useState } from 'react';

import { Icon, type IconName } from '@/components/ui/icon';
import { ModelMark } from '@/components/ui/marks';
import { Kbd } from '@/components/ui/misc';
import { useFocusTrap } from '@/components/ui/overlay';
import { EmptyPill } from '@/components/ui/states';
import { api } from '@/lib/api/client';
import { cn } from '@/lib/cn';
import { favouriteLink } from '@/lib/favourites';
import { NAV, navAllowed, SETTINGS_NAV } from '@/lib/nav';
import { usePreferences } from '@/stores/preferences';
import { type RecentItem, type RecentKind, useRecents } from '@/stores/recents';
import { hasGatewayPermission, hasPermission, navHolds, useSession } from '@/stores/session';
import { budgetId } from '@/features/budgets/types';
import type { Me } from '@/lib/api/types';
import { SETTINGS_SECTIONS } from '@/features/settings/components/sections';
import { SHORTCUTS } from '@/lib/shortcuts';
import { useUi } from '@/stores/ui';

import { SHARED_PARAMS, useOpenSettings } from './url-sync';

interface Entry {
  id: string;
  label: string;
  detail: string;
  icon: IconName;
  /** A model's name: the entry shows the model's maker mark in place of `icon`. */
  model?: string;
  href: string;
  group: string;
  /** The go-to shortcut's keys ("g o"). */
  keys?: string[];
  /** Opens the Settings dialog at this section instead of following `href`. */
  settings?: string;
  /** Puts this text in the search box instead of following `href` (a recent search). */
  search?: string;
  /** Remembered as a recent item when opened. */
  recent?: RecentItem;
}

interface Index {
  keys: string[];
  models: string[];
  users: string[];
  teams: string[];
  routes: string[];
  alerts: { id: string; title: string }[];
  mcpServers: string[];
  agents: string[];
  plugins: string[];
  providers: string[];
  guardrails: string[];
  mcpGateways: string[];
  /** Routing rules by name, with the route they belong to. */
  rules: { route: string; rule: string }[];
  alertRules: string[];
  projects: string[];
  /** Budgets by their id (`team/search/monthly`), with words for the row. */
  budgets: { id: string; label: string; detail: string }[];
}

/** Things the palette finds that are not remembered as recent items, and where they open. */
const PLACES: Record<
  'provider' | 'guardrail' | 'mcpGateway' | 'rule' | 'alertRule' | 'project' | 'budget',
  { path: string; icon: IconName; detail: string }
> = {
  provider: { path: '/providers/', icon: 'plug', detail: 'Provider' },
  guardrail: { path: '/safety/', icon: 'shield', detail: 'Guardrail rule' },
  mcpGateway: { path: '/mcp/', icon: 'tools', detail: 'MCP gateway' },
  rule: { path: '/routing/', icon: 'route', detail: 'Routing rule' },
  alertRule: { path: '/alerts/', icon: 'sliders', detail: 'Alert rule' },
  project: { path: '/teams/', icon: 'list', detail: 'Project' },
  budget: { path: '/budgets/', icon: 'dollar', detail: 'Budget' },
};

/**
 * Whether typed text is shaped like a request's record id or request id (a UUID), so "Open request"
 * is offered only then; other text searches the log instead.
 */
export function looksLikeRequestId(text: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(text);
}

/** Where a recent item opens, and its icon. */
const RECENT: Record<RecentKind, { path: string; param: string; icon: IconName; detail: string }> = {
  request: { path: '/request-log/', param: 'request', icon: 'list', detail: 'Request' },
  key: { path: '/keys/', param: 'key', icon: 'key', detail: 'API key' },
  model: { path: '/models/', param: 'model', icon: 'cube', detail: 'Model' },
  user: { path: '/teams/', param: 'user', icon: 'user', detail: 'Person' },
  team: { path: '/teams/', param: 'team', icon: 'users', detail: 'Team' },
  route: { path: '/routing/', param: 'route', icon: 'route', detail: 'Route' },
  alert: { path: '/alerts/', param: 'alert', icon: 'alert', detail: 'Alert' },
  change: { path: '/change-requests/', param: 'request', icon: 'pullRequest', detail: 'Change request' },
  mcp_server: { path: '/mcp/', param: 'server', icon: 'tools', detail: 'MCP server' },
  agent: { path: '/agents/', param: 'agent', icon: 'robot', detail: 'Agent' },
  plugin: { path: '/plugins/', param: 'plugin', icon: 'puzzle', detail: 'Plugin' },
};

/**
 * What the palette searches, each list read only when the principal may read it (on the whole
 * gateway for gateway-wide resources), so a narrow role makes no refused calls.
 */
async function loadIndex(me: Me | null): Promise<Index> {
  const may = (permission: string) => hasPermission(me, permission, null);
  const mayAll = (permission: string) => hasGatewayPermission(me, permission);
  const names = async (allowed: boolean, read: () => Promise<{ items: { name: string }[] }>) => {
    if (!allowed) return [];
    try {
      return (await read()).items.map((i) => i.name);
    } catch {
      return [];
    }
  };
  const alerts = async () => {
    if (!may('alerts:read')) return [];
    try {
      const page = await api.get('/alerts', { query: { status: ['firing', 'acknowledged'], limit: 200 } });
      return page.items.map((a) => ({ id: a.id, title: a.summary || a.rule }));
    } catch {
      return [];
    }
  };
  const routeList = async () => {
    if (!mayAll('models:read')) return [];
    try {
      return (await api.get('/routes', { query: { limit: 500 } })).items;
    } catch {
      return [];
    }
  };
  const budgetList = async () => {
    if (!may('policies:read')) return [];
    try {
      return (await api.get('/runtime/budgets', { query: { limit: 200 } })).items;
    } catch {
      return [];
    }
  };
  const [
    keys,
    models,
    users,
    teams,
    routeItems,
    open,
    mcpServers,
    agents,
    plugins,
    providers,
    guardrails,
    mcpGateways,
    alertRules,
    projects,
    counters,
  ] = await Promise.all([
    names(may('keys:read'), () => api.get('/keys', { query: { limit: 500 } })),
    names(mayAll('models:read'), () => api.get('/models', { query: { limit: 500 } })),
    names(mayAll('users:read'), () => api.get('/users', { query: { limit: 500 } })),
    names(may('teams:read'), () => api.get('/teams', { query: { limit: 500 } })),
    routeList(),
    alerts(),
    names(mayAll('mcp:read'), () => api.get('/mcp/servers', { query: { limit: 500 } })),
    names(mayAll('mcp:read'), () => api.get('/a2a/agents', { query: { limit: 500 } })),
    names(mayAll('plugins:read'), () => api.get('/plugins', { query: { limit: 500 } })),
    names(mayAll('providers:read'), () => api.get('/providers', { query: { limit: 500 } })),
    names(mayAll('policies:read'), () => api.get('/guardrails', { query: { limit: 500 } })),
    names(mayAll('mcp:read'), () => api.get('/mcp/gateways', { query: { limit: 500 } })),
    names(mayAll('alerts:read'), () => api.get('/alerts/rules', { query: { limit: 500 } })),
    names(may('projects:read'), () => api.get('/projects', { query: { limit: 500 } })),
    budgetList(),
  ]);
  const rules = routeItems.flatMap((r) => {
    const tree = (r.spec as { tree?: { rules?: { name?: string }[] } }).tree;
    return (tree?.rules ?? []).flatMap((rule) => (rule.name ? [{ route: r.name, rule: rule.name }] : []));
  });
  return {
    keys,
    models,
    users,
    teams,
    routes: routeItems.map((r) => r.name),
    alerts: open,
    mcpServers,
    agents,
    plugins,
    providers,
    guardrails,
    mcpGateways,
    rules,
    alertRules,
    projects,
    budgets: counters.map((c) => ({
      id: budgetId(c),
      label: c.name,
      detail: c.level === 'gateway' ? 'Gateway budget' : `Budget of ${c.level} ${c.owner}`,
    })),
  };
}

/** ⌘K: look up a request by id or text, or jump to a page, key, user, model or setting. */
export function CommandPalette() {
  const open = useUi((s) => s.paletteOpen);
  const setOpen = useUi((s) => s.setPaletteOpen);
  const router = useRouter();
  const openSettings = useOpenSettings();
  const params = useSearchParams();
  const ref = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState('');
  const [index, setIndex] = useState<Index | null>(null);
  const [active, setActive] = useState(0);
  // Closing (Escape, a click outside) forgets the query, so the palette reopens on Recent.
  const [shownOpen, setShownOpen] = useState(open);
  if (shownOpen !== open) {
    setShownOpen(open);
    if (!open) {
      setQuery('');
      setActive(0);
    }
  }
  const favourites = usePreferences((s) => s.favourites);
  const recentItems = useRecents((s) => s.items);
  const recentSearches = useRecents((s) => s.searches);
  const remember = useRecents((s) => s.visit);
  const searched = useRecents((s) => s.searched);
  const clearRecents = useRecents((s) => s.clear);
  const me = useSession((s) => s.me);
  useFocusTrap(ref, open, () => setOpen(false));

  useEffect(() => {
    if (open && !index) void loadIndex(me).then(setIndex);
  }, [open, index, me]);

  const shared = useMemo(() => {
    const out = new URLSearchParams();
    for (const k of SHARED_PARAMS) {
      const v = params.get(k);
      if (v) out.set(k, v);
    }
    return out;
  }, [params]);

  const entries = useMemo<Entry[]>(() => {
    const q = query.trim();
    const lower = q.toLowerCase();
    const link = (path: string, extra: Record<string, string> = {}) => {
      const p = new URLSearchParams(shared);
      for (const [k, v] of Object.entries(extra)) p.set(k, v);
      return `${path}?${p.toString()}`;
    };
    const out: Entry[] = [];
    for (const f of favourites) {
      const l = favouriteLink(f);
      if (!lower || l.label.toLowerCase().includes(lower))
        out.push({
          id: `fav-${f.kind}-${f.name}`,
          label: l.label,
          detail: l.kind,
          icon: l.icon,
          ...(f.kind === 'model' ? { model: f.name } : {}),
          href: link(l.path, l.query),
          group: 'Favourites',
        });
    }
    const known: Partial<Record<RecentKind, string[]>> = index
      ? { key: index.keys, model: index.models, user: index.users, team: index.teams, route: index.routes }
      : {};
    if (!q) {
      for (const r of recentItems) {
        if (known[r.kind] && !known[r.kind]?.includes(r.name)) continue;
        const k = RECENT[r.kind];
        out.push({
          id: `recent-${r.kind}-${r.name}`,
          label: r.label ?? r.name,
          detail: k.detail,
          icon: k.icon,
          ...(r.kind === 'model' ? { model: r.name } : {}),
          href: link(k.path, { [k.param]: r.name }),
          group: 'Recent',
          recent: r,
        });
      }
      for (const text of recentSearches)
        out.push({
          id: `search-${text}`,
          label: text,
          detail: 'Search',
          icon: 'history',
          href: '',
          group: 'Recent searches',
          search: text,
        });
    }
    const add = (names: string[] | undefined, kind: RecentKind, group: string) => {
      const k = RECENT[kind];
      for (const n of names ?? []) {
        if (lower && n.toLowerCase().includes(lower))
          out.push({
            id: `${kind}-${n}`,
            label: n,
            detail: k.detail,
            icon: k.icon,
            ...(kind === 'model' ? { model: n } : {}),
            href: link(k.path, kind === 'team' ? { tab: 'teams', team: n } : { [k.param]: n }),
            group,
            recent: { kind, name: n },
          });
      }
    };
    add(index?.keys, 'key', 'Keys, models and people');
    add(index?.models, 'model', 'Keys, models and people');
    add(index?.users, 'user', 'Keys, models and people');
    add(index?.teams, 'team', 'Keys, models and people');
    add(index?.routes, 'route', 'Routes and alerts');
    add(index?.mcpServers, 'mcp_server', 'MCP servers, agents and plugins');
    add(index?.agents, 'agent', 'MCP servers, agents and plugins');
    add(index?.plugins, 'plugin', 'MCP servers, agents and plugins');
    for (const a of index?.alerts ?? []) {
      if (lower && (a.title.toLowerCase().includes(lower) || a.id.toLowerCase().includes(lower)))
        out.push({
          id: `alert-${a.id}`,
          label: a.title,
          detail: 'Open alert',
          icon: 'alert',
          href: link('/alerts/', { alert: a.id }),
          group: 'Routes and alerts',
          recent: { kind: 'alert', name: a.id, label: a.title },
        });
    }
    const place = (
      names: string[] | undefined,
      kind: keyof typeof PLACES,
      extra: (n: string) => Record<string, string>,
    ) => {
      const p = PLACES[kind];
      for (const n of names ?? [])
        if (lower && n.toLowerCase().includes(lower))
          out.push({
            id: `${kind}-${n}`,
            label: n,
            detail: p.detail,
            icon: p.icon,
            href: link(p.path, extra(n)),
            group: 'Providers, rules and gateways',
          });
    };
    place(index?.providers, 'provider', () => ({}));
    place(index?.guardrails, 'guardrail', () => ({ tab: 'rules' }));
    place(index?.mcpGateways, 'mcpGateway', (n) => ({ tab: 'gateways', gateway: n }));
    place(index?.alertRules, 'alertRule', (n) => ({ tab: 'rules', rule: n }));
    place(index?.projects, 'project', (n) => ({ tab: 'projects', detail: n }));
    for (const b of index?.budgets ?? [])
      if (lower && (b.label.toLowerCase().includes(lower) || b.id.toLowerCase().includes(lower)))
        out.push({
          id: `budget-${b.id}`,
          label: b.label,
          detail: b.detail,
          icon: PLACES.budget.icon,
          href: link(PLACES.budget.path, { budget: b.id }),
          group: 'Providers, rules and gateways',
        });
    for (const r of index?.rules ?? [])
      if (lower && r.rule.toLowerCase().includes(lower))
        out.push({
          id: `rule-${r.route}-${r.rule}`,
          label: r.rule,
          detail: `${PLACES.rule.detail} in ${r.route}`,
          icon: PLACES.rule.icon,
          href: link(PLACES.rule.path, { route: r.route, rule: r.rule }),
          group: 'Providers, rules and gateways',
        });
    // An exact name first, then the rest in their groups; requests by text come after what was named.
    if (lower) {
      const exact = out.filter((e) => e.label.toLowerCase() === lower);
      const rest = out.filter((e) => e.label.toLowerCase() !== lower);
      out.length = 0;
      out.push(...exact.map((e) => ({ ...e, group: 'Best match' })), ...rest);
    }
    if (q) {
      if (looksLikeRequestId(q)) {
        out.push({
          id: 'req',
          label: `Open request ${q}`,
          detail: 'Request log',
          icon: 'list',
          href: link('/request-log/', { request: q }),
          group: 'Requests',
        });
      }
      out.push({
        id: 'logq',
        label: `Requests containing “${q}”`,
        detail: 'Request log',
        icon: 'search',
        href: link('/request-log/', { q }),
        group: 'Requests',
      });
    }
    const screens = [...NAV.flatMap((s) => s.items), SETTINGS_NAV].filter((s) =>
      navAllowed(s, navHolds(me, null)),
    );
    for (const s of screens) {
      if (!lower || s.label.toLowerCase().includes(lower))
        out.push({
          id: `nav-${s.id}`,
          label: s.label,
          detail: 'Page',
          icon: s.icon,
          href: link(s.href, s.query),
          group: 'Pages',
          ...(s.shortcut ? { keys: ['g', s.shortcut] } : {}),
          ...(s.id === SETTINGS_NAV.id ? { settings: 'general' } : {}),
        });
    }
    for (const section of SETTINGS_SECTIONS) {
      const words = [section.id, ...(section.aliases ?? []), section.label.toLowerCase()];
      if (lower && words.some((w) => w.includes(lower)))
        out.push({
          id: `set-${section.id}`,
          label: `Gateway settings › ${section.label}`,
          detail: 'Setting',
          icon: section.icon,
          href: '',
          settings: section.id,
          group: 'Settings',
        });
    }
    return out.slice(0, 40);
  }, [query, index, shared, favourites, recentItems, recentSearches, me]);

  if (!open) return null;

  function go(entry: Entry | undefined) {
    if (!entry) return;
    if (entry.search !== undefined) {
      setQuery(entry.search);
      setActive(0);
      return;
    }
    // A lookup by id may find nothing; only searches and what was found are worth remembering.
    if (query.trim() && entry.id !== 'req') searched(query);
    if (entry.recent) remember(entry.recent);
    setOpen(false);
    setQuery('');
    if (entry.settings) openSettings(entry.settings);
    else router.push(entry.href);
  }

  return (
    <div className="fixed inset-0 z-[60] flex items-start justify-center bg-overlay p-4 pt-[12vh]">
      <div className="fixed inset-0" aria-hidden="true" onClick={() => setOpen(false)} />
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-label="Search"
        className="relative w-full max-w-[640px] animate-fade-in overflow-hidden rounded-container border border-line-strong bg-raised shadow-pop"
      >
        <div className="flex items-center gap-2.5 border-b border-line px-4">
          <span className="text-dim">
            <Icon name="search" size={16} />
          </span>
          <input
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-results"
            aria-activedescendant={entries[active] ? `palette-${entries[active].id}` : undefined}
            aria-label="Search"
            placeholder="Search keys, models, people, routes, alerts, requests, settings…"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActive(0);
            }}
            onKeyDown={(e) => {
              if (e.key === 'ArrowDown') {
                e.preventDefault();
                setActive((a) => Math.min(entries.length - 1, a + 1));
              } else if (e.key === 'ArrowUp') {
                e.preventDefault();
                setActive((a) => Math.max(0, a - 1));
              } else if (e.key === 'Enter') {
                e.preventDefault();
                go(entries[active]);
              }
            }}
            className="h-12 min-w-0 flex-1 bg-transparent text-base text-fg outline-none placeholder:text-dim"
          />
          <Kbd>esc</Kbd>
        </div>
        <ul
          id="palette-results"
          role="listbox"
          aria-label="Results"
          className="max-h-[56vh] overflow-y-auto p-1.5"
        >
          {entries.length === 0 ? (
            <li className="flex justify-center px-3 py-8">
              <EmptyPill>No results for “{query.trim()}”</EmptyPill>
            </li>
          ) : null}
          {entries.map((e, i) => (
            <Fragment key={e.id}>
              {i === 0 || entries[i - 1]?.group !== e.group ? (
                <li role="presentation" className="px-3 pt-2.5 pb-1 text-caption font-medium text-dim">
                  {e.group}
                </li>
              ) : null}
              <li id={`palette-${e.id}`} role="option" aria-selected={i === active}>
                <button
                  type="button"
                  tabIndex={-1}
                  onMouseEnter={() => setActive(i)}
                  onClick={() => go(e)}
                  className={cn(
                    'flex h-9 w-full items-center gap-3 rounded-sm px-3 text-left',
                    i === active ? 'bg-accent-dim' : 'hover:bg-hover',
                  )}
                >
                  <span className="text-muted">
                    {e.model ? <ModelMark model={e.model} size="md" /> : <Icon name={e.icon} size={16} />}
                  </span>
                  <span className="flex-1 truncate text-body text-fg">{e.label}</span>
                  {e.keys ? (
                    <span className="flex gap-1">
                      {e.keys.map((k) => (
                        <Kbd key={k}>{k}</Kbd>
                      ))}
                    </span>
                  ) : (
                    <span className="text-caption text-dim">{e.detail}</span>
                  )}
                </button>
              </li>
            </Fragment>
          ))}
        </ul>
        <div className="flex items-center gap-4 border-t border-line bg-canvas px-4 py-2 text-caption text-dim">
          <span className="flex items-center gap-1.5">
            <Kbd>↑</Kbd>
            <Kbd>↓</Kbd> to move
          </span>
          <span className="flex items-center gap-1.5">
            <Kbd>↵</Kbd> to open
          </span>
          {!query && (recentItems.length > 0 || recentSearches.length > 0) ? (
            <button
              type="button"
              onClick={clearRecents}
              className="ml-auto text-caption text-muted hover:text-fg"
            >
              Clear recent
            </button>
          ) : null}
          <span
            className={cn(
              'flex items-center gap-1.5',
              query || (recentItems.length === 0 && recentSearches.length === 0) ? 'ml-auto' : '',
            )}
          >
            <Kbd>?</Kbd> all shortcuts
          </span>
        </div>
      </div>
    </div>
  );
}

export function ShortcutsDialog() {
  const open = useUi((s) => s.shortcutsOpen);
  const setOpen = useUi((s) => s.setShortcutsOpen);
  const ref = useRef<HTMLDivElement>(null);
  useFocusTrap(ref, open, () => setOpen(false));
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-[60] flex items-start justify-center bg-overlay p-4 pt-[12vh]">
      <div className="fixed inset-0" aria-hidden="true" onClick={() => setOpen(false)} />
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcuts-title"
        tabIndex={-1}
        className="relative w-full max-w-[480px] animate-fade-in rounded-container border border-line-strong bg-raised p-5 shadow-pop"
      >
        <h2 id="shortcuts-title" className="mb-3 text-title font-semibold text-fg">
          Keyboard shortcuts
        </h2>
        <dl className="flex flex-col gap-2">
          {SHORTCUTS.map((s) => (
            <div key={s.description} className="flex items-center justify-between gap-4">
              <dt className="text-ui text-muted">{s.description}</dt>
              <dd className="flex shrink-0 gap-1">
                {s.keys.map((k) =>
                  k === 'then' ? (
                    <span key={k} className="px-0.5 text-caption text-dim">
                      then
                    </span>
                  ) : (
                    <Kbd key={k}>{k}</Kbd>
                  ),
                )}
              </dd>
            </div>
          ))}
        </dl>
      </div>
    </div>
  );
}
