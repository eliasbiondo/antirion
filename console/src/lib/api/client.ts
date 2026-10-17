import type { components, paths } from './schema';

/**
 * A thin, typed `fetch` wrapper over the Admin API contract. Paths, parameters, bodies and
 * responses come from the generated `schema.d.ts`, so a call that is not in the contract does not
 * compile. The console authenticates with the session cookie (HttpOnly, same origin); it never
 * holds a token after sign-in.
 */

export const API_BASE = process.env.NEXT_PUBLIC_ADMIN_API_BASE ?? '/admin/v1';

export type HttpMethod = 'get' | 'put' | 'post' | 'patch' | 'delete';

export type PathsWith<M extends HttpMethod> = {
  [P in keyof paths]: paths[P] extends Record<M, infer O> ? ([O] extends [never] ? never : P) : never;
}[keyof paths];

export type Operation<P extends keyof paths, M extends HttpMethod> =
  paths[P] extends Record<M, infer O> ? O : never;

type Params<O> = O extends { parameters: infer X } ? X : Record<string, never>;
type PathParamsOf<O> =
  Params<O> extends { path: infer X } ? ([X] extends [never] ? undefined : X) : undefined;
type QueryOf<O> = Params<O> extends { query?: infer X } ? ([X] extends [never] ? undefined : X) : undefined;

type JsonBody<O> = O extends { requestBody: { content: { 'application/json': infer B } } }
  ? B
  : O extends { requestBody?: { content: { 'application/json': infer B } } }
    ? B
    : O extends { requestBody: { content: { 'application/merge-patch+json': infer B } } }
      ? B
      : undefined;

type SuccessCode = 200 | 201 | 202;
type JsonOf<R> = R extends { content: { 'application/json': infer B } } ? B : undefined;
type Held = components['responses']['HeldForApproval'];
/** The body of a success answer; a call the two-person rule holds is thrown, never returned. */
export type SuccessData<O> = O extends { responses: infer R }
  ? {
      [C in keyof R]: C extends SuccessCode
        ? C extends 202
          ? [R[C]] extends [Held]
            ? never
            : JsonOf<R[C]>
          : JsonOf<R[C]>
        : C extends 204
          ? undefined
          : never;
    }[keyof R]
  : never;

export type ChangeRequest = components['schemas']['ChangeRequest'];

/** A write as it was sent, so a change request can carry it when change control refuses it. */
export interface AttemptedChange {
  method: 'POST' | 'PUT' | 'PATCH' | 'DELETE';
  /** The Admin API path with its query, as `/routes/default`. */
  path: string;
  body?: unknown;
}

export interface RequestOptions<O> {
  path?: PathParamsOf<O>;
  query?: QueryOf<O>;
  body?: JsonBody<O>;
  /** Sent as `If-Match` so an edit fails with 412 when the resource changed meanwhile. */
  ifMatch?: string | undefined;
  /** Sent as `x-antirion-change-reason`, recorded in the audit entry. */
  reason?: string | undefined;
  signal?: AbortSignal | undefined;
  /** Answers listed here are returned instead of thrown (for example 404 on an optional read). */
  accept?: number[];
  /**
   * Members the gateway reads although the contract declares no body for the operation (the
   * sign-in challenge of the enrolment calls). Merged into the JSON body.
   */
  undeclared?: Record<string, unknown>;
}

export interface ApiResponse<T> {
  data: T;
  status: number;
  etag: string | null;
  requestId: string | null;
}

export type ValidationDetail = components['schemas']['ValidationDetail'];

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId: string | null;
  readonly details: ValidationDetail[];
  readonly operation: string;
  /** The write change control refused (409 `change_request_required`). */
  readonly attempt: AttemptedChange | null;
  /** The change request the two-person rule opened instead of acting (202). */
  readonly changeRequest: ChangeRequest | null;

  constructor(init: {
    status: number;
    code: string;
    message: string;
    requestId: string | null;
    details?: ValidationDetail[];
    operation: string;
    attempt?: AttemptedChange | null;
    changeRequest?: ChangeRequest | null;
  }) {
    super(init.message);
    this.name = 'ApiError';
    this.status = init.status;
    this.code = init.code;
    this.requestId = init.requestId;
    this.details = init.details ?? [];
    this.operation = init.operation;
    this.attempt = init.attempt ?? null;
    this.changeRequest = init.changeRequest ?? null;
  }

  get notPermitted(): boolean {
    return this.status === 403;
  }

  get notFound(): boolean {
    return this.status === 404;
  }

  /** A write that the gateway's change control turned into a change request. */
  get changeRequestRequired(): boolean {
    return this.status === 409 && this.code === 'change_request_required';
  }

  /** A write the two-person rule held: it runs once someone else approves the change request. */
  get approvalRequested(): boolean {
    return this.status === 202 && this.changeRequest !== null;
  }

  get preconditionFailed(): boolean {
    return this.status === 412;
  }
}

