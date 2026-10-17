'use client';

import { usePathname, useRouter } from 'next/navigation';
import { useEffect, useRef, useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';
import { IconButton } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Select } from '@/components/ui/form';
import { Icon } from '@/components/ui/icon';
import { Portal, useBackdropClose, useFocusTrap } from '@/components/ui/overlay';
import { cn } from '@/lib/cn';
import { useMediaQuery } from '@/lib/hooks';
import { Forbidden } from '@/components/ui/gate';
import { navAllowed, SETTINGS_PARAM } from '@/lib/nav';
import { scopeOrganization, useScope } from '@/stores/scope';
import { navHolds, useSession } from '@/stores/session';

import { hasUnsaved, useUnsaved } from '../unsaved';
import { AlertEditorsHost } from './alert-editors';
import { findSection, MOVED_SECTIONS, type Section, SETTINGS_GROUPS, settingsScopeWords } from './sections';

/** The groups with only the sections the principal may read; a group left empty is dropped. */
function useReadableGroups() {
  const me = useSession((s) => s.me);
  const organization = scopeOrganization(useScope((s) => s.scope));
  const holds = navHolds(me, organization);
  const allowed = (s: Section) => navAllowed(s, holds);
  const groups = SETTINGS_GROUPS.map((g) => ({ ...g, sections: g.sections.filter(allowed) })).filter(
    (g) => g.sections.length > 0,
  );
  return { groups, allowed };
}

