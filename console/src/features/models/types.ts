import type { DeploymentHealth, DeploymentResource, ModelResource, RouteResource } from '@/lib/api/types';

import { type DeploymentCapabilities, modalityFor, tokenLimit } from './catalog';

export type Lifecycle = 'preview' | 'ga' | 'restricted' | 'deprecated';
export const LIFECYCLES: Lifecycle[] = ['ga', 'preview', 'restricted', 'deprecated'];
export const BUILTIN_TIERS = ['frontier', 'balanced', 'fast', 'specialized'] as const;

export type Modality = 'chat' | 'embeddings' | 'rerank' | 'image' | 'audio' | 'other';

export interface ModelView {
  model: ModelResource;
  name: string;
  displayName: string;
  enabled: boolean;
  lifecycle: Lifecycle;
  tier: string | null;
  leaves: string[];
  providers: string[];
  upstream: string[];
  aliases: string[];
  /** From the first leaf whose capabilities are known (catalog.ts `modalityFor`). */
  modality: Modality;
  /** The smallest context window its leaves state, or null. */
  context: number | null;
}

type Json = unknown;

function isRecord(value: Json): value is Record<string, Json> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * The deployments a route tree can reach, in tree order: a bare name or a `deployment` node is a
 * leaf, a `route` node is followed into that route, and every other node's children are walked.
 */
export function collectLeaves(
  node: Json,
  routes: Map<string, RouteResource>,
  seen = new Set<string>(),
): string[] {
  if (typeof node === 'string') return [node];
  if (Array.isArray(node)) return node.flatMap((n) => collectLeaves(n, routes, seen));
  if (!isRecord(node)) return [];
  if (typeof node.deployment === 'string') return [node.deployment];
  if (typeof node.route === 'string') {
    if (seen.has(node.route)) return [];
    seen.add(node.route);
    const route = routes.get(node.route);
    return route ? collectLeaves(route.spec.tree, routes, seen) : [];
  }
  const out: string[] = [];
  for (const [key, value] of Object.entries(node)) {
    if (key === 'params' || key === 'policies' || key === 'when' || key === 'match') continue;
    if (typeof value === 'object' && value !== null) out.push(...collectLeaves(value, routes, seen));
  }
  return out;
}

/** The fallback chain of a model's tree: the children of its root `fallback` node, in order. */
export function fallbackChain(node: Json): Json[] | null {
  if (!isRecord(node) || node.type !== 'fallback') return null;
  const children: Json = node.children;
  return Array.isArray(children) ? (children as Json[]) : null;
}

export function nodeLabel(node: Json): string {
  if (typeof node === 'string') return node;
  if (isRecord(node)) {
    if (typeof node.deployment === 'string') return node.deployment;
    if (typeof node.route === 'string') return `route ${node.route}`;
    if (typeof node.type === 'string') return node.type;
  }
  return 'node';
}

export function toModelView(
  model: ModelResource,
  deployments: Map<string, DeploymentResource>,
  routes: Map<string, RouteResource>,
  capabilities: Record<string, DeploymentCapabilities | null> = {},
): ModelView {
  const tree = (model.spec.target ?? (model.spec.route ? { route: model.spec.route } : null)) as Json;
  const leaves = [...new Set(collectLeaves(tree, routes))];
  const deps = leaves.map((l) => deployments.get(l)).filter((d): d is DeploymentResource => d !== undefined);
  return {
    model,
    name: model.name,
    displayName: model.spec.display_name ?? model.name,
    enabled: model.spec.enabled !== false,
    lifecycle: model.spec.lifecycle?.state ?? 'ga',
    tier: model.spec.tier ?? null,
    leaves,
    providers: [...new Set(deps.map((d) => d.spec.provider))],
    upstream: [...new Set(deps.map((d) => d.spec.model))],
    aliases: Object.keys(model.spec.aliases ?? {}),
    modality: modalityFor(capabilities[leaves[0] ?? '']?.capabilities, deps[0]?.spec.model),
    context: smallest(leaves.map((l) => tokenLimit(capabilities[l]?.capabilities, 'context_window'))),
  };
}

function smallest(values: (number | null)[]): number | null {
  const known = values.filter((v): v is number => v !== null);
  return known.length > 0 ? Math.min(...known) : null;
}

export function lifecycleTone(state: Lifecycle): 'success' | 'info' | 'warn' | 'neutral' {
  switch (state) {
    case 'ga':
      return 'success';
    case 'preview':
      return 'info';
    case 'restricted':
      return 'warn';
    case 'deprecated':
      return 'neutral';
  }
}

/** A Model's modality from its leaves' operations capability. */
export function modalityOf(operations: string[] | undefined): Modality {
  if (!operations || operations.length === 0) return 'other';
  if (operations.includes('generate')) return 'chat';
  if (operations.includes('embed')) return 'embeddings';
  if (operations.includes('rerank')) return 'rerank';
  if (operations.includes('image_generate')) return 'image';
  if (operations.includes('transcribe') || operations.includes('speech')) return 'audio';
  return 'other';
}

/** One rate of a price object (`input`, `output`, `cache_read`…), as the decimal string the API sends. */
export function rate(price: Record<string, unknown> | null | undefined, field: string): string | null {
  const value = price?.[field];
  return typeof value === 'string' || typeof value === 'number' ? String(value) : null;
}

/** The deployment a health entry describes; the gateway names the field `deployment`, the contract `name`. */
export function healthName(entry: DeploymentHealth): string {
  const raw = entry as DeploymentHealth & { deployment?: string };
  return raw.deployment ?? raw.name;
}

/** What an operator reads first about a model: whether apps can use it, and how. */
export type ModelStatus = 'available' | 'preview' | 'restricted' | 'deprecated' | 'off';

export function statusOf(view: Pick<ModelView, 'enabled' | 'lifecycle'>): ModelStatus {
  if (!view.enabled) return 'off';
  return view.lifecycle === 'ga' ? 'available' : view.lifecycle;
}

export const STATUS_LABEL: Record<ModelStatus, string> = {
  available: 'Available',
  preview: 'Preview',
  restricted: 'Restricted',
  deprecated: 'Deprecated',
  off: 'Off',
};

export const STATUS_TONE: Record<ModelStatus, 'success' | 'info' | 'warn' | 'neutral' | 'error'> = {
  available: 'success',
  preview: 'info',
  restricted: 'warn',
  deprecated: 'neutral',
  off: 'error',
};

/** "balanced" → "Balanced"; custom tier names keep their spelling after the first letter. */
export function tierLabel(tier: string): string {
  return tier.charAt(0).toUpperCase() + tier.slice(1).replace(/[_-]/g, ' ');
}