/** Whether an error is change control at work (a change request to open or one opened), not a failure. */
export function isChangeControl(error: unknown): error is ApiError {
  return error instanceof ApiError && (error.changeRequestRequired || error.approvalRequested);
}

export type ChangeControlEvent =
  { kind: 'required'; attempt: AttemptedChange } | { kind: 'held'; request: ChangeRequest };

let onChangeControl: ((event: ChangeControlEvent) => void) | null = null;

/** The shell registers here to offer a change request (409) or show the one opened (202). */
export function setChangeControlHandler(handler: ((event: ChangeControlEvent) => void) | null): void {
  onChangeControl = handler;
}

let onWrite: ((path: string) => void) | null = null;

/** Told the Admin API path of every write that succeeded, so figures shown everywhere can follow. */
export function setWriteListener(listener: ((path: string) => void) | null): void {
  onWrite = listener;
}

function isChangeRequest(value: unknown): value is ChangeRequest {
  if (typeof value !== 'object' || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.id === 'string' &&
    (v.kind === 'config' || v.kind === 'action') &&
    typeof v.approvals_required === 'number' &&
    Array.isArray(v.changes)
  );
}

type UnauthenticatedHandler = () => void;
let onUnauthenticated: UnauthenticatedHandler | null = null;

/** The session store registers here so any 401 sends the operator back to sign-in. */
export function setUnauthenticatedHandler(handler: UnauthenticatedHandler | null): void {
  onUnauthenticated = handler;
}

function expandPath(template: string, params: Record<string, unknown> | undefined): string {
  return template.replace(/\{([^}]+)\}/g, (_, name: string) => {
    const value = params?.[name];
    if (value === undefined || value === null) throw new Error(`missing path parameter ${name}`);
    return encodeURIComponent(toParam(value));
  });
}

function toParam(value: unknown): string {
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint')
    return value.toString();
  return JSON.stringify(value);
}

function queryString(query: Record<string, unknown> | undefined): string {
  if (!query) return '';
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') continue;
    if (Array.isArray(value)) {
      for (const item of value as unknown[]) search.append(key, toParam(item));
    } else {
      search.append(key, toParam(value));
    }
  }
  const text = search.toString();
  return text ? `?${text}` : '';
}

interface ErrorBody {
  error?: { code?: string; message?: string; request_id?: string; details?: ValidationDetail[] };
}

async function parseError(response: Response, operation: string): Promise<ApiError> {
  let body: ErrorBody = {};
  try {
    body = (await response.json()) as ErrorBody;
  } catch {
    body = {};
  }
  return new ApiError({
    status: response.status,
    code: body.error?.code ?? `http_${response.status}`,
    message: body.error?.message ?? (response.statusText || 'The request failed.'),
    requestId: body.error?.request_id ?? response.headers.get('x-request-id'),
    details: body.error?.details ?? [],
    operation,
  });
}

/**
 * The error of a failed answer read outside `request` (a streamed call), with the same 401
 * handling: the session store sends the operator back to sign-in.
 */
export async function errorOf(response: Response, operation: string): Promise<ApiError> {
  const error = await parseError(response, operation);
  if (response.status === 401 && onUnauthenticated) onUnauthenticated();
  return error;
}

