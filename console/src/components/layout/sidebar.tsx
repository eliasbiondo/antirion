'use client';

import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { useEffect } from 'react';

import { IconButton } from '@/components/ui/button';
import { BrandMark, Icon } from '@/components/ui/icon';
import { ModelMark } from '@/components/ui/marks';
import { cn } from '@/lib/cn';
import { formatCount, formatPercent } from '@/lib/format';
import { useMediaQuery } from '@/lib/hooks';
import { favouriteLink } from '@/lib/favourites';
import { activeNav, NAV, navAllowed, type NavItem } from '@/lib/nav';
import { type Favourite, usePreferences } from '@/stores/preferences';
import { scopeOrganization, useScope } from '@/stores/scope';
import { hasGatewayPermission, hasPermission, navHolds, useSession } from '@/stores/session';
import { useShell } from '@/stores/shell';
import { useUi } from '@/stores/ui';

import { useScopedHref } from './url-sync';
import { Tooltip } from '@/components/ui/tooltip';

function SideLink({ item, active, collapsed }: { item: NavItem; active: boolean; collapsed: boolean }) {
  const href = useScopedHref(item.href, item.query);
  const facts = useShell();
  const compactNumbers = usePreferences((s) => s.compactNumbers);
  const count =
    item.count === 'models'
      ? facts.modelCount
      : item.count === 'providers'
        ? facts.providerCount
        : item.count === 'requests'
          ? facts.requests
          : item.count === 'flagged'
            ? facts.flagged
            : item.count === 'alerts'
              ? facts.alertsFiring
              : item.count === 'changes'
                ? facts.pendingChanges
                : null;
  const warn = item.count === 'flagged' || item.count === 'alerts' || item.count === 'changes';
  const tip = item.shortcut ? `${item.label}  (g ${item.shortcut})` : item.label;
  const link = (
    <Link
      href={href}
      aria-current={active ? 'page' : undefined}
      className={cn(
        'relative flex h-[30px] items-center gap-2.5 rounded-sm px-2.5 text-body transition-colors',
        collapsed && 'justify-center px-0',
        active ? 'bg-accent-dim font-medium text-fg' : 'text-muted hover:bg-hover hover:text-fg',
      )}
    >
      {active ? (
        <span
          aria-hidden="true"
          className="absolute top-1.5 bottom-1.5 -left-2 w-0.5 rounded-full bg-accent"
        />
      ) : null}
      <span className={cn('shrink-0', active ? 'text-accent' : 'text-muted')}>
        <Icon name={item.icon} size={16} />
      </span>
      {collapsed ? (
        <span className="sr-only">{item.label}</span>
      ) : (
        <span className="truncate">{item.label}</span>
      )}
      {!collapsed && count !== null && count > 0 ? (
        <span
          className={cn('mono ml-auto text-caption tabular-nums', warn ? 'text-warn' : 'text-dim')}
          aria-label={`${formatCount(count)} ${warn ? 'need attention' : ''}`.trim()}
        >
          {formatCount(count, { compact: compactNumbers })}
        </span>
      ) : null}
    </Link>
  );
  return collapsed ? (
    <Tooltip content={tip} side="bottom" describe={false}>
      {link}
    </Tooltip>
  ) : (
    link
  );
}

/** At most this many favourites show in the sidebar; search lists them all. */
const SIDEBAR_FAVOURITES = 8;

function FavouriteLink({ favourite }: { favourite: Favourite }) {
  const link = favouriteLink(favourite);
  const href = useScopedHref(link.path, link.query);
  return (
    <Link
      href={href}
      className="flex h-[30px] items-center gap-2.5 rounded-sm px-2.5 text-body text-muted transition-colors hover:bg-hover hover:text-fg"
    >
      <span className="shrink-0 text-dim">
        {favourite.kind === 'model' ? (
          <ModelMark model={favourite.name} size="md" />
        ) : (
          <Icon name={link.icon} size={16} />
        )}
      </span>
      <span className={cn('truncate', favourite.kind !== 'page' && 'mono text-ui')}>{link.label}</span>
      <span className="sr-only">, {link.kind}</span>
    </Link>
  );
}

/** The user's starred pages and items, above the screens; it folds away and keeps that choice. */
function Favourites() {
  const favourites = usePreferences((s) => s.favourites);
  const open = usePreferences((s) => s.favouritesOpen);
  const set = usePreferences((s) => s.set);
  const openPalette = useUi((s) => s.setPaletteOpen);
  if (favourites.length === 0) return null;
  const shown = favourites.slice(0, SIDEBAR_FAVOURITES);
  return (
    <div className="px-2 pt-3 pb-1.5">
      <button
        type="button"
        aria-expanded={open}
        aria-controls="sidebar-favourites"
        onClick={() => set({ favouritesOpen: !open })}
        className="mx-0.5 mt-1 mb-1.5 flex w-[calc(100%-4px)] items-center gap-1.5 rounded-sm px-2 text-left text-caption font-medium text-dim hover:text-fg"
      >
        <Icon name={open ? 'chevronDown' : 'chevronRight'} size={12} />
        Favourites
        <span className="ml-auto tabular-nums">{favourites.length}</span>
      </button>
      {open ? (
        <ul id="sidebar-favourites" className="flex flex-col gap-px">
          {shown.map((f) => (
            <li key={`${f.kind}:${f.name}`}>
              <FavouriteLink favourite={f} />
            </li>
          ))}
          {favourites.length > shown.length ? (
            <li>
              <button
                type="button"
                onClick={() => openPalette(true)}
                className="flex h-[30px] w-full items-center rounded-sm px-2.5 text-left text-caption text-muted hover:bg-hover hover:text-fg"
              >
                {favourites.length - shown.length} more in search
              </button>
            </li>
          ) : null}
        </ul>
      ) : null}
    </div>
  );
}

