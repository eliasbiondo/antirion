import { useCallback, useEffect } from 'react';
import { create } from 'zustand';

import { api, listAll } from '@/lib/api/client';
import type { DeploymentResource, ModelResource } from '@/lib/api/types';
import type { ModelMakerHints } from '@/lib/marks';

/** What the gateway says about a model's maker: its metadata and the upstream ids it calls. */
export interface ModelHints {
  ownedBy: string | null;
  family: string | null;
  upstream: string[];
}

interface ModelMakersState {
  /** Model name → its hints, once read. */
  models: Map<string, ModelHints> | null;
  loading: boolean;
  load: () => Promise<void>;
}

/** The deployments a model's target names, without following routes (a route's model falls back to its name). */
export function targetDeployments(node: unknown, out: string[] = []): string[] {
  if (typeof node === 'string') out.push(node);
  else if (Array.isArray(node)) for (const n of node) targetDeployments(n, out);
  else if (typeof node === 'object' && node !== null) {
    const record = node as Record<string, unknown>;
    if (typeof record.deployment === 'string') out.push(record.deployment);
    for (const [key, value] of Object.entries(record)) {
      if (key === 'params' || key === 'policies' || key === 'when' || key === 'match') continue;
      if (typeof value === 'object' && value !== null) targetDeployments(value, out);
    }
  }
  return out;
}

/** A model's hints from its resource and the deployments by name. */
export function modelHints(
  model: ModelResource,
  deployments: Map<string, DeploymentResource['spec']>,
): ModelHints {
  const upstream: string[] = [];
  for (const name of targetDeployments(model.spec.target)) {
    const d = deployments.get(name);
    if (!d) continue;
    if (d.base_model) upstream.push(d.base_model);
    upstream.push(d.model);
  }
  return {
    ownedBy: model.spec.owned_by ?? null,
    family: model.spec.catalog?.family ?? null,
    upstream,
  };
}

/**
 * Each model's maker hints, read once per page load, for model marks. A reader who can't list
 * models or deployments still gets marks from the names the screen shows.
 */
export const useModelMakers = create<ModelMakersState>()((set, get) => ({
  models: null,
  loading: false,
  async load() {
    if (get().models || get().loading) return;
    set({ loading: true });
    const [models, deployments] = await Promise.all([
      listAll((cursor) => api.get('/models', { query: { limit: 200, cursor } })).catch(() => []),
      listAll((cursor) => api.get('/deployments', { query: { limit: 200, cursor } })).catch(() => []),
    ]);
    const byName = new Map(deployments.map((d) => [d.name, d.spec] as const));
    set({ models: new Map(models.map((m) => [m.name, modelHints(m, byName)])), loading: false });
  },
}));

/**
 * A function from a model's name (and the upstream id a record says served it) to its maker hints,
 * for `ModelMark`.
 */
export function useModelHints(): (
  name: string | null | undefined,
  upstream?: string | null,
) => ModelMakerHints {
  const models = useModelMakers((s) => s.models);
  const load = useModelMakers((s) => s.load);
  useEffect(() => {
    void load();
  }, [load]);
  return useCallback(
    (name, upstream) => {
      const known = name ? models?.get(name) : undefined;
      return {
        ownedBy: known?.ownedBy,
        family: known?.family,
        upstream: [...(known?.upstream ?? []), upstream],
        name,
      };
    },
    [models],
  );
}
