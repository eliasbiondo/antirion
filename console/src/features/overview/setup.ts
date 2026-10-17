'use client';

import { useEffect, useState } from 'react';

import { api } from '@/lib/api/client';
import { usePolling } from '@/lib/hooks';
import { hasGatewayPermission, hasPermission, useSession } from '@/stores/session';

/** What a new gateway still needs before it can serve traffic; `null` where it could not be read. */
export interface SetupState {
  loaded: boolean;
  hasProvider: boolean | null;
  hasModel: boolean | null;
  hasKey: boolean | null;
  /** Whether the gateway has logged any request, in any window. */
  hasRequest: boolean | null;
  /** A model name for the first-request snippet. */
  modelName: string | null;
}

const INITIAL: SetupState = {
  loaded: false,
  hasProvider: null,
  hasModel: null,
  hasKey: null,
  hasRequest: null,
  modelName: null,
};

async function settle<T>(promise: Promise<T>): Promise<T | null> {
  try {
    return await promise;
  } catch {
    return null;
  }
}

async function readSetup(): Promise<SetupState> {
  const me = useSession.getState().me;
  // Each step is read only when the principal may read it; a step it can't see stays unknown.
  const readIf = <T>(allowed: boolean, read: () => Promise<T>) =>
    allowed ? settle(read()) : Promise.resolve(null);
  const [providers, models, keys, logs] = await Promise.all([
    readIf(hasGatewayPermission(me, 'providers:read'), () => api.get('/providers', { query: { limit: 1 } })),
    readIf(hasGatewayPermission(me, 'models:read'), () => api.get('/models', { query: { limit: 1 } })),
    readIf(hasPermission(me, 'keys:read', null), () => api.get('/keys', { query: { limit: 1 } })),
    readIf(hasPermission(me, 'logs:read', null), () => api.get('/logs', { query: { limit: 1 } })),
  ]);
  return {
    loaded: true,
    hasProvider: providers ? providers.items.length > 0 : null,
    hasModel: models ? models.items.length > 0 : null,
    hasKey: keys ? keys.items.length > 0 : null,
    hasRequest: logs ? logs.items.length > 0 : null,
    modelName: models?.items[0]?.name ?? null,
  };
}

/**
 * The Get started checklist's state, from real reads: a provider, a model, a key and a first
 * request. It is read again on every poll until the gateway has served a request, so the steps
 * tick as the operator completes them in another tab.
 */
export function useSetup(): SetupState {
  const [state, setState] = useState<SetupState>(INITIAL);
  const done = state.hasRequest === true;
  useEffect(() => {
    let live = true;
    void readSetup().then((s) => {
      if (live) setState(s);
    });
    return () => {
      live = false;
    };
  }, []);
  usePolling(() => {
    void readSetup().then(setState);
  }, state.loaded && !done);
  return state;
}

/** The base address of the data plane for a snippet: no trailing slash, `/v1` added once. */
export function chatUrl(dataPlaneUrl: string | null | undefined): string {
  const base = (dataPlaneUrl ?? 'https://your-gateway.example.com').replace(/\/+$/, '');
  return `${base.endsWith('/v1') ? base : `${base}/v1`}/chat/completions`;
}

/** The first-request snippet: one chat completion through the gateway with an API key. */
export function curlSnippet(dataPlaneUrl: string | null | undefined, model: string | null): string {
  return [
    `curl ${chatUrl(dataPlaneUrl)} \\`,
    '  -H "Authorization: Bearer $ANTIRION_API_KEY" \\',
    '  -H "Content-Type: application/json" \\',
    "  -d '{",
    `    "model": ${JSON.stringify(model ?? 'your-model')},`,
    '    "messages": [{"role": "user", "content": "Hello"}]',
    "  }'",
  ].join('\n');
}
