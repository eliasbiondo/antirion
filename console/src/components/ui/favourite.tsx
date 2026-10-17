'use client';

import { useMemo } from 'react';

import { cn } from '@/lib/cn';
import { useUrlFilters } from '@/lib/url-filters';
import { type FavouriteKind, useIsFavourite, usePreferences } from '@/stores/preferences';

import { IconButton } from './button';
import { ChipToggle } from './form';

/** The names a user starred of one kind, kept up to date. */
export function useFavouriteNames(kind: FavouriteKind): Set<string> {
  const favourites = usePreferences((s) => s.favourites);
  return useMemo(
    () => new Set(favourites.filter((f) => f.kind === kind).map((f) => f.name)),
    [favourites, kind],
  );
}

/**
 * The star that adds an item to the user's favourites (sidebar, search and the list's Starred
 * filter) or takes it out. `label` names the item in its accessible name ("Star key support-bot").
 */
export function FavouriteButton({
  kind,
  name,
  label,
  className,
}: {
  kind: FavouriteKind;
  name: string;
  label: string;
  className?: string;
}) {
  const starred = useIsFavourite(kind, name);
  const toggle = usePreferences((s) => s.toggleFavourite);
  return (
    <IconButton
      icon={starred ? 'starFilled' : 'star'}
      label={starred ? `Remove ${label} from favourites` : `Add ${label} to favourites`}
      aria-pressed={starred}
      onClick={(e) => {
        e.stopPropagation();
        toggle({ kind, name });
      }}
      className={cn(starred && 'text-star hover:text-star', className)}
    />
  );
}

/** A filter chip that narrows a list to the starred items of its kind; hidden while none is starred. */
export function StarredFilter({
  kind,
  pressed,
  onChange,
}: {
  kind: FavouriteKind;
  pressed: boolean;
  onChange: (pressed: boolean) => void;
}) {
  const names = useFavouriteNames(kind);
  if (names.size === 0 && !pressed) return null;
  return (
    <ChipToggle pressed={pressed} onChange={onChange} count={names.size}>
      Starred
    </ChipToggle>
  );
}

const STARRED = { starred: false };

export interface StarredFilterState {
  on: boolean;
  set: (on: boolean) => void;
  /** The items to show: every one, or only the starred ones while the filter is on. */
  apply: <T>(items: T[], nameOf: (item: T) => string) => T[];
}

/** A list's "Starred" filter, kept in the URL (`starred=1`) like the list's other filters. */
export function useStarredFilter(kind: FavouriteKind): StarredFilterState {
  const names = useFavouriteNames(kind);
  const { filters, set } = useUrlFilters(STARRED);
  const on = filters.starred;
  return {
    on,
    set: (value) => set({ starred: value }),
    apply: (items, nameOf) => (on ? items.filter((i) => names.has(nameOf(i))) : items),
  };
}
