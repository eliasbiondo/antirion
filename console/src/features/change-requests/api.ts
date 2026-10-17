import { api, type AttemptedChange, type ChangeRequest } from '@/lib/api/client';

export type ChangeRequestState = ChangeRequest['state'];

/** Waiting for a reviewer or for someone to apply it. */
export const PENDING: ChangeRequestState[] = ['open', 'approved'];

export const changeRequestsApi = {
  list: (query: { state?: ChangeRequestState[]; author?: string; cursor?: string; limit?: number }) =>
    api.get('/change_requests', {
      query: {
        limit: query.limit ?? 200,
        ...(query.state && query.state.length > 0 ? { state: query.state } : {}),
        ...(query.author ? { author: query.author } : {}),
        ...(query.cursor ? { cursor: query.cursor } : {}),
      },
    }),
  get: (id: string) => api.get('/change_requests/{change_request_id}', { path: { change_request_id: id } }),
  open: (title: string, description: string, change: AttemptedChange) =>
    api.post('/change_requests', {
      body: {
        title,
        ...(description ? { description } : {}),
        changes: [change.body === undefined ? { method: change.method, path: change.path } : change],
      },
    }),
  approve: (id: string) =>
    api.post('/change_requests/{change_request_id}:approve', { path: { change_request_id: id } }),
  reject: (id: string, reason?: string) =>
    api.post('/change_requests/{change_request_id}:reject', {
      path: { change_request_id: id },
      body: reason ? { reason } : {},
    }),
  apply: (id: string) =>
    api.post('/change_requests/{change_request_id}:apply', { path: { change_request_id: id } }),
  cancel: (id: string, reason?: string) =>
    api.post('/change_requests/{change_request_id}:cancel', {
      path: { change_request_id: id },
      body: reason ? { reason } : {},
    }),
};
