import type { IconName } from '@/components/ui/icon';

export interface NavItem {
  id: string;
  label: string;
  href: string;
  icon: IconName;
  /** Extra query parameters of the link. */
  query?: Record<string, string>;
  /** A sidebar count from the shell's facts. */
  count?: 'models' | 'providers' | 'requests' | 'flagged' | 'alerts' | 'changes';
  /** Shown only to principals holding this permission (or one of these), as the page reads nothing without it. */
  permission?: string | readonly string[];
  /** Whether the permission must be held on the whole gateway: the page reads gateway-wide resources. */
  gatewayWide?: boolean;
  /** The `g` shortcut's second key ("o" for g o). */
  shortcut?: string;
  /** Whether the page reads the time range (and environment), so the top bar shows them. */
  timed?: boolean;
  /** Whether the page shows deltas against a comparison window, so the top bar offers "Compare with". */
  compares?: boolean;
}

export interface NavSection {
  label: string;
  items: NavItem[];
}

/** The sidebar of the prototype: Observe, Govern and Configure, then Extend (tools, agents, plugins) and Settings. */
export const NAV: NavSection[] = [
  {
    label: 'Observe',
    items: [
      {
        id: 'overview',
        label: 'Overview',
        href: '/',
        icon: 'activity',
        shortcut: 'o',
        timed: true,
        compares: true,
      },
      {
        id: 'request-log',
        label: 'Request log',
        href: '/request-log/',
        icon: 'list',
        count: 'requests',
        shortcut: 'l',
        timed: true,
      },
      {
        id: 'analytics',
        label: 'Analytics',
        href: '/analytics/',
        icon: 'chart',
        shortcut: 'a',
        timed: true,
        compares: true,
      },
      {
        id: 'alerts',
        label: 'Alerts',
        href: '/alerts/',
        icon: 'alert',
        count: 'alerts',
        timed: true,
        permission: 'alerts:read',
      },
    ],
  },
  {
    label: 'Govern',
    items: [
      { id: 'budgets', label: 'Budgets', href: '/budgets/', icon: 'dollar', shortcut: 'b', timed: true },
      {
        id: 'safety',
        label: 'Safety & PII',
        href: '/safety/',
        icon: 'shield',
        count: 'flagged',
        timed: true,
      },
      { id: 'keys', label: 'API keys', href: '/keys/', icon: 'key', shortcut: 'k' },
      {
        id: 'teams',
        label: 'Teams & users',
        href: '/teams/',
        icon: 'users',
        shortcut: 't',
        permission: ['users:read', 'teams:read'],
      },
      {
        id: 'change-requests',
        label: 'Change requests',
        href: '/change-requests/',
        icon: 'pullRequest',
        count: 'changes',
        permission: 'config:read',
      },
    ],
  },
  {
    label: 'Configure',
    items: [
      {
        id: 'models',
        label: 'Models',
        href: '/models/',
        icon: 'cube',
        count: 'models',
        shortcut: 'm',
        permission: 'models:read',
        gatewayWide: true,
      },
      {
        id: 'providers',
        label: 'Providers',
        href: '/providers/',
        icon: 'plug',
        count: 'providers',
        permission: 'providers:read',
        gatewayWide: true,
      },
      {
        id: 'cache',
        label: 'Cache',
        href: '/cache/',
        icon: 'cache',
        shortcut: 'c',
        permission: 'policies:read',
      },
      {
        id: 'routing',
        label: 'Routing policy',
        href: '/routing/',
        icon: 'route',
        shortcut: 'r',
        permission: 'models:read',
        gatewayWide: true,
      },
      {
        id: 'playground',
        label: 'Playground',
        href: '/playground/',
        icon: 'play',
        shortcut: 'p',
        permission: 'playground:send',
      },
    ],
  },
  {
    label: 'Extend',
    items: [
      { id: 'mcp', label: 'MCP', href: '/mcp/', icon: 'tools', permission: 'mcp:read' },
      { id: 'agents', label: 'Agents', href: '/agents/', icon: 'robot', permission: 'mcp:read' },
      { id: 'plugins', label: 'Plugins', href: '/plugins/', icon: 'puzzle', permission: 'plugins:read' },
    ],
  },
];

/**
 * Gateway settings is a dialog over the current page, opened from the account menu (`?settings=<section>`), not a page; its old address
 * `/settings/` still works and opens the dialog.
 */
export const SETTINGS_NAV: NavItem = {
  id: 'settings',
  label: 'Gateway settings',
  href: '/settings/',
  icon: 'settings',
  shortcut: 's',
};

/** The query parameter that opens the Settings dialog at a section. */
export const SETTINGS_PARAM = 'settings';

const ALL_ITEMS = [...NAV.flatMap((s) => s.items), SETTINGS_NAV];

/**
 * Whether a principal may open a nav item: `holds` answers for one permission, on the whole gateway
 * when the item's page reads gateway-wide resources. The sidebar, the palette, the page and the
 * Settings sections use it.
 */
export function navAllowed(
  item: Pick<NavItem, 'permission' | 'gatewayWide'>,
  holds: (permission: string, gatewayWide: boolean) => boolean,
): boolean {
  if (!item.permission) return true;
  const any = typeof item.permission === 'string' ? [item.permission] : item.permission;
  return any.some((p) => holds(p, item.gatewayWide === true));
}

/** The nav item for a pathname (without the base path), for the active highlight. */
export function activeNav(pathname: string): string | null {
  const clean = pathname.endsWith('/') ? pathname : `${pathname}/`;
  if (clean === '/') return 'overview';
  for (const item of NAV.flatMap((s) => s.items)) {
    if (item.href !== '/' && clean.startsWith(item.href)) return item.id;
  }
  return null;
}

/** The nav item a pathname belongs to. */
export function navItem(id: string | null): NavItem | undefined {
  return ALL_ITEMS.find((i) => i.id === id);
}

/** Whether the page at a pathname reads the time range, so the top bar shows the picker. */
export function usesTimeRange(pathname: string): boolean {
  return navItem(activeNav(pathname))?.timed === true;
}

/** Whether the page at a pathname shows deltas, so the top bar offers "Compare with". */
export function comparesWindows(pathname: string): boolean {
  return navItem(activeNav(pathname))?.compares === true;
}

/** `g` then a key: the go-to shortcuts, from the nav items that declare one. */
export const GO_KEYS: Record<string, string> = Object.fromEntries(
  ALL_ITEMS.flatMap((i) => (i.shortcut ? [[i.shortcut, i.href]] : [])),
);