export async function request<M extends HttpMethod, P extends PathsWith<M>>(
  method: M,
  path: P,
  options: RequestOptions<Operation<P, M>> = {},
): Promise<ApiResponse<SuccessData<Operation<P, M>>>> {
  const adminPath =
    expandPath(path, options.path as Record<string, unknown> | undefined) +
    queryString(options.query as Record<string, unknown> | undefined);
  const url = API_BASE + adminPath;
  const headers: Record<string, string> = { accept: 'application/json' };
  const payload: unknown =
    options.undeclared === undefined
      ? options.body
      : { ...((options.body ?? {}) as Record<string, unknown>), ...options.undeclared };
  if (payload !== undefined) {
    headers['content-type'] = method === 'patch' ? 'application/merge-patch+json' : 'application/json';
  }
  if (options.ifMatch) headers['if-match'] = options.ifMatch;
  if (options.reason) headers['x-antirion-change-reason'] = options.reason;
  const operation = `${method.toUpperCase()} ${path}`;
  let response: Response;
  try {
    response = await fetch(url, {
      method: method.toUpperCase(),
      headers,
      body: payload === undefined ? null : JSON.stringify(payload),
      credentials: 'same-origin',
      cache: 'no-store',
      signal: options.signal ?? null,
    });
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === 'AbortError') throw cause;
    throw new ApiError({
      status: 0,
      code: 'network_error',
      message: 'The gateway could not be reached.',
      requestId: null,
      operation,
    });
  }
  const requestId = response.headers.get('x-request-id');
  const etag = response.headers.get('etag');
  if (!response.ok && !(options.accept ?? []).includes(response.status)) {
    let error = await parseError(response, operation);
    if (response.status === 401 && onUnauthenticated) onUnauthenticated();
    if (error.changeRequestRequired && method !== 'get' && !adminPath.startsWith('/change_requests')) {
      const attempt: AttemptedChange = {
        method: method.toUpperCase() as AttemptedChange['method'],
        path: adminPath,
        ...(payload === undefined ? {} : { body: payload }),
      };
      error = new ApiError({
        status: error.status,
        code: error.code,
        message: error.message,
        requestId: error.requestId,
        details: error.details,
        operation,
        attempt,
      });
      onChangeControl?.({ kind: 'required', attempt });
    }
    throw error;
  }
  let data: unknown;
  if (response.status === 204 || response.headers.get('content-length') === '0') {
    // Read to its end: a response whose body is never read is reported as cancelled once it is
    // dropped, though the call succeeded.
    await response.arrayBuffer().catch(() => undefined);
    data = undefined;
  } else {
    const type = response.headers.get('content-type') ?? '';
    data = type.includes('json') ? await response.json() : await response.text();
  }
  if (response.status === 202 && method !== 'get' && isChangeRequest(data)) {
    onChangeControl?.({ kind: 'held', request: data });
    throw new ApiError({
      status: 202,
      code: 'approval_requested',
      message: 'Approval requested. This runs once a second person approves it.',
      requestId,
      operation,
      changeRequest: data,
    });
  }
  if (method !== 'get') onWrite?.(adminPath);
  return { data: data as SuccessData<Operation<P, M>>, status: response.status, etag, requestId };
}

/** Convenience verbs; each returns the parsed body of a success answer. */
export const api = {
  get: async <P extends PathsWith<'get'>>(path: P, options?: RequestOptions<Operation<P, 'get'>>) =>
    (await request('get', path, options)).data,
  post: async <P extends PathsWith<'post'>>(path: P, options?: RequestOptions<Operation<P, 'post'>>) =>
    (await request('post', path, options)).data,
  put: async <P extends PathsWith<'put'>>(path: P, options?: RequestOptions<Operation<P, 'put'>>) =>
    (await request('put', path, options)).data,
  patch: async <P extends PathsWith<'patch'>>(path: P, options?: RequestOptions<Operation<P, 'patch'>>) =>
    (await request('patch', path, options)).data,
  delete: async <P extends PathsWith<'delete'>>(path: P, options?: RequestOptions<Operation<P, 'delete'>>) =>
    (await request('delete', path, options)).data,
};

/** Reads every page of a cursor-paged list. */
export async function listAll<T>(
  fetchPage: (cursor: string | undefined) => Promise<{ items: T[]; next_cursor?: string | null }>,
  maxPages = 50,
): Promise<T[]> {
  const out: T[] = [];
  let cursor: string | undefined;
  for (let page = 0; page < maxPages; page += 1) {
    const result = await fetchPage(cursor);
    out.push(...result.items);
    if (!result.next_cursor) break;
    cursor = result.next_cursor;
  }
  return out;
}

export function isApiError(value: unknown): value is ApiError {
  return value instanceof ApiError;
}

/** A user-facing message for any thrown value. */
export function describeError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 0) return error.message;
    if (error.approvalRequested) return error.message;
    if (error.changeRequestRequired)
      return 'This change needs a reviewer’s approval. Open a change request to submit it.';
    if (error.status === 403) return "You don't have permission to do this.";
    if (error.status === 404) return 'This item no longer exists. Refresh the page.';
    if (error.status === 412)
      return 'Someone else changed this while you were editing. Reload to see their changes, then try again.';
    if (error.details.length > 0) return error.details.map((d) => `${d.path}: ${d.message}`).join('; ');
    return error.message;
  }
  return error instanceof Error ? error.message : 'Something went wrong. Try again.';
}
