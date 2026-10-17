'use client';

import type { ReactNode } from 'react';

import { cn } from '@/lib/cn';

import { IconButton } from './button';
import { Icon } from './icon';
import { MenuButton, type MenuItem } from './menu';
import { Tooltip } from './tooltip';

export interface SavedView {
  id: string;
  name: string;
  /** Starred views are drawn with a filled star and listed first. */
  starred: boolean;
  /** Whether the list is showing this view now. */
  active?: boolean;
  /** Shared with the organization (a muted mark after the name). */
  shared?: boolean;
  /** A muted line under the name ("Shared by ada"). */
  note?: string | undefined;
  /** Whether the reader may star or unstar it (built-in views may not). */
  starrable?: boolean;
}

export interface SavedViewsProps {
  views: SavedView[];
  onSelect: (id: string) => void;
  /** Stars or unstars a view; without it the stars only show. */
  onToggleStar?: (id: string) => void;
  /** A view's own actions (rename, share, delete), in a "More" menu on its row. */
  menu?: (id: string) => MenuItem[];
  /** An action next to the list's title ("Save view"). */
  action?: ReactNode;
  label?: string;
  className?: string;
}

/**
 * The prototype's saved views: a short list of named filters with stars, starred first, the
 * active one highlighted; optionally with a star toggle and a menu per view.
 */
export function SavedViews({
  views,
  onSelect,
  onToggleStar,
  menu,
  action,
  label = 'Saved views',
  className,
}: SavedViewsProps) {
  const ordered = [...views.filter((v) => v.starred), ...views.filter((v) => !v.starred)];
  return (
    <nav aria-label={label} className={cn('px-2 py-2', className)}>
      <div className="flex min-h-6 items-center justify-between gap-2 px-2 pb-1">
        <span className="text-caption font-medium text-muted">{label}</span>
        {action}
      </div>
      <ul className="flex flex-col gap-px">
        {ordered.map((v) => {
          const items = menu?.(v.id) ?? [];
          const canStar = onToggleStar !== undefined && v.starrable !== false;
          return (
            <li
              key={v.id}
              className={cn(
                'group flex items-center gap-1 rounded-sm pr-1 transition-colors',
                v.active ? 'bg-accent-dim' : 'hover:bg-hover',
              )}
            >
              {canStar ? (
                <IconButton
                  icon={v.starred ? 'starFilled' : 'star'}
                  size={14}
                  label={v.starred ? `Unstar ${v.name}` : `Star ${v.name}`}
                  aria-pressed={v.starred}
                  className={cn(
                    'ml-0.5 h-6 w-6 shrink-0 hover:bg-transparent',
                    v.starred ? 'text-star hover:text-star' : v.active ? 'text-muted' : 'text-dim',
                  )}
                  onClick={() => onToggleStar(v.id)}
                />
              ) : null}
              <button
                type="button"
                aria-current={v.active ? 'true' : undefined}
                onClick={() => onSelect(v.id)}
                className={cn(
                  'flex min-h-[30px] min-w-0 flex-1 items-center gap-2 rounded-sm py-1 text-left text-ui',
                  canStar ? '-ml-[3px] pl-0' : 'px-2',
                  v.active ? 'font-medium text-fg' : 'text-fg',
                )}
              >
                {canStar ? null : (
                  <Icon
                    name={v.starred ? 'starFilled' : 'star'}
                    size={14}
                    className={cn('shrink-0', v.starred ? 'text-star' : v.active ? 'text-muted' : 'text-dim')}
                    label={v.starred ? 'Starred' : undefined}
                  />
                )}
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate">{v.name}</span>
                  {v.note ? (
                    <span className="truncate text-caption font-normal text-muted">{v.note}</span>
                  ) : null}
                </span>
                {v.shared ? (
                  <Tooltip content="Shared with the organization">
                    <span className="flex shrink-0">
                      <Icon name="users" size={12} className="text-dim" label="Shared" />
                    </span>
                  </Tooltip>
                ) : null}
              </button>
              {items.length > 0 ? (
                <span className="shrink-0 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 max-md:opacity-100 [&_button]:h-6 [&_button]:w-6">
                  <MenuButton items={items} iconOnly label={`Actions for ${v.name}`} />
                </span>
              ) : null}
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
