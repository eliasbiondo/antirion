import { http, HttpResponse } from 'msw';
import { describe, expect, it, vi } from 'vitest';

import { server } from '@/mocks/node';

import { api, ApiError, request, setUnauthenticatedHandler } from '../api/client';

describe('the Admin API client', () => {
  it('sends the change reason, If-Match and a merge-patch body', async () => {
    let seen: Headers | null = null;
    let body: unknown = null;
    server.use(
      http.patch('*/admin/v1/keys/ci', async ({ request: r }) => {
        seen = r.headers;
        body = await r.json();
        return HttpResponse.json({
          name: 'ci',
          spec: {},
          metadata: { etag: '"2"', generation: 2, created_at: '', updated_at: '', managed_by: 'api' },
        });
      }),
    );
    await api.patch('/keys/{name}', {
      path: { name: 'ci' },
      body: { spec: { enabled: false } },
      ifMatch: '"1"',
      reason: 'rotate',
    });
    expect(seen).not.toBeNull();
    const headers = seen as unknown as Headers;
    expect(headers.get('if-match')).toBe('"1"');
    expect(headers.get('x-antirion-change-reason')).toBe('rotate');
    expect(headers.get('content-type')).toBe('application/merge-patch+json');
    expect(body).toEqual({ spec: { enabled: false } });
  });

  it('throws the error envelope as an ApiError with the request id', async () => {
    server.use(
      http.get('*/admin/v1/keys/missing', () =>
        HttpResponse.json(
          { error: { code: 'not_found', message: 'The resource does not exist.', request_id: 'req-1' } },
          { status: 404 },
        ),
      ),
    );
    const error = await api.get('/keys/{name}', { path: { name: 'missing' } }).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).notFound).toBe(true);
    expect((error as ApiError).requestId).toBe('req-1');
  });

  it('calls the unauthenticated handler on 401', async () => {
    const handler = vi.fn();
    setUnauthenticatedHandler(handler);
    server.use(
      http.get('*/admin/v1/status', () =>
        HttpResponse.json(
          { error: { code: 'unauthenticated', message: 'no', request_id: 'x' } },
          { status: 401 },
        ),
      ),
    );
    await expect(api.get('/status')).rejects.toBeInstanceOf(ApiError);
    expect(handler).toHaveBeenCalledOnce();
    setUnauthenticatedHandler(null);
  });

  it('encodes arrays as repeated parameters and returns the ETag', async () => {
    let url = '';
    server.use(
      http.get('*/admin/v1/analytics/usage', ({ request: r }) => {
        url = r.url;
        return HttpResponse.json({ items: [], next_cursor: null }, { headers: { etag: '"g7"' } });
      }),
    );
    const r = await request('get', '/analytics/usage', { query: { group_by: ['model', 'team'], from: 'a' } });
    expect(new URL(url).searchParams.getAll('group_by')).toEqual(['model', 'team']);
    expect(r.etag).toBe('"g7"');
  });

  it('reports an unreachable gateway as a network error', async () => {
    server.use(http.get('*/admin/v1/health', () => HttpResponse.error()));
    const error = await api.get('/health').catch((e: unknown) => e);
    expect((error as ApiError).code).toBe('network_error');
  });
});
