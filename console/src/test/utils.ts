import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import type { Me } from '@/lib/api/types';
import { useScope } from '@/stores/scope';
import { useSession } from '@/stores/session';

/** Signs a principal in for a test, with the given permissions at gateway scope. */
export function signInAs(permissions: string[], scope = 'gateway'): Me {
  const me: Me = {
    principal: { type: 'user', id: 'tester', authenticated_by: 'session' },
    bindings: [],
    permissions: { [scope]: permissions },
    session: null,
  };
  useSession.setState({ status: 'authenticated', me });
  useScope.setState({
    scope:
      scope === 'gateway'
        ? { kind: 'gateway' }
        : { kind: 'organization', name: scope.split('/')[1] ?? 'default' },
    environment: null,
  });
  return me;
}

export const ALL_KEY_PERMISSIONS = [
  'keys:read',
  'keys:create',
  'keys:update',
  'keys:delete',
  'keys:rotate',
  'keys:grant_permissions',
];

/**
 * Picks an option of a console Select, Combobox or MultiSelect: opens the list from its trigger,
 * then clicks the option (the user-event equivalent of `selectOptions` on a native select).
 */
export async function pickOption(trigger: HTMLElement, option: string | RegExp): Promise<void> {
  await userEvent.click(trigger);
  const list = await screen.findByRole('listbox');
  await userEvent.click(within(list).getByRole('option', { name: option }));
}
