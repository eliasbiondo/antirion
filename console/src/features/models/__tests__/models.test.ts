import { describe, expect, it } from 'vitest';

import type { RouteResource } from '@/lib/api/types';

import { addModelSchema, applyModelEdit, deploymentNameFor, EMPTY_ADD_MODEL, fromModelSpec } from '../form';
import { collectLeaves, fallbackChain, modalityOf } from '../types';

const routes = new Map<string, RouteResource>([
  [
    'shared',
    {
      name: 'shared',
      spec: { tree: { type: 'fallback', children: ['b', { deployment: 'c' }] } },
      metadata: { etag: '', generation: 1, created_at: '', updated_at: '', managed_by: 'api' },
    },
  ],
]);

describe('route trees', () => {
  it('collects every reachable deployment, following routes', () => {
    expect(collectLeaves('a', routes)).toEqual(['a']);
    expect(
      collectLeaves(
        { type: 'weighted', children: [{ deployment: 'a', weight: 2 }, { route: 'shared' }] },
        routes,
      ),
    ).toEqual(['a', 'b', 'c']);
  });
  it('reads the fallback chain of a fallback root', () => {
    expect(fallbackChain({ type: 'fallback', children: ['a', 'b'] })).toEqual(['a', 'b']);
    expect(fallbackChain('a')).toBeNull();
  });
  it('derives the modality from operations', () => {
    expect(modalityOf(['generate', 'embed'])).toBe('chat');
    expect(modalityOf(['embed'])).toBe('embeddings');
  });
});

describe('model forms', () => {
  it('names deployments from provider and upstream id', () => {
    expect(deploymentNameFor('openai', 'gpt-4o-2024-08-06')).toBe('openai-gpt-4o-2024-08-06');
  });
  it('requires both prices or neither', () => {
    const v = { ...EMPTY_ADD_MODEL, name: 'm', provider: 'p', upstreamModel: 'x', inputPrice: '1' };
    expect(addModelSchema.safeParse(v).success).toBe(false);
    expect(addModelSchema.safeParse({ ...v, outputPrice: '2' }).success).toBe(true);
  });
  it('keeps the tree and aliases when editing', () => {
    const spec = { target: 'dep', aliases: { fast: {} }, tier: 'fast', display_name: 'Old' };
    const edited = applyModelEdit(spec, {
      ...fromModelSpec(spec),
      displayName: 'New',
      lifecycle: 'deprecated',
      eolAt: '2026-12-31',
    });
    expect(edited.target).toBe('dep');
    expect(edited.aliases).toEqual({ fast: {} });
    expect(edited.display_name).toBe('New');
    expect(edited.lifecycle).toEqual({ state: 'deprecated', eol_at: '2026-12-31T00:00:00Z' });
  });
});
