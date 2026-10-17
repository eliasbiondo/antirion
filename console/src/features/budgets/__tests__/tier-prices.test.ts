import { describe, expect, it } from 'vitest';

import { priceListKey } from '@/features/models/tiers';
import type { DeploymentResource, EffectivePrice } from '@/lib/api/types';

import {
  ownTiers,
  parseTierText,
  tierRowsOf,
  tierSettingsPatch,
  tierText,
  upstreamChoices,
} from '../tier-prices';

const price = (deployment: string, provider_type: string, upstream_model: string): EffectivePrice => ({
  deployment,
  provider: 'p',
  provider_type,
  upstream_model,
  price: { input: '1.00', output: '2.00' },
  price_version: 'v1',
});

describe('the list price key', () => {
  it('is the provider type and the upstream model, never the public Model name', () => {
    expect(priceListKey({ provider_type: 'openai', upstream_model: 'gpt-4o-2024-08-06' })).toBe(
      'openai/gpt-4o-2024-08-06',
    );
    expect(priceListKey({ provider_type: 'openai' })).toBeNull();
    expect(priceListKey(null)).toBeNull();
  });
});

describe('the service-tier drawer', () => {
  it('offers each upstream model once, keyed as the Models price grid keys it', () => {
    const choices = upstreamChoices([
      price('fast-a', 'openai', 'gpt-4o-mini'),
      price('fast-b', 'openai', 'gpt-4o-mini'),
      price('claude', 'anthropic', 'claude-sonnet-4'),
      { deployment: 'unpriced', price: null, price_version: null },
    ]);
    expect(choices).toEqual([
      { key: 'anthropic/claude-sonnet-4', upstream: 'claude-sonnet-4', deployments: ['claude'] },
      { key: 'openai/gpt-4o-mini', upstream: 'gpt-4o-mini', deployments: ['fast-a', 'fast-b'] },
    ]);
  });

  it('reads the list prices that set tiers, and leaves the others out', () => {
    const { rows, before } = tierRowsOf({
      'openai/gpt-4o': { input: '2.50', service_tiers: { flex: '0.5', priority: '2' } },
      'openai/gpt-4o-mini': { input: '0.15' },
      'anthropic/claude-sonnet-4': { batch_multiplier: '0.5' },
    });
    expect(rows).toEqual([
      { key: 'openai/gpt-4o', tiers: 'flex=0.5, priority=2', batch: '' },
      { key: 'anthropic/claude-sonnet-4', tiers: '', batch: '0.5' },
    ]);
    expect(before['openai/gpt-4o']).toEqual([
      { tier: 'flex', multiplier: '0.5' },
      { tier: 'priority', multiplier: '2' },
    ]);
  });

  it('writes under the list price key and removes the tiers a row dropped', () => {
    const { before } = tierRowsOf({ 'openai/gpt-4o': { service_tiers: { flex: '0.5', priority: '2' } } });
    const result = tierSettingsPatch(
      [
        { key: 'openai/gpt-4o', tiers: 'flex=0.4', batch: '' },
        { key: 'openai/gpt-4o-mini', tiers: 'priority=1.5', batch: '0.5' },
      ],
      before,
    );
    expect(result).toEqual({
      patch: {
        'openai/gpt-4o': { service_tiers: { priority: null, flex: '0.4' }, batch_multiplier: null },
        'openai/gpt-4o-mini': { service_tiers: { priority: '1.5' }, batch_multiplier: '0.5' },
      },
    });
  });

  it('clears the tiers of a removed row and keeps its token prices', () => {
    const { before } = tierRowsOf({ 'openai/gpt-4o': { input: '2.50', service_tiers: { flex: '0.5' } } });
    expect(tierSettingsPatch([], before)).toEqual({
      patch: { 'openai/gpt-4o': { service_tiers: null, batch_multiplier: null } },
    });
  });

  it('skips an empty new row and refuses a repeated key or a malformed tier', () => {
    expect(tierSettingsPatch([{ key: 'openai/x', tiers: '', batch: '' }], {})).toEqual({ patch: {} });
    expect(
      tierSettingsPatch(
        [
          { key: 'openai/x', tiers: 'flex=0.5', batch: '' },
          { key: 'openai/x', tiers: 'priority=2', batch: '' },
        ],
        {},
      ),
    ).toEqual({ error: 'openai/x is listed twice.' });
    expect(tierSettingsPatch([{ key: 'openai/x', tiers: 'flex 0.5', batch: '' }], {})).toHaveProperty(
      'error',
    );
    expect(tierSettingsPatch([{ key: 'openai/x', tiers: 'Flex=0.5', batch: '' }], {})).toHaveProperty(
      'error',
    );
    expect(tierSettingsPatch([{ key: 'openai/x', tiers: 'flex=half', batch: '' }], {})).toHaveProperty(
      'error',
    );
    expect(tierSettingsPatch([{ key: 'openai/x', tiers: '', batch: 'x' }], {})).toHaveProperty('error');
  });

  it('parses and prints tiers as name=multiplier', () => {
    expect(parseTierText(' flex = 0.5 , priority=2 ')).toEqual([
      { tier: 'flex', multiplier: '0.5' },
      { tier: 'priority', multiplier: '2' },
    ]);
    expect(parseTierText('')).toEqual([]);
    expect(parseTierText('flex')).toBeNull();
    expect(tierText([{ tier: 'flex', multiplier: '0.5' }])).toBe('flex=0.5');
  });

  it('lists the deployments that set tiers in their own price', () => {
    const deployment = (name: string, p: Record<string, unknown> | undefined) =>
      ({
        name,
        spec: { provider: 'p', model: 'm', ...(p ? { price: p } : {}) },
      }) as unknown as DeploymentResource;
    expect(
      ownTiers([
        deployment('a', { input: '1', service_tiers: { flex: '0.5' } }),
        deployment('b', { input: '1' }),
        deployment('c', undefined),
      ]),
    ).toEqual([{ deployment: 'a', tiers: 'flex=0.5', batch: '' }]);
  });
});
