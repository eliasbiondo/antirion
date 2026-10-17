import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { server } from '@/mocks/node';

import { api, ApiError, describeError, isChangeControl, setChangeControlHandler } from '../api/client';

const HELD = {
  id: 'cr_1',
  kind: 'action',
  title: 'Run cache_flush_all',
  author: 'alice',
  created_at: '2026-10-14T10:00:00Z',
  base_generation: 3,
  changes: [{ method: 'POST', path: '/cache:invalidate', body: { level: 'gateway' } }],
  diff: [],
  state: 'open',
  approvals: [],
  approvals_required: 1,
  expires_at: '2026-10-14T11:00:00Z',
  applied_generation: null,
};

afterEach(() => setChangeControlHandler(null));

describe('change control in the client', () => {
  it('turns a held action (202) into an approval request, never a success', async () => {
    const handler = vi.fn();
    setChangeControlHandler(handler);
    server.use(http.post('*/admin/v1/cache:invalidate', () => HttpResponse.json(HELD, { status: 202 })));
    const error = await api
      .post('/cache:invalidate', { body: { level: 'gateway' } })
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    const e = error as ApiError;
    expect(e.approvalRequested).toBe(true);
    expect(e.changeRequest?.id).toBe('cr_1');
    expect(isChangeControl(e)).toBe(true);
    expect(describeError(e)).toMatch(/Approval requested/);
    expect(handler).toHaveBeenCalledWith({ kind: 'held', request: HELD });
  });

  it('keeps the refused write on a 409 change_request_required, with its query and body', async () => {
    const handler = vi.fn();
    setChangeControlHandler(handler);
    server.use(
      http.patch('*/admin/v1/routes/default', () =>
        HttpResponse.json(
          { error: { code: 'change_request_required', message: 'under change control' } },
          { status: 409 },
        ),
      ),
    );
    const body = { spec: { type: 'conditional' } };
    const error = (await api
      .patch('/routes/{name}', { path: { name: 'default' }, body })
      .catch((e: unknown) => e)) as ApiError;
    expect(error.changeRequestRequired).toBe(true);
    expect(error.attempt).toEqual({ method: 'PATCH', path: '/routes/default', body });
    expect(handler).toHaveBeenCalledWith({ kind: 'required', attempt: error.attempt });
    expect(describeError(error)).toMatch(/reviewer/);
  });

  it('never offers a change request for the change request calls themselves', async () => {
    const handler = vi.fn();
    setChangeControlHandler(handler);
    server.use(
      http.post('*/admin/v1/change_requests/:id', () =>
        HttpResponse.json(
          { error: { code: 'change_request_required', message: 'needs approvals' } },
          { status: 409 },
        ),
      ),
    );
    const error = (await api
      .post('/change_requests/{change_request_id}:apply', { path: { change_request_id: 'cr_1' } })
      .catch((e: unknown) => e)) as ApiError;
    expect(error.status).toBe(409);
    expect(error.attempt).toBeNull();
    expect(handler).not.toHaveBeenCalled();
  });

  it('returns other 202 answers as data (an export job)', async () => {
    server.use(
      http.post('*/admin/v1/organizations/acme:export', () =>
        HttpResponse.json(
          { id: 'job_1', kind: 'organization_export', state: 'running', progress: 0 },
          { status: 202 },
        ),
      ),
    );
    const job = await api.post('/organizations/{name}:export', { path: { name: 'acme' }, body: {} });
    expect((job as { id: string }).id).toBe('job_1');
  });
});
