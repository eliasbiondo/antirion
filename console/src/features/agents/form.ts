import type { FieldErrors } from '@/lib/validation';

import {
  durationError,
  headerErrors,
  headerMap,
  headerRows,
  type HeaderRow,
  mcpNameError,
  put,
  urlError,
} from '@/features/mcp/fields';

import type { AgentResource, AgentSpec } from './api';

export type Protocol = NonNullable<AgentSpec['protocol']>;

export const PROTOCOL_LABEL: Record<Protocol, string> = {
  auto: 'Detect',
  '0.3': 'A2A 0.3',
  '1.0': 'A2A 1.0',
};

export interface AgentForm {
  name: string;
  cardUrl: string;
  url: string;
  protocol: Protocol;
  enabled: boolean;
  publicCard: boolean;
  headers: HeaderRow[];
  cardCacheTtl: string;
  requestTimeout: string;
  streamIdleTimeout: string;
  extraMethods: string[];
}

export const EMPTY_AGENT: AgentForm = {
  name: '',
  cardUrl: '',
  url: '',
  protocol: 'auto',
  enabled: true,
  publicCard: false,
  headers: [],
  cardCacheTtl: '',
  requestTimeout: '',
  streamIdleTimeout: '',
  extraMethods: [],
};

export function agentForm(r: AgentResource): AgentForm {
  const s = r.spec;
  return {
    name: r.name,
    cardUrl: s.card_url,
    url: s.url ?? '',
    protocol: s.protocol ?? 'auto',
    enabled: s.enabled !== false,
    publicCard: s.public_card === true,
    headers: headerRows(s.headers),
    cardCacheTtl: s.card_cache_ttl ?? '',
    requestTimeout: s.request_timeout ?? '',
    streamIdleTimeout: s.stream_idle_timeout ?? '',
    extraMethods: s.extra_methods ?? [],
  };
}

/** The agent to write; identity headers, which the form doesn't show, are kept from the read. */
export function agentSpec(f: AgentForm, existing: AgentResource | null): AgentSpec {
  const spec: AgentSpec = existing ? structuredClone(existing.spec) : { card_url: f.cardUrl.trim() };
  spec.card_url = f.cardUrl.trim();
  put(spec, 'url', f.url.trim());
  put(spec, 'protocol', f.protocol === 'auto' ? undefined : f.protocol);
  put(spec, 'enabled', f.enabled ? undefined : false);
  put(spec, 'public_card', f.publicCard ? true : undefined);
  put(spec, 'headers', headerMap(f.headers));
  put(spec, 'card_cache_ttl', f.cardCacheTtl.trim());
  put(spec, 'request_timeout', f.requestTimeout.trim());
  put(spec, 'stream_idle_timeout', f.streamIdleTimeout.trim());
  put(spec, 'extra_methods', f.extraMethods);
  return spec;
}

export function agentErrors(f: AgentForm, taken: string[], creating: boolean): FieldErrors {
  const e: FieldErrors = {};
  const name = creating ? mcpNameError(f.name, taken) : undefined;
  if (name) e.name = name;
  const card = urlError(f.cardUrl, true);
  if (card) e.cardUrl = card;
  const url = urlError(f.url, false);
  if (url) e.url = url;
  const headers = headerErrors(f.headers);
  if (headers) e.headers = headers;
  for (const key of ['cardCacheTtl', 'requestTimeout', 'streamIdleTimeout'] as const) {
    const d = durationError(f[key]);
    if (d) e[key] = d;
  }
  return e;
}
