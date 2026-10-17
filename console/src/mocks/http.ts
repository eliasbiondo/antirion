import { http, HttpResponse, type HttpHandler } from 'msw';

import type { HttpMethod, Operation, PathsWith, SuccessData } from '@/lib/api/client';

export interface MockContext {
  params: Record<string, string>;
  query: URLSearchParams;
  body: unknown;
}

const VERBS = {
  get: http.get,
  put: http.put,
  post: http.post,
  patch: http.patch,
  delete: http.delete,
} as const;

function pattern(path: string): { regex: RegExp; names: string[] } {
  const names: string[] = [];
  const source = path
    .split(/(\{[^}]+\})/)
    .map((part) => {
      const m = /^\{([^}]+)\}$/.exec(part);
      if (m?.[1]) {
        names.push(m[1]);
        return '([^/]+)';
      }
      return part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    })
    .join('');
  return { regex: new RegExp(`/admin/v1${source}$`), names };
}

/**
 * A handler for one operation of the contract: the path and the answer are typed from the
 * generated schema, so a mock cannot drift from the API it stands in for.
 */
export function mock<M extends HttpMethod, P extends PathsWith<M>>(
  method: M,
  path: P,
  resolve: (ctx: MockContext) => SuccessData<Operation<P, M>> | Promise<SuccessData<Operation<P, M>>>,
  status = 200,
): HttpHandler {
  const { regex, names } = pattern(path);
  return VERBS[method](
    ({ request }) => regex.test(new URL(request.url).pathname),
    async ({ request }) => {
      const url = new URL(request.url);
      const match = regex.exec(url.pathname);
      const params = Object.fromEntries(names.map((n, i) => [n, decodeURIComponent(match?.[i + 1] ?? '')]));
      let body: unknown = undefined;
      if (method !== 'get' && method !== 'delete') {
        const text = await request.text();
        body = text ? (JSON.parse(text) as unknown) : undefined;
      }
      const data = await resolve({ params, query: url.searchParams, body });
      if (status === 204) return new HttpResponse(null, { status });
      return HttpResponse.json(data as object, {
        status,
        headers: { 'x-request-id': `mock-${Date.now().toString(36)}` },
      });
    },
  );
}
