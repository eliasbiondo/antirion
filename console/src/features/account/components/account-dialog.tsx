'use client';

import { useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { Dialog } from '@/components/ui/overlay';
import { cn } from '@/lib/cn';
import { useSession } from '@/stores/session';

import { hasAccount } from '../describe';
import { useMessages } from '../i18n';
import { NotificationsSection } from './notifications-section';
import { PreferencesSection } from './preferences-section';
import { ProfileSection, TokenPrincipalNote } from './profile-section';
import { SecuritySection } from './security-section';
import { TokensSection } from './tokens-section';

const SECTIONS = [
  { id: 'profile', account: false },
  { id: 'preferences', account: false },
  { id: 'security', account: false },
  { id: 'tokens', account: true },
  { id: 'notifications', account: true },
] as const;

type SectionId = (typeof SECTIONS)[number]['id'];

function isSection(value: string | null): value is SectionId {
  return SECTIONS.some((s) => s.id === value);
}

/**
 * The account dialog, opened from the user menu; its section is in the URL (`?account=`). Signed in
 * with an admin token there is no account, so only what applies is offered: the profile explains
 * why, preferences stay in this browser, and security lists the sessions.
 */
export function AccountDialog() {
  const [section, setSection] = useQueryParam('account');
  const [dirty, setDirty] = useState(false);
  const me = useSession((s) => s.me);
  const t = useMessages().account;
  const account = hasAccount(me);
  const open = isSection(section);
  const active: SectionId = isSection(section) ? section : 'profile';
  const visible = SECTIONS.filter((s) => account || !s.account);
  const unavailable = !account && SECTIONS.some((s) => s.id === active && s.account);
  return (
    <Dialog
      open={open}
      onClose={() => {
        setDirty(false);
        setSection(null);
      }}
      title={t.title}
      size="lg"
    >
      <div className="grid grid-cols-1 gap-5 md:min-h-[420px] md:grid-cols-[168px_minmax(0,1fr)] md:gap-6">
        <nav aria-label={t.sectionsLabel} className="min-w-0">
          <ul className="-mx-1 flex gap-1 overflow-x-auto px-1 pb-1 md:mx-0 md:flex-col md:gap-px md:overflow-visible md:px-0 md:pb-0">
            {visible.map((s) => (
              <li key={s.id} className="shrink-0">
                <button
                  type="button"
                  aria-current={active === s.id ? 'page' : undefined}
                  onClick={() => setSection(s.id)}
                  className={cn(
                    'w-full rounded-sm px-2.5 py-1.5 text-left text-ui whitespace-nowrap',
                    active === s.id
                      ? 'bg-accent-dim font-semibold text-fg'
                      : 'text-muted hover:bg-hover hover:text-fg',
                  )}
                >
                  {t.sections[s.id]}
                </button>
              </li>
            ))}
          </ul>
          {dirty ? <p className="mt-3 hidden text-caption text-warn md:block">{t.unsaved}</p> : null}
        </nav>
        <div className="min-w-0">
          {unavailable ? (
            <TokenPrincipalNote what={active === 'tokens' ? 'tokens' : 'notifications'} />
          ) : active === 'profile' ? (
            <ProfileSection onDirty={setDirty} />
          ) : active === 'preferences' ? (
            <PreferencesSection />
          ) : active === 'security' ? (
            <SecuritySection />
          ) : active === 'tokens' ? (
            <TokensSection />
          ) : (
            <NotificationsSection />
          )}
        </div>
      </div>
    </Dialog>
  );
}
