'use client';

import { useRouter } from 'next/navigation';
import { useEffect, useRef, useState } from 'react';

import { Icon, type IconName } from '@/components/ui/icon';
import { Avatar } from '@/components/ui/misc';
import { menuKeyHandler, Popover } from '@/components/ui/overlay';
import { capitalize } from '@/lib/format';
import { usePreferences } from '@/stores/preferences';
import { anySection } from '@/features/settings/components/sections';
import { navAllowed } from '@/lib/nav';
import { highestRole, navHolds, useSession } from '@/stores/session';
import { scopeOrganization, useScope } from '@/stores/scope';

import { useOpenSettings, useQueryParam } from './url-sync';

function principalName(me: ReturnType<typeof useSession.getState>['me']): {
  name: string;
  email: string | null;
  handle: string;
} {
  if (!me) return { name: 'Unknown', email: null, handle: '' };
  const user = me.user && typeof me.user === 'object' ? me.user : null;
  if (user) return { name: user.display_name || user.name, email: user.email ?? null, handle: user.name };
  if (me.principal.type === 'bootstrap') return { name: 'Bootstrap admin', email: null, handle: 'bootstrap' };
  return { name: me.principal.id, email: null, handle: me.principal.id };
}

function useRemaining(expiresAt: string | undefined): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  if (!expiresAt) return null;
  return Math.max(0, (new Date(expiresAt).getTime() - now) / 1000);
}

/** "12 h", "35 min": the time a session has left, rounded for a glance. */
function sessionLeft(seconds: number): string {
  if (seconds >= 2 * 3600) return `${Math.round(seconds / 3600)} h`;
  if (seconds >= 60) return `${Math.round(seconds / 60)} min`;
  return 'less than a minute';
}

/** The avatar menu: who is signed in, the session's remaining time, account sections, the gateway's
 * settings and sign out. The theme lives in Preferences and the top bar's toggle. */
export function AccountMenu() {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const me = useSession((s) => s.me);
  const signOut = useSession((s) => s.signOut);
  const scope = useScope((s) => s.scope);
  const router = useRouter();
  const [, setAccount] = useQueryParam('account');
  const openSettings = useOpenSettings();
  const who = principalName(me);
  const role = highestRole(me);
  const remaining = useRemaining(me?.session?.expires_at);
  const isUser = me?.principal.type === 'user';
  const photo = usePreferences((s) => (isUser ? s.photo : null));
  const holds = navHolds(me, scopeOrganization(scope));
  const canSettings = anySection((s) => navAllowed(s, holds));

  // A token principal has no profile, factors, personal tokens or notifications of its own.
  const items: { icon: IconName; label: string; section: string }[] = isUser
    ? [
        { icon: 'users', label: 'Profile and preferences', section: 'profile' },
        { icon: 'lock', label: 'Security', section: 'security' },
        { icon: 'key', label: 'Personal tokens', section: 'tokens' },
        { icon: 'bell', label: 'Notifications', section: 'notifications' },
      ]
    : [{ icon: 'sliders', label: 'Preferences', section: 'preferences' }];

  return (
    <div className="relative ml-1">
      <button
        ref={anchor}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`Account menu for ${who.name}`}
        onClick={() => setOpen(!open)}
        className="rounded-full border border-transparent p-0.5 hover:border-line hover:bg-hover aria-expanded:border-accent"
      >
        <Avatar name={who.name} size="sm" photo={photo} />
      </button>
      <Popover
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        align="right"
        label="Account"
        role="menu"
        className="w-[300px] p-1"
      >
        <div onKeyDown={menuKeyHandler}>
          <div className="mb-1 flex items-center gap-2.5 border-b border-line px-2.5 pt-2.5 pb-3">
            <Avatar name={who.name} size="md" photo={photo} />
            <div className="min-w-0">
              <div className="truncate text-body font-semibold text-fg">{who.name}</div>
              {who.email ? <div className="mono truncate text-caption text-muted">{who.email}</div> : null}
              <div className="mt-0.5 flex items-center gap-1.5">
                {/* A username that is the email is shown once, as the email above. */}
                {who.handle && who.handle !== who.email ? (
                  <span className="mono text-caption text-muted">@{who.handle}</span>
                ) : null}
                {role ? (
                  <span className="rounded-xs border border-[color-mix(in_oklab,var(--ant-accent)_30%,var(--ant-border))] bg-[color-mix(in_oklab,var(--ant-accent)_12%,transparent)] px-1.5 text-caption font-medium text-accent">
                    {capitalize(role.replaceAll('_', ' '))}
                  </span>
                ) : null}
              </div>
            </div>
          </div>
          <div className="px-2.5 pt-1.5 pb-1 text-caption font-medium text-dim">Account</div>
          {items.map((item) => (
            <button
              key={item.section}
              type="button"
              role="menuitem"
              onClick={() => {
                anchor.current?.focus();
                setOpen(false);
                setAccount(item.section);
              }}
              className="flex w-full items-center gap-2.5 rounded-sm px-2.5 py-1.5 text-left text-ui text-fg hover:bg-hover focus:bg-hover"
            >
              <span className="text-muted">
                <Icon name={item.icon} size={16} />
              </span>
              {item.label}
            </button>
          ))}
          <div className="my-1 border-t border-line" />
          {canSettings ? (
            <button
              type="button"
              role="menuitem"
              onClick={() => {
                // Focus returns to the avatar when the dialog closes.
                anchor.current?.focus();
                setOpen(false);
                openSettings();
              }}
              className="flex w-full items-center gap-2.5 rounded-sm px-2.5 py-1.5 text-left text-ui text-fg hover:bg-hover focus:bg-hover"
            >
              <span className="text-muted">
                <Icon name="settings" size={16} />
              </span>
              Gateway settings
            </button>
          ) : null}
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false);
              void signOut().finally(() => router.replace('/login/'));
            }}
            className="flex w-full items-center gap-2.5 rounded-sm px-2.5 py-1.5 text-left text-ui text-fg hover:bg-hover focus:bg-hover"
          >
            <span className="text-muted">
              <Icon name="logout" size={16} />
            </span>
            Sign out
          </button>
          <div className="flex justify-between gap-2 border-t border-line px-2.5 pt-2 pb-1.5 text-caption text-muted">
            <span className="truncate">{scope.kind === 'gateway' ? 'All organizations' : scope.name}</span>
            <span className="whitespace-nowrap">
              {remaining === null ? 'Signed in with a token' : `Session ends in ${sessionLeft(remaining)}`}
            </span>
          </div>
        </div>
      </Popover>
    </div>
  );
}
