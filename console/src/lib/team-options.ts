'use client';

import { useEffect, useState } from 'react';

import { api } from '@/lib/api/client';

import type { SelectOption } from '@/components/ui/select';

/**
 * The teams, as options for a field that picks some (a model's access list). Empty when the
 * principal may not list teams: the field still takes typed names.
 */
export function useTeamOptions(enabled = true): SelectOption[] {
  const [options, setOptions] = useState<SelectOption[]>([]);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    api.get('/teams', { query: { limit: 500 } }).then(
      (page) => {
        if (live)
          setOptions(
            page.items.map((t) => ({
              value: t.name,
              label: t.name,
              ...(t.spec.display_name && t.spec.display_name !== t.name
                ? { description: t.spec.display_name }
                : {}),
            })),
          );
      },
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, [enabled]);
  return options;
}