function SectionNav({
  current,
  groups,
  onSelect,
}: {
  current: Section | null;
  groups: typeof SETTINGS_GROUPS;
  onSelect: (id: string) => void;
}) {
  return (
    <nav aria-label="Settings sections" className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
      {groups.map((g) => (
        <div key={g.label || 'last'} className="pb-1.5">
          {g.label ? (
            <h3 className="mx-2.5 mt-3 mb-1 text-caption font-medium text-dim">{g.label}</h3>
          ) : (
            <div className="mx-2.5 my-2.5 border-t border-line" />
          )}
          <ul className="flex flex-col gap-px">
            {g.sections.map((s) => {
              const on = s.id === current?.id;
              return (
                <li key={s.id}>
                  <button
                    type="button"
                    aria-current={on ? 'page' : undefined}
                    data-autofocus={on ? true : undefined}
                    onClick={() => onSelect(s.id)}
                    className={cn(
                      'flex h-[30px] w-full items-center gap-2.5 rounded-sm px-2.5 text-left text-body transition-colors',
                      on ? 'bg-accent-dim font-medium text-fg' : 'text-muted hover:bg-hover hover:text-fg',
                      s.id === 'danger' && !on && 'text-error',
                    )}
                  >
                    <span className={on ? 'text-accent' : s.id === 'danger' ? 'text-error' : 'text-muted'}>
                      <Icon name={s.icon} size={16} />
                    </span>
                    {s.label}
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}

/**
 * Settings as a large dialog over the page, the section list on the left and the section on the
 * right (full screen on phones, with the sections in a select). It opens from the sidebar, the
 * account menu, the palette and `?settings=<section>`; edits not saved yet are confirmed before
 * the dialog closes or another section opens.
 */
export function SettingsDialog() {
  const [param, setParam] = useQueryParam(SETTINGS_PARAM);
  const router = useRouter();
  const pathname = usePathname();
  const scope = useScope((s) => s.scope);
  const phone = useMediaQuery('(max-width: 767px)');
  const ref = useRef<HTMLDivElement>(null);
  const [pending, setPending] = useState<{ next: string | null } | null>(null);
  const unsaved = useUnsaved((s) => Object.keys(s.dirty).length > 0);
  const moved = param !== null ? MOVED_SECTIONS[param] : undefined;
  const open = param !== null && moved === undefined;
  const { groups, allowed } = useReadableGroups();
  const section = findSection(param, allowed);

  // An old link to a section that became a page (Providers) opens that page instead.
  useEffect(() => {
    if (!moved) return;
    const search = new URLSearchParams(window.location.search);
    search.delete(SETTINGS_PARAM);
    const query = search.toString();
    router.replace(query ? `${moved}?${query}` : moved);
  }, [moved, router, pathname]);

  /** Goes to `next` (a section, or null to close), asking first when edits would be lost. */
  const go = (next: string | null) => {
    if (next === section?.id) return;
    if (hasUnsaved()) setPending({ next });
    else setParam(next);
  };

  const isTop = useFocusTrap(ref, open, () => go(null));
  const backdrop = useBackdropClose(isTop, () => go(null));

  if (!open) return null;

  const title = 'Gateway settings';
  const forWhom = section ? settingsScopeWords(section.id, scope) : '';

  return (
    <Portal>
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-overlay md:p-6">
        <div className="fixed inset-0" aria-hidden="true" {...backdrop} />
        <div
          ref={ref}
          role="dialog"
          aria-modal="true"
          aria-labelledby="settings-dialog-title"
          aria-describedby="settings-dialog-scope"
          tabIndex={-1}
          className={cn(
            'relative flex animate-fade-in overflow-hidden bg-raised outline-none',
            phone
              ? 'h-dvh w-full flex-col'
              : 'h-[min(860px,calc(100dvh-48px))] w-[min(1180px,calc(100vw-48px))] rounded-container border border-line-strong shadow-pop',
          )}
        >
          {phone ? (
            <div className="flex shrink-0 flex-col gap-3 border-b border-line px-gutter-sm pt-3 pb-3">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <h2 id="settings-dialog-title" className="text-title font-semibold text-fg">
                    {title}
                  </h2>
                  <p id="settings-dialog-scope" className="text-caption text-muted">
                    {forWhom}
                  </p>
                </div>
                <IconButton icon="x" label="Close settings" onClick={() => go(null)} />
              </div>
              <Select
                aria-label="Section"
                data-autofocus
                value={section?.id ?? ''}
                onChange={(v) => go(v)}
                options={groups.flatMap((g) =>
                  g.sections.map((s) => ({
                    value: s.id,
                    label: g.label ? `${g.label}: ${s.label}` : s.label,
                  })),
                )}
              />
            </div>
          ) : (
            <aside className="flex w-[232px] shrink-0 flex-col border-r border-line bg-canvas">
              <div className="shrink-0 px-4.5 pt-4 pb-1">
                <h2 id="settings-dialog-title" className="text-title font-semibold text-fg">
                  {title}
                </h2>
                <p id="settings-dialog-scope" className="text-caption text-muted">
                  {forWhom}
                </p>
              </div>
              <SectionNav current={section} groups={groups} onSelect={go} />
              {unsaved ? (
                <p className="flex shrink-0 items-center gap-1.5 border-t border-line px-4.5 py-2.5 text-caption text-warn">
                  <Icon name="alertCircle" size={14} />
                  Unsaved changes
                </p>
              ) : null}
            </aside>
          )}
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            <header className="flex shrink-0 items-start justify-between gap-4 border-b border-line px-6 py-3.5 max-md:px-gutter-sm">
              <div className="min-w-0">
                <h3 className="text-page font-semibold tracking-[-0.01em] text-fg">
                  {section?.label ?? title}
                </h3>
                {section?.description ? (
                  <p className="mt-0.5 text-ui text-muted">{section.description}</p>
                ) : null}
              </div>
              {phone ? null : <IconButton icon="x" label="Close settings" onClick={() => go(null)} />}
            </header>
            <div data-settings-body className="min-h-0 flex-1 overflow-y-auto overscroll-contain bg-canvas">
              {section ? (
                <div className="flex w-full flex-col">{section.render()}</div>
              ) : (
                <div className="flex h-full items-center justify-center">
                  <Forbidden what="gateway settings" />
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
      <ConfirmDialog
        open={pending !== null}
        onClose={() => setPending(null)}
        title="Discard changes?"
        effect={
          pending?.next === null
            ? `Your edits in ${section?.label ?? title} aren't saved. Closing Settings discards them.`
            : `Your edits in ${section?.label ?? title} aren't saved. Opening another section discards them.`
        }
        confirmLabel="Discard"
        cancelLabel="Keep editing"
        withReason={false}
        onConfirm={async () => {
          const next = pending?.next ?? null;
          useUnsaved.setState({ dirty: {} });
          setPending(null);
          setParam(next);
          await Promise.resolve();
        }}
      />
      <AlertEditorsHost />
    </Portal>
  );
}
