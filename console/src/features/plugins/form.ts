import type { FieldErrors } from '@/lib/validation';

import {
  durationError,
  headerErrors,
  headerMap,
  headerRows,
  type HeaderRow,
  put,
  REDACTED,
  urlError,
} from '@/features/mcp/fields';
import { parseDuration } from '@/lib/format';

import type { PluginResource, PluginRuntime, PluginSpec } from './api';

export type PluginType = PluginSpec['type'];
export type Hook = NonNullable<PluginSpec['hooks']>[number];

export const TYPE_LABEL: Record<PluginType, string> = {
  webhook: 'Webhook',
  builtin: 'Built-in',
  wasm: 'WebAssembly',
};

export const HOOK_LABEL: Record<Hook, string> = {
  request: 'Before the request',
  response: 'After the response',
  stream_chunk: 'Each streamed chunk',
  attempt: 'Each attempt',
};

/** The longest call timeout the gateway accepts for a plugin. */
export const MAX_TIMEOUT_S = 30;

/** Plugin names: the generic resource name. */
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;

export interface PluginForm {
  name: string;
  type: PluginType;
  enabled: boolean;
  hooks: Hook[];
  order: string;
  onError: 'fail_closed' | 'fail_open';
  timeout: string;
  includeBody: boolean;
  config: string;
  url: string;
  headers: HeaderRow[];
  signingSecret: string;
  signingSecretStored: boolean;
  maxResponse: string;
  forwardHeaders: string[];
  builtin: string;
  module: string;
  sha256: string;
  maxMemory: string;
  maxDuration: string;
}

export const EMPTY_PLUGIN: PluginForm = {
  name: '',
  type: 'webhook',
  enabled: true,
  hooks: ['request'],
  order: '',
  onError: 'fail_closed',
  timeout: '',
  includeBody: true,
  config: '',
  url: '',
  headers: [],
  signingSecret: '',
  signingSecretStored: false,
  maxResponse: '',
  forwardHeaders: [],
  builtin: '',
  module: '',
  sha256: '',
  maxMemory: '',
  maxDuration: '',
};

export function pluginForm(r: PluginResource): PluginForm {
  const s = r.spec;
  return {
    name: r.name,
    type: s.type,
    enabled: s.enabled !== false,
    hooks: s.hooks ?? [],
    order: s.order === undefined ? '' : String(s.order),
    onError: s.on_error ?? 'fail_closed',
    timeout: s.timeout ?? '',
    includeBody: s.include_body !== false,
    config: s.config ? JSON.stringify(s.config, null, 2) : '',
    url: s.webhook?.url ?? '',
    headers: headerRows(s.webhook?.headers),
    signingSecret: '',
    signingSecretStored: Boolean(s.webhook?.signing_secret),
    maxResponse: s.webhook?.max_response_bytes ?? '',
    forwardHeaders: s.webhook?.forward_headers ?? [],
    builtin: s.builtin?.name ?? '',
    module: s.wasm?.module ?? '',
    sha256: s.wasm?.sha256 ?? '',
    maxMemory: s.wasm?.max_memory ?? '',
    maxDuration: s.wasm?.max_duration ?? '',
  };
}

export function parseConfig(
  text: string,
): { ok: true; value: Record<string, unknown> | undefined } | { ok: false } {
  if (!text.trim()) return { ok: true, value: undefined };
  try {
    const v: unknown = JSON.parse(text);
    return v !== null && typeof v === 'object' && !Array.isArray(v)
      ? { ok: true, value: v as Record<string, unknown> }
      : { ok: false };
  } catch {
    return { ok: false };
  }
}

