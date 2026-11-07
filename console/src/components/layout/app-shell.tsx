'use client';

import { usePathname, useRouter } from 'next/navigation';
import { Suspense, useEffect, useRef, useState, type ReactNode } from 'react';

import { AccountDialog } from '@/features/account/components/account-dialog';
import { GatewayNotices } from '@/features/settings/components/gateway-notices';
import { SettingsDialog } from '@/features/settings/components/settings-dialog';
import { Button } from '@/components/ui/button';
import { BrandMark } from '@/components/ui/icon';
import { KeyboardHintsSlot } from '@/components/ui/keyboard-hints';
import { Toaster } from '@/components/ui/misc';
import { Dialog } from '@/components/ui/overlay';
import { activeNav, GO_KEYS, navAllowed, navItem, SETTINGS_NAV } from '@/lib/nav';
import { Forbidden } from '@/components/ui/gate';
import { PageHeader } from '@/components/ui/layout';
import { focusPageSearch, isTypingTarget, runPageShortcut } from '@/lib/shortcuts';
import { usePreferences } from '@/stores/preferences';
import { scopeOrganization, useScope } from '@/stores/scope';
import { navHolds, useSession } from '@/stores/session';
import { useShell } from '@/stores/shell';
import { useQueryWindow } from '@/stores/time-range';
import { useUi } from '@/stores/ui';

import { ChangeControlHost } from './change-control';
import { CommandPalette, ShortcutsDialog } from './command-palette';
import { RecentTracker } from './recent-tracker';
import { Sidebar } from './sidebar';
import { Topbar } from './topbar';
import { UrlSync, useOpenSettings } from './url-sync';
import { useTranslate } from '@/lib/i18n';

function useGlobalKeys() {
  const router = useRouter();
  const openSettings = useOpenSettings();
  const pending = useRef<number | null>(null);
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const ui = useUi.getState();
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        ui.setPaletteOpen(!ui.paletteOpen);
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey || isTypingTarget(e.target)) return;
      if (document.querySelector('[aria-modal="true"]')) return;
      if (pending.current !== null) {
        const target = GO_KEYS[e.key];
        window.clearTimeout(pending.current);
        pending.current = null;
        if (target === SETTINGS_NAV.href) {
          e.preventDefault();
          openSettings();
        } else if (target) {
          e.preventDefault();
          router.push(`${target}${window.location.search}`);
        }
        return;
      }
      if (e.key === '?') {
        e.preventDefault();
        ui.setShortcutsOpen(true);
      } else if (e.key === '/') {
        if (focusPageSearch()) e.preventDefault();
      } else if (e.key === 'g') {
        pending.current = window.setTimeout(() => {
          pending.current = null;
        }, 1200);
      } else if (runPageShortcut(e.key)) {
        e.preventDefault();
      }
    }
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [router, openSettings]);
}

function ShellFacts() {
  const window = useQueryWindow();
  const scope = useScope((s) => s.scope);
  const environment = useScope((s) => s.environment);
  const load = useShell((s) => s.load);
  useEffect(() => {
    void load({
      from: window.from,
      to: window.to,
      organization: scopeOrganization(scope),
      tag: environment ? `env:${environment}` : null,
    });
  }, [window, scope, environment, load]);
  return null;
}

/**
 * A page the principal may not read says so in place of its content, with the sidebar's own check,
 * so it makes none of the calls the gateway would refuse.
 */
function PageAccess({ children }: { children: ReactNode }) {
  const translate = useTranslate();
  const pathname = usePathname();
  const me = useSession((s) => s.me);
  const organization = scopeOrganization(useScope((s) => s.scope));
  const item = navItem(activeNav(pathname));
  if (!item || navAllowed(item, navHolds(me, organization))) return children;
  return (
    <>
      <PageHeader title={translate(item.label)} />
      <div className="flex min-h-[calc(100vh-48px-57px)] items-center justify-center bg-raised">
        <Forbidden what={translate(item.label)} />
      </div>
    </>
  );
}

/**
 * The scrolling main area: the page, then the slot the keyboard-hints strip renders into. The page
 * grows to fill the view, so the strip sits at the bottom of the view under a short list and stays
 * pinned there (sticky) over a long one; `main` is the only thing that scrolls.
 */
