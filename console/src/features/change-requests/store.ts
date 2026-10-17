import { create } from 'zustand';

import { type ChangeRequest, listAll } from '@/lib/api/client';
import { emptySlot, loadSlot, type Slot } from '@/lib/slot';
import { useShell } from '@/stores/shell';

import { changeRequestsApi } from './api';

interface ChangeRequestsState {
  list: Slot<ChangeRequest[]>;
  load: (options?: { quiet?: boolean }) => Promise<void>;
  approve: (id: string) => Promise<void>;
  reject: (id: string, reason?: string) => Promise<void>;
  apply: (id: string) => Promise<void>;
  cancel: (id: string, reason?: string) => Promise<void>;
}

/** Change requests in every state; the screen filters them in the page. */
export const useChangeRequests = create<ChangeRequestsState>()((set, get) => {
  const replace = (updated: ChangeRequest) => {
    set((s) => ({
      list: {
        ...s.list,
        data: (s.list.data ?? []).map((cr) => (cr.id === updated.id ? updated : cr)),
      },
    }));
    void useShell.getState().loadPending();
  };
  return {
    list: emptySlot(),
    load: (options) =>
      loadSlot(
        set,
        get,
        'list',
        () => listAll((cursor) => changeRequestsApi.list({ ...(cursor ? { cursor } : {}) })),
        options,
      ),
    async approve(id) {
      replace(await changeRequestsApi.approve(id));
    },
    async reject(id, reason) {
      replace(await changeRequestsApi.reject(id, reason));
    },
    async apply(id) {
      replace(await changeRequestsApi.apply(id));
    },
    async cancel(id, reason) {
      replace(await changeRequestsApi.cancel(id, reason));
    },
  };
});
