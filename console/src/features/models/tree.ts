/**
 * A Model's route tree as the console edits it: the fallback chain of its root, or the shared route
 * it points at. Steps are positions in the tree, never other Models' names.
 */
import type { ModelSpec, RouteResource } from '@/lib/api/types';

type Json = unknown;

function isRecord(value: Json): value is Record<string, Json> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export type StepKind = 'deployment' | 'route' | 'tier' | 'subtree';

export interface Step {
  /** A stable key for the list while it is reordered. */
  key: string;
  kind: StepKind;
  /** The deployment, route or tier it names; for a sub-tree, its node type. */
  value: string;
  /** The node as it is written, kept as it was for steps the editor does not change. */
  node: Json;
}

let counter = 0;
const nextKey = () => `s${(counter += 1)}`;

/** One child of a fallback node as a step. */
export function stepOf(node: Json): Step {
  if (typeof node === 'string') return { key: nextKey(), kind: 'deployment', value: node, node };
  if (isRecord(node)) {
    const type = typeof node.type === 'string' ? node.type : undefined;
    if (typeof node.deployment === 'string' && (!type || type === 'deployment'))
      return { key: nextKey(), kind: 'deployment', value: node.deployment, node };
    if (typeof node.route === 'string' && (!type || type === 'route'))
      return { key: nextKey(), kind: 'route', value: node.route, node };
    if (typeof node.tier === 'string' && (!type || type === 'tier'))
      return { key: nextKey(), kind: 'tier', value: node.tier, node };
    return { key: nextKey(), kind: 'subtree', value: type ?? 'node', node };
  }
  return { key: nextKey(), kind: 'subtree', value: 'node', node };
}

/** A new step that names a deployment, a shared route or a tier. */
export function newStep(kind: Exclude<StepKind, 'subtree'>, value: string): Step {
  const node = kind === 'deployment' ? value : kind === 'route' ? { route: value } : { tier: value };
  return { key: nextKey(), kind, value, node };
}

/** The root of a Model's tree: its `target`, or a reference to the shared route it points at. */
export function rootOf(spec: ModelSpec): Json {
  if (spec.route) return { route: spec.route };
  return spec.target ?? null;
}

/** The steps of a Model's chain: the children of a `fallback` root, or the root as one step. */
export function stepsOf(spec: ModelSpec): Step[] {
  const root = rootOf(spec);
  if (root === null || root === undefined) return [];
  if (isRecord(root) && root.type === 'fallback' && Array.isArray(root.children))
    return (root.children as Json[]).map(stepOf);
  return [stepOf(root)];
}

/**
 * The spec with its chain replaced: one step is the target itself, several are a `fallback` node
 * (keeping the old fallback root's id, params and policies). The Model then has a `target` of its
 * own and no longer points at a shared route.
 */
export function withChain(spec: ModelSpec, steps: Step[]): ModelSpec {
  const old = rootOf(spec);
  const { route: _route, target: _target, ...rest } = spec;
  if (steps.length === 0) return spec;
  const [only] = steps;
  if (steps.length === 1 && only) return { ...rest, target: only.node };
  const keep = isRecord(old) && old.type === 'fallback' ? old : {};
  const { children: _children, type: _type, ...attrs } = keep;
  return {
    ...rest,
    target: { ...attrs, type: 'fallback', children: steps.map((s) => s.node) },
  };
}

/** The spec pointing at a shared route, its own tree dropped. */
export function withRoute(spec: ModelSpec, route: string): ModelSpec {
  const { target: _target, route: _route, ...rest } = spec;
  return { ...rest, route };
}

/** Moves the step at `from` by `delta` places, staying in the list. */
export function moveStep<T>(steps: T[], from: number, delta: number): T[] {
  const to = Math.max(0, Math.min(steps.length - 1, from + delta));
  if (to === from) return steps;
  const next = [...steps];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item as T);
  return next;
}

/** A node in words, for a step that is a whole sub-tree: "weighted split of a, b". */
export function describeNode(node: Json, routes?: Map<string, RouteResource>): string {
  if (typeof node === 'string') return node;
  if (!isRecord(node)) return 'node';
  if (typeof node.deployment === 'string') return node.deployment;
  if (typeof node.route === 'string') {
    const exists = routes ? routes.has(node.route) : true;
    return `shared route ${node.route}${exists ? '' : ' (missing)'}`;
  }
  if (typeof node.tier === 'string') return `the ${node.tier} tier`;
  const children = Array.isArray(node.children) ? (node.children as Json[]) : [];
  const names = children.map((c) => describeNode(c, routes)).join(', ');
  switch (node.type) {
    case 'weighted':
      return `weighted split of ${names}`;
    case 'fallback':
      return `${names}, in order`;
    case 'least_latency':
      return `the fastest of ${names}`;
    case 'least_busy':
      return `the least busy of ${names}`;
    case 'least_cost':
      return `the cheapest of ${names}`;
    case 'conditional':
      return 'rules by request';
    case 'block':
      return 'a block';
    default:
      return typeof node.type === 'string' ? node.type.replace(/_/g, ' ') : 'node';
  }
}

/**
 * A Model's rule list, when its root is `conditional`: in its own target (the route is then named
 * after the Model) or in the shared route it points at. `route` is the name its paths start with.
 */
export function conditionalRoot(
  model: string,
  spec: ModelSpec,
  routes: Map<string, RouteResource>,
): { route: string; shared: boolean; tree: Record<string, Json> } | null {
  const tree = spec.route ? routes.get(spec.route)?.spec.tree : spec.target;
  if (!isRecord(tree) || tree.type !== 'conditional') return null;
  return { route: spec.route ?? model, shared: Boolean(spec.route), tree };
}
