import { describe, expect, it } from 'vitest';

import { makerFromModelId, makerFromOwner, modelFace, providerFace, resolveModelMaker } from '../marks';

describe('makerFromModelId', () => {
  it.each([
    ['gpt-4o', 'openai'],
    ['gpt-4o-mini', 'openai'],
    ['gpt-5', 'openai'],
    ['o1', 'openai'],
    ['o1-mini', 'openai'],
    ['o3-mini-2025-01-31', 'openai'],
    ['o4-mini', 'openai'],
    ['chatgpt-4o-latest', 'openai'],
    ['text-embedding-3-small', 'openai'],
    ['text-embedding-ada-002', 'openai'],
    ['whisper-1', 'openai'],
    ['claude-sonnet-4-5', 'anthropic'],
    ['claude-3-5-haiku-20241022', 'anthropic'],
    ['mistral-large-latest', 'mistral'],
    ['mixtral-8x7b-instruct', 'mistral'],
    ['codestral-2501', 'mistral'],
    ['ministral-8b-latest', 'mistral'],
    ['pixtral-12b', 'mistral'],
    ['magistral-medium', 'mistral'],
    ['open-mistral-nemo', 'mistral'],
    ['gemini-2.5-pro', 'google'],
    ['gemma-3-27b-it', 'google'],
    ['text-embedding-004', 'google'],
    ['llama-3.3-70b-instruct', 'meta'],
    ['llama3:8b', 'meta'],
    ['command-r-plus', 'cohere'],
    ['embed-english-v3.0', 'cohere'],
    ['deepseek-chat', 'deepseek'],
    ['deepseek-r1', 'deepseek'],
    ['qwen2.5-72b-instruct', 'qwen'],
    ['qwq-32b', 'qwen'],
    ['grok-4', 'xai'],
    ['phi-4', 'microsoft'],
    ['phi3:mini', 'microsoft'],
    ['nova-pro', 'amazon'],
    ['granite-3.1-8b-instruct', 'ibm'],
    ['jamba-1.5-large', 'ai21'],
    ['kimi-k2', 'moonshot'],
    ['glm-4.5', 'zhipu'],
    ['sonar-pro', 'perplexity'],
  ])('%s is made by %s', (id, maker) => {
    expect(makerFromModelId(id)).toBe(maker);
  });

  it('reads vendor prefixes, publisher paths and hub owners', () => {
    expect(makerFromModelId('anthropic.claude-3-5-sonnet-20240620-v1:0')).toBe('anthropic');
    expect(makerFromModelId('us.anthropic.claude-sonnet-4-5-20250929-v1:0')).toBe('anthropic');
    expect(makerFromModelId('meta.llama3-70b-instruct-v1:0')).toBe('meta');
    expect(makerFromModelId('amazon.nova-lite-v1:0')).toBe('amazon');
    expect(makerFromModelId('cohere.command-r-v1:0')).toBe('cohere');
    expect(makerFromModelId('projects/p/locations/l/publishers/google/models/gemini-2.0-flash')).toBe(
      'google',
    );
    expect(makerFromModelId('meta-llama/Llama-3.3-70B-Instruct')).toBe('meta');
    expect(makerFromModelId('mistralai/Mixtral-8x7B-Instruct-v0.1')).toBe('mistral');
    expect(makerFromModelId('Qwen/Qwen2.5-Coder-32B-Instruct')).toBe('qwen');
    expect(makerFromModelId('deepseek-ai/some-distill')).toBe('deepseek');
    expect(makerFromModelId('accounts/fireworks/models/llama-v3p1-8b-instruct')).toBe('meta');
  });

  it('needs a whole prefix and knows nothing about other names', () => {
    expect(makerFromModelId('o1x')).toBeNull();
    expect(makerFromModelId('phind-codellama')).toBeNull();
    expect(makerFromModelId('acme-house-7b')).toBeNull();
    expect(makerFromModelId('house-model')).toBeNull();
    expect(makerFromModelId('')).toBeNull();
    expect(makerFromModelId(null)).toBeNull();
  });
});

describe('resolveModelMaker', () => {
  it('prefers the model metadata, then its upstream ids, then its name', () => {
    expect(resolveModelMaker({ ownedBy: 'Anthropic', name: 'gpt-4o' })).toBe('anthropic');
    expect(resolveModelMaker({ ownedBy: 'mistralai' })).toBe('mistral');
    expect(resolveModelMaker({ family: 'claude-sonnet-4', name: 'chat' })).toBe('anthropic');
    expect(resolveModelMaker({ upstream: [null, 'gemini-2.5-flash'], name: 'gpt-alias' })).toBe('google');
    expect(resolveModelMaker({ upstream: ['acme-7b'], name: 'claude-sonnet' })).toBe('anthropic');
    expect(resolveModelMaker({ name: 'fast' })).toBeNull();
  });

  it('never takes the maker from the provider: a claude model on an OpenAI-typed provider stays Anthropic', () => {
    expect(modelFace({ name: 'claude-sonnet', upstream: ['claude-sonnet-4-5'] })).toEqual({
      brand: 'anthropic',
      label: 'Anthropic',
    });
  });

  it('draws an unknown maker as a monogram, of its owner when it has one', () => {
    expect(modelFace({ name: 'house-model' })).toEqual({ brand: null, letter: 'H', label: 'Unknown maker' });
    expect(modelFace({ name: 'house-model', ownedBy: 'acme' })).toEqual({
      brand: null,
      letter: 'A',
      label: 'acme',
    });
    expect(makerFromOwner('Meta Llama')).toBe('meta');
    expect(makerFromOwner('acme')).toBeNull();
  });
});

describe('providerFace', () => {
  it("draws a provider type's brand, and generic or unknown types as a monogram", () => {
    expect(providerFace('openai')).toEqual({ brand: 'openai', label: 'OpenAI' });
    expect(providerFace('bedrock')).toEqual({ brand: 'bedrock', label: 'AWS Bedrock' });
    expect(providerFace('azure_openai')).toEqual({ brand: 'azure', label: 'Azure OpenAI' });
    expect(providerFace('triton')).toEqual({ brand: 'nvidia', label: 'Triton' });
    expect(providerFace('openai_compatible')).toEqual({
      brand: null,
      letter: 'O',
      label: 'OpenAI-compatible',
    });
    expect(providerFace('anthropic_compatible')).toEqual({
      brand: null,
      letter: 'A',
      label: 'Anthropic-compatible',
    });
    expect(providerFace('custom_thing')).toEqual({ brand: null, letter: 'C', label: 'custom_thing' });
    expect(providerFace(undefined)).toEqual({ brand: null, letter: 'U', label: 'unknown' });
  });
});