export function MainArea({ children }: { children: ReactNode }) {
  const [slot, setSlot] = useState<HTMLDivElement | null>(null);
  return (
    // While a page shows its main empty, error or loading state, the view below the page's content
    // takes the raised surface, so that state's band runs to the bottom of the view.
    <main
      id="main"
      tabIndex={-1}
      className="flex min-w-0 flex-col overflow-auto outline-none has-[[data-page-state]]:bg-raised"
    >
      <div data-page className="flex shrink-0 grow flex-col [&>*]:shrink-0">
        <KeyboardHintsSlot.Provider value={slot}>{children}</KeyboardHintsSlot.Provider>
      </div>
      <div ref={setSlot} data-keyboard-hints-slot="" className="sticky bottom-0 z-10 shrink-0" />
    </main>
  );
}

/** The splash shown while the session is checked; static, with no data. */
export function Splash() {
  const translate = useTranslate();
  return (
    <div
      className="flex h-screen items-center justify-center bg-canvas text-muted"
      role="status"
      aria-live="polite"
    >
      <span className="flex items-center gap-2 text-ui">
        <BrandMark size={16} />
        {translate('Loading Antirion…')}
      </span>
    </div>
  );
}

function SessionExpiredDialog() {
  const translate = useTranslate();
  const status = useSession((s) => s.status);
  const router = useRouter();
  const pathname = usePathname();
  return (
    <Dialog
      open={status === 'expired'}
      onClose={() => undefined}
      title={translate('Your session expired')}
      description={translate('Sign in again to pick up where you left off.')}
      size="sm"
      role="alertdialog"
      footer={
        <Button
          variant="primary"
          onClick={() =>
            router.replace(`/login/?next=${encodeURIComponent(pathname + window.location.search)}`)
          }
        >
          {translate('Sign in again')}
        </Button>
      }
    >
      <p className="text-ui text-muted">{translate('Unsaved changes on this page will be lost.')}</p>
    </Dialog>
  );
}

/**
 * The console frame: topbar, sidebar and the page. It renders only once the session is known, so
 * the static HTML is a splash with no per-user data.
 */
export function AppShell({ children }: { children: ReactNode }) {
  const translate = useTranslate();
  const status = useSession((s) => s.status);
  const load = useSession((s) => s.load);
  const pullPreferences = usePreferences((s) => s.pull);
  const router = useRouter();
  const pathname = usePathname();
  useGlobalKeys();

  useEffect(() => {
    if (useSession.getState().status === 'unknown') void load();
  }, [load]);

  useEffect(() => {
    // Only user accounts have stored preferences; a token principal keeps the browser's, and asking
    // for them would only add a denied call to the audit log on every load.
    if (status === 'authenticated' && useSession.getState().me?.principal.type === 'user')
      void pullPreferences();
    if (status === 'anonymous') {
      const next = pathname + window.location.search;
      router.replace(`/login/${next && next !== '/' ? `?next=${encodeURIComponent(next)}` : ''}`);
    }
  }, [status, pullPreferences, router, pathname]);

  if (status !== 'authenticated' && status !== 'expired') return <Splash />;

  return (
    <>
      <UrlSync />
      <RecentTracker />
      <ShellFacts />
      <a
        href="#main"
        className="sr-only focus:not-sr-only focus:fixed focus:start-2 focus:top-2 focus:z-[70] focus:rounded-sm focus:bg-raised focus:px-3 focus:py-2"
      >
        {translate('Skip to main content')}
      </a>
      <div className="grid h-dvh grid-cols-[minmax(0,1fr)] grid-rows-[48px_minmax(0,1fr)] bg-canvas lg:grid-cols-[var(--spacing-sidebar)_minmax(0,1fr)]">
        <Topbar />
        <Sidebar />
        <MainArea>
          <Suspense fallback={null}>
            <GatewayNotices />
            <PageAccess>{children}</PageAccess>
          </Suspense>
        </MainArea>
      </div>
      <CommandPalette />
      <ShortcutsDialog />
      <AccountDialog />
      <SettingsDialog />
      <SessionExpiredDialog />
      <ChangeControlHost />
      <Toaster />
    </>
  );
}