const NOT_SERVED = 'Not available on this gateway';

/** The prototype's footer facts: ingress, nodes, availability and the release. */
function Footer() {
  const facts = useShell();
  const me = useSession((s) => s.me);
  /** Why a figure is blank: still loading, not readable by this principal, or not served. */
  const why = (readable: boolean) =>
    facts.loadedAt === null ? 'Loading' : readable ? NOT_SERVED : 'Your role does not include this figure';
  const rows: { label: string; value: string | null; tone?: 'success'; blank: string }[] = [
    {
      label: 'Ingress',
      blank: why(hasPermission(me, 'analytics:read', null)),
      value:
        facts.ingressPerMinute === null
          ? null
          : `${formatCount(facts.ingressPerMinute, { compact: true })}/min`,
    },
    {
      label: 'Nodes ready',
      blank: why(hasGatewayPermission(me, 'cluster:read')),
      value: facts.nodesKnown === null ? null : `${facts.nodesReady ?? 0} / ${facts.nodesKnown}`,
    },
    {
      label: 'Uptime (30 days)',
      blank: why(hasGatewayPermission(me, 'analytics:read')),
      value: facts.availability30d === null ? null : formatPercent(facts.availability30d, 2),
      tone: 'success',
    },
  ];
  return (
    <dl className="flex shrink-0 flex-col gap-1.5 border-t border-line px-3 py-3 text-caption">
      {rows.map((r) => (
        <div key={r.label} className="flex items-center justify-between gap-2">
          <dt className="text-dim">{r.label}</dt>
          <Tooltip content={r.value === null ? r.blank : null}>
            <dd
              className={cn(
                'mono tabular-nums',
                r.value === null ? 'text-dim' : r.tone === 'success' ? 'text-success' : 'text-muted',
              )}
            >
              {r.value ?? '—'}
              {r.value === null ? <span className="sr-only"> ({r.blank})</span> : null}
            </dd>
          </Tooltip>
        </div>
      ))}
      <div className="flex items-center justify-between gap-2 border-t border-dashed border-line pt-1.5">
        <dt className="text-dim">Version</dt>
        <dd className="mono text-muted">{facts.status ? facts.status.version : '—'}</dd>
      </div>
    </dl>
  );
}

/**
 * The sidebar: Observe, Govern, Configure and Extend with their counters, and the gateway's facts
 * at the bottom. From 1024px it is a column that collapses to icons; under that it is a sheet the
 * menu button opens.
 */
export function Sidebar() {
  const pathname = usePathname();
  const active = activeNav(pathname);
  const wide = useMediaQuery('(min-width: 1024px)');
  const collapsedPref = usePreferences((s) => s.sidebarCollapsed);
  const navOpen = useUi((s) => s.navOpen);
  const setNavOpen = useUi((s) => s.setNavOpen);
  const collapsed = wide && collapsedPref;
  const me = useSession((s) => s.me);
  const organization = scopeOrganization(useScope((s) => s.scope));
  const allowed = (item: NavItem) => navAllowed(item, navHolds(me, organization));

  useEffect(() => {
    setNavOpen(false);
  }, [pathname, setNavOpen]);

  useEffect(() => {
    if (!navOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setNavOpen(false);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [navOpen, setNavOpen]);

  return (
    <>
      {navOpen ? (
        <div
          aria-hidden="true"
          className="fixed inset-0 z-40 bg-overlay lg:hidden"
          onClick={() => setNavOpen(false)}
        />
      ) : null}
      <nav
        aria-label="Console"
        className={cn(
          'flex min-h-0 flex-col border-r border-line bg-raised',
          'max-lg:fixed max-lg:inset-y-0 max-lg:left-0 max-lg:z-50 max-lg:w-[272px] max-lg:shadow-pop max-lg:transition-transform',
          navOpen ? 'max-lg:translate-x-0' : 'max-lg:invisible max-lg:-translate-x-full',
        )}
      >
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-line px-3 lg:hidden">
          <BrandMark size={18} />
          <span className="text-base font-semibold tracking-tight text-fg">Antirion</span>
          <IconButton icon="x" label="Close menu" className="ml-auto" onClick={() => setNavOpen(false)} />
        </div>
        {/* The links scroll on their own; the gateway facts stay pinned under them. */}
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain pb-2">
          {collapsed ? null : <Favourites />}
          {NAV.filter((section) => section.items.some(allowed)).map((section, i) => (
            <div
              key={section.label}
              className={cn(
                'px-2 pt-3 pb-1.5',
                collapsed && 'px-1',
                collapsed && i > 0 && 'mx-2 mt-1.5 border-t border-line px-0 pt-3',
              )}
            >
              {collapsed ? null : (
                <h2 className="mx-2.5 mt-1 mb-1.5 text-caption font-medium text-dim">{section.label}</h2>
              )}
              <ul className="flex flex-col gap-px">
                {section.items.filter(allowed).map((item) => (
                  <li key={item.id}>
                    <SideLink item={item} active={active === item.id} collapsed={collapsed} />
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
        {collapsed ? null : <Footer />}
      </nav>
    </>
  );
}
