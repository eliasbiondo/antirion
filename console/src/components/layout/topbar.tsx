'use client';

import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { useState } from 'react';

import { IconButton } from '@/components/ui/button';
import { BrandMark, Icon } from '@/components/ui/icon';
import { Kbd } from '@/components/ui/misc';
import { comparesWindows, usesTimeRange } from '@/lib/nav';
import { resolveTheme, usePreferences } from '@/stores/preferences';
import { useShell } from '@/stores/shell';
import { useTimeRange } from '@/stores/time-range';
import { useUi } from '@/stores/ui';

import { AccountMenu } from './account-menu';
import { ComparePicker } from './compare-picker';
import { CreateOrganizationDialog } from '@/features/teams-users/components/organizations-panel';
import { EnvironmentSelect, ScopeSwitcher } from './scope-switcher';
import { TimeRangePicker } from './time-range-picker';
import { useScopedHref } from './url-sync';

/**
 * The top bar: menu, brand, organization, search; on the right the time range, environment and
 * refresh (only on pages whose data they change), the theme and the account. On a phone the
 * comparison window stays in the URL but its control gives way, so the account stays on screen.
 */
export function Topbar() {
  const toggleSidebar = usePreferences((s) => s.toggleSidebar);
  const collapsed = usePreferences((s) => s.sidebarCollapsed);
  const toggleTheme = usePreferences((s) => s.toggleTheme);
  const theme = usePreferences((s) => s.theme);
  const refresh = useTimeRange((s) => s.refresh);
  const openPalette = useUi((s) => s.setPaletteOpen);
  const setNavOpen = useUi((s) => s.setNavOpen);
  const navOpen = useUi((s) => s.navOpen);
  const version = useShell((s) => s.status?.version);
  const pathname = usePathname();
  const home = useScopedHref('/');
  const [creatingOrg, setCreatingOrg] = useState(false);
  const dark = resolveTheme(theme) === 'dark';
  const timed = usesTimeRange(pathname);
  const compares = comparesWindows(pathname);

  function onMenu() {
    if (window.matchMedia('(min-width: 1024px)').matches) toggleSidebar();
    else setNavOpen(!navOpen);
  }

  return (
    <header className="col-span-full flex h-12 min-w-0 items-center gap-2 border-b border-line bg-raised pr-3 pl-2.5 sm:gap-3 sm:pl-3.5">
      <div className="flex shrink-0 items-center gap-2 lg:min-w-[calc(var(--spacing-sidebar)-14px)] lg:border-r lg:border-line lg:pr-3">
        <IconButton
          icon="menu"
          label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          aria-expanded={navOpen || undefined}
          onClick={onMenu}
        />
        <Link href={home} className="flex items-center gap-2 text-fg" aria-label="Antirion overview">
          <BrandMark size={18} />
          <span className="text-base font-semibold tracking-tight max-sm:sr-only">Antirion</span>
          {version ? <small className="mono text-caption text-muted max-lg:hidden">v{version}</small> : null}
        </Link>
      </div>
      <ScopeSwitcher onCreateOrganization={() => setCreatingOrg(true)} />
      {creatingOrg ? <CreateOrganizationDialog onClose={() => setCreatingOrg(false)} /> : null}
      <button
        type="button"
        onClick={() => openPalette(true)}
        className="hidden h-7 max-w-[520px] min-w-0 flex-1 items-center gap-2 rounded-control border border-line bg-canvas px-2.5 text-left text-ui text-dim hover:border-line-strong md:flex"
        aria-label="Search"
      >
        <Icon name="search" size={14} />
        <span className="flex-1 truncate">Search or jump to…</span>
        <Kbd>⌘K</Kbd>
      </button>
      <div className="min-w-0 flex-1" />
      <div className="flex shrink-0 items-center gap-1 sm:gap-1.5">
        <IconButton icon="search" label="Search" onClick={() => openPalette(true)} className="md:hidden" />
        {timed ? (
          <>
            <div className="max-xl:hidden">
              <EnvironmentSelect />
            </div>
            <TimeRangePicker />
            {compares ? (
              <div className="max-sm:hidden">
                <ComparePicker />
              </div>
            ) : null}
            <IconButton icon="refresh" label="Refresh" onClick={refresh} className="max-sm:hidden" />
          </>
        ) : null}
        <IconButton
          icon={dark ? 'sun' : 'moon'}
          label={dark ? 'Switch to light theme' : 'Switch to dark theme'}
          onClick={toggleTheme}
          className="border-line max-sm:hidden"
        />
        <AccountMenu />
      </div>
    </header>
  );
}