export function pluginSpec(f: PluginForm, existing: PluginResource | null): PluginSpec {
  const spec: PluginSpec = existing ? structuredClone(existing.spec) : { type: f.type };
  spec.type = f.type;
  put(spec, 'enabled', f.enabled ? undefined : false);
  put(spec, 'hooks', f.hooks);
  put(spec, 'order', f.order.trim() === '' ? undefined : Number(f.order));
  put(spec, 'on_error', f.onError === 'fail_closed' ? undefined : f.onError);
  put(spec, 'timeout', f.timeout.trim());
  put(spec, 'include_body', f.includeBody ? undefined : false);
  const config = parseConfig(f.config);
  put(spec, 'config', config.ok ? config.value : undefined);
  if (f.type === 'webhook') {
    const webhook: NonNullable<PluginSpec['webhook']> = existing?.spec.webhook
      ? structuredClone(existing.spec.webhook)
      : { url: f.url.trim() };
    webhook.url = f.url.trim();
    put(webhook, 'headers', headerMap(f.headers));
    put(webhook, 'signing_secret', f.signingSecret.trim() || (f.signingSecretStored ? REDACTED : undefined));
    put(webhook, 'max_response_bytes', f.maxResponse.trim());
    put(webhook, 'forward_headers', f.forwardHeaders);
    spec.webhook = webhook;
  } else delete spec.webhook;
  if (f.type === 'builtin') spec.builtin = { name: f.builtin.trim() };
  else delete spec.builtin;
  if (f.type === 'wasm') {
    const wasm: NonNullable<PluginSpec['wasm']> = {
      module: f.module.trim(),
      sha256: f.sha256.trim().toLowerCase(),
    };
    put(wasm, 'max_memory', f.maxMemory.trim());
    put(wasm, 'max_duration', f.maxDuration.trim());
    spec.wasm = wasm;
  } else delete spec.wasm;
  return spec;
}

const CREDENTIAL_HEADERS = [
  'authorization',
  'proxy-authorization',
  'x-api-key',
  'api-key',
  'cookie',
  'x-goog-api-key',
];

export function pluginErrors(f: PluginForm, taken: string[], creating: boolean): FieldErrors {
  const e: FieldErrors = {};
  if (creating) {
    const n = f.name.trim();
    if (!n) e.name = 'Required';
    else if (!NAME.test(n)) e.name = 'Letters, digits, . _ and -; start with a letter or digit';
    else if (taken.includes(n)) e.name = `${n} is already used`;
  }
  if (f.hooks.length === 0) e.hooks = 'Pick at least one';
  if (f.order.trim() && !/^-?[0-9]{1,7}$/.test(f.order.trim())) e.order = 'A whole number';
  const t = durationError(f.timeout);
  if (t) e.timeout = t;
  else if (f.timeout.trim() && (parseDuration(f.timeout.trim()) ?? 0) > MAX_TIMEOUT_S)
    e.timeout = 'At most 30s';
  if (!parseConfig(f.config).ok) e.config = 'A JSON object, such as {"content": "…"}';
  if (f.type === 'webhook') {
    const u = urlError(f.url, true);
    if (u) e.url = u;
    const h = headerErrors(f.headers);
    if (h) e.headers = h;
    const credential = f.forwardHeaders.find((h) => CREDENTIAL_HEADERS.includes(h.toLowerCase()));
    if (credential) e.forwardHeaders = `${credential} carries a credential, which plugins never receive`;
    if (f.maxResponse.trim() && !/^[0-9]+(B|KiB|MiB|KB|MB)$/.test(f.maxResponse.trim()))
      e.maxResponse = 'A size such as 512KiB or 1MiB';
  }
  if (f.type === 'builtin' && !f.builtin.trim()) e.builtin = 'Required';
  if (f.type === 'builtin' && f.builtin.trim() === 'static_response') {
    const c = parseConfig(f.config);
    if (!c.ok || typeof c.value?.content !== 'string') e.config = 'A static reply needs {"content": "…"}';
  }
  if (f.type === 'wasm') {
    if (!/^(file|oci):\/\//.test(f.module.trim())) e.module = 'A file:// or oci:// location';
    if (!/^[0-9a-fA-F]{64}$/.test(f.sha256.trim())) e.sha256 = 'The module’s SHA-256, 64 hexadecimal digits';
    const d = durationError(f.maxDuration);
    if (d) e.maxDuration = d;
  }
  return e;
}

/** A plugin's failure rate over its calls, or null before the first. */
export function failureRate(r: PluginRuntime | undefined): number | null {
  if (!r || r.calls === 0) return null;
  return r.failures / r.calls;
}
