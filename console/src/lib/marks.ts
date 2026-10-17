/**
 * Which brand mark names a provider or a model, in one place, so a provider or a model looks the
 * same on every screen.
 *
 * - A provider's mark is its type's brand. Generic types (OpenAI- or Anthropic-compatible, and any
 *   type without a mark) get a neutral monogram: never another brand's logo.
 * - A model's mark is its maker's, never the provider that serves it: `claude-*` is Anthropic's
 *   whether it runs on Anthropic, Bedrock or an OpenAI-compatible proxy. The maker comes from the
 *   model's own metadata (`owned_by`, `catalog.family`), then from the upstream model ids it calls,
 *   then from its name, through a small prefix table. Unknown makers get a neutral monogram.
 */
import type { BrandId } from '@/components/ui/brand-marks';

import { providerTypeLabel } from './providers';

/** A model maker with a mark. */
export type Maker =
  | 'openai'
  | 'anthropic'
  | 'mistral'
  | 'google'
  | 'meta'
  | 'cohere'
  | 'deepseek'
  | 'qwen'
  | 'xai'
  | 'microsoft'
  | 'amazon'
  | 'nvidia'
  | 'ibm'
  | 'ai21'
  | 'moonshot'
  | 'zhipu'
  | 'minimax'
  | 'perplexity';

export const MAKERS: Record<Maker, { label: string; brand: BrandId }> = {
  openai: { label: 'OpenAI', brand: 'openai' },
  anthropic: { label: 'Anthropic', brand: 'anthropic' },
  mistral: { label: 'Mistral AI', brand: 'mistral' },
  google: { label: 'Google', brand: 'gemini' },
  meta: { label: 'Meta', brand: 'meta' },
  cohere: { label: 'Cohere', brand: 'cohere' },
  deepseek: { label: 'DeepSeek', brand: 'deepseek' },
  qwen: { label: 'Qwen', brand: 'qwen' },
  xai: { label: 'xAI', brand: 'xai' },
  microsoft: { label: 'Microsoft', brand: 'microsoft' },
  amazon: { label: 'Amazon', brand: 'nova' },
  nvidia: { label: 'NVIDIA', brand: 'nvidia' },
  ibm: { label: 'IBM', brand: 'ibm' },
  ai21: { label: 'AI21 Labs', brand: 'ai21' },
  moonshot: { label: 'Moonshot AI', brand: 'kimi' },
  zhipu: { label: 'Zhipu AI', brand: 'zhipu' },
  minimax: { label: 'MiniMax', brand: 'minimax' },
  perplexity: { label: 'Perplexity', brand: 'perplexity' },
};

/**
 * Model id prefixes, checked in order on the id's last path segment, lower-cased. A prefix ending in
 * a letter or digit must be followed by the end, a separator or a digit (`o1` matches `o1-mini`,
 * not `o1x`); a prefix ending in `-` or `.` matches as is.
 */
export const MODEL_PREFIXES: [prefix: string, maker: Maker][] = [
  // Google's text embeddings (text-embedding-004) before OpenAI's (text-embedding-3-small).
  ['text-embedding-00', 'google'],
  ['text-multilingual-embedding', 'google'],
  ['textembedding-gecko', 'google'],
  ['gpt', 'openai'],
  ['chatgpt', 'openai'],
  ['o1', 'openai'],
  ['o3', 'openai'],
  ['o4', 'openai'],
  ['text-embedding', 'openai'],
  ['text-moderation', 'openai'],
  ['omni-moderation', 'openai'],
  ['davinci', 'openai'],
  ['babbage', 'openai'],
  ['dall-e', 'openai'],
  ['whisper', 'openai'],
  ['tts-1', 'openai'],
  ['codex', 'openai'],
  ['computer-use', 'openai'],
  ['sora', 'openai'],
  ['claude', 'anthropic'],
  ['mistral', 'mistral'],
  ['open-mistral', 'mistral'],
  ['open-mixtral', 'mistral'],
  ['mixtral', 'mistral'],
  ['codestral', 'mistral'],
  ['ministral', 'mistral'],
  ['pixtral', 'mistral'],
  ['magistral', 'mistral'],
  ['devstral', 'mistral'],
  ['voxtral', 'mistral'],
  ['gemini', 'google'],
  ['gemma', 'google'],
  ['palm', 'google'],
  ['text-bison', 'google'],
  ['chat-bison', 'google'],
  ['imagen', 'google'],
  ['veo', 'google'],
  ['llama', 'meta'],
  ['meta-llama', 'meta'],
  ['codellama', 'meta'],
  ['command', 'cohere'],
  ['embed-', 'cohere'],
  ['rerank-', 'cohere'],
  ['c4ai-aya', 'cohere'],
  ['aya', 'cohere'],
  ['deepseek', 'deepseek'],
  ['qwen', 'qwen'],
  ['qwq', 'qwen'],
  ['qvq', 'qwen'],
  ['grok', 'xai'],
  ['phi', 'microsoft'],
  ['nova', 'amazon'],
  ['titan', 'amazon'],
  ['nemotron', 'nvidia'],
  ['granite', 'ibm'],
  ['jamba', 'ai21'],
  ['kimi', 'moonshot'],
  ['moonshot', 'moonshot'],
  ['glm', 'zhipu'],
  ['chatglm', 'zhipu'],
  ['minimax', 'minimax'],
  ['abab', 'minimax'],
  ['sonar', 'perplexity'],
];

/** Organisation names as they appear in `owned_by`, ids' vendor prefixes and hub paths. */
const OWNERS: Record<string, Maker> = {
  openai: 'openai',
  'openai-internal': 'openai',
  anthropic: 'anthropic',
  mistral: 'mistral',
  mistralai: 'mistral',
  'mistral-ai': 'mistral',
  google: 'google',
  'google-deepmind': 'google',
  googleai: 'google',
  deepmind: 'google',
  meta: 'meta',
  'meta-llama': 'meta',
  facebook: 'meta',
  cohere: 'cohere',
  coherelabs: 'cohere',
  forcohere: 'cohere',
  deepseek: 'deepseek',
  'deepseek-ai': 'deepseek',
  qwen: 'qwen',
  alibaba: 'qwen',
  'alibaba-cloud': 'qwen',
  xai: 'xai',
  'x-ai': 'xai',
  microsoft: 'microsoft',
  amazon: 'amazon',
  aws: 'amazon',
  nvidia: 'nvidia',
  ibm: 'ibm',
  'ibm-granite': 'ibm',
  ai21: 'ai21',
  ai21labs: 'ai21',
  moonshot: 'moonshot',
  moonshotai: 'moonshot',
  zhipu: 'zhipu',
  zhipuai: 'zhipu',
  thudm: 'zhipu',
  minimax: 'minimax',
  minimaxai: 'minimax',
  perplexity: 'perplexity',
  'perplexity-ai': 'perplexity',
};

/** An organisation name (`owned_by`, a vendor prefix) as a maker, or null. */
export function makerFromOwner(owner: string | null | undefined): Maker | null {
  if (!owner) return null;
  const key = owner
    .trim()
    .toLowerCase()
    .replace(/[\s_]+/g, '-');
  return OWNERS[key] ?? OWNERS[key.replace(/-/g, '')] ?? null;
}

function matchesPrefix(id: string, prefix: string): boolean {
  if (!id.startsWith(prefix)) return false;
  if (/[-.]$/.test(prefix)) return true;
  const next = id.charAt(prefix.length);
  return next === '' || /[^a-z]/.test(next);
}

/**
 * A model id's maker, or null: `claude-sonnet-4-5`, Bedrock's `us.anthropic.claude-…` and
 * `meta.llama3-…`, Vertex's `publishers/google/models/gemini-…`, hub paths such as
 * `meta-llama/Llama-3.3-70B-Instruct` or `mistralai/Mixtral-8x7B`.
 */
export function makerFromModelId(id: string | null | undefined): Maker | null {
  if (!id) return null;
  const lower = id.trim().toLowerCase();
  if (!lower) return null;
  const segments = lower.split('/').filter(Boolean);
  // A vendor in the path: publishers/google/models/…, meta-llama/…, accounts/fireworks/models/….
  const publisher = segments.indexOf('publishers');
  if (publisher >= 0) {
    const m = makerFromOwner(segments[publisher + 1]);
    if (m) return m;
  }
  let last = segments.at(-1) ?? lower;
  // Bedrock: an optional region group, then the vendor, then the model (us.anthropic.claude-…).
  const bedrock = /^(?:(?:us|eu|apac|global|us-gov|jp|au|ca)\.)?([a-z0-9-]+)\.(.+)$/.exec(last);
  if (bedrock?.[1] && bedrock[2]) {
    const vendor = makerFromOwner(bedrock[1]);
    if (vendor) return vendor;
  }
  last = last.replace(/:.*$/, '');
  for (const [prefix, maker] of MODEL_PREFIXES) if (matchesPrefix(last, prefix)) return maker;
  // A hub owner before the model's name (mistralai/…, Qwen/…) when the name says nothing.
  for (const s of segments.slice(0, -1)) {
    const m = makerFromOwner(s);
    if (m) return m;
  }
  return null;
}

export interface ModelMakerHints {
  /** The model's `owned_by`. */
  ownedBy?: string | null | undefined;
  /** The model's `catalog.family`. */
  family?: string | null | undefined;
  /** Upstream model ids the model calls (a log record's, its deployments'), most specific first. */
  upstream?: readonly (string | null | undefined)[] | undefined;
  /** The model's own name, last. */
  name?: string | null | undefined;
}

/** The maker of a model, from its metadata first and its names last; null when nothing says. */
export function resolveModelMaker(hints: ModelMakerHints): Maker | null {
  return (
    makerFromOwner(hints.ownedBy) ??
    makerFromOwner(hints.family) ??
    makerFromModelId(hints.family) ??
    hints.upstream?.reduce<Maker | null>((found, id) => found ?? makerFromModelId(id), null) ??
    makerFromModelId(hints.name)
  );
}

/** Provider types and the brand their mark shows; generic types have none (a monogram). */
const PROVIDER_BRANDS: Record<string, BrandId> = {
  openai: 'openai',
  azure_openai: 'azure',
  anthropic: 'anthropic',
  bedrock: 'bedrock',
  vertex: 'vertex',
  gemini: 'gemini',
  mistral: 'mistral',
  cohere: 'cohere',
  groq: 'groq',
  together: 'together',
  fireworks: 'fireworks',
  deepseek: 'deepseek',
  xai: 'xai',
  cerebras: 'cerebras',
  deepinfra: 'deepinfra',
  perplexity: 'perplexity',
  huggingface: 'huggingface',
  replicate: 'replicate',
  sagemaker: 'aws',
  watsonx: 'ibm',
  ollama_native: 'ollama',
  triton: 'nvidia',
};

/** What a mark draws: a brand's mark, or a neutral monogram of one letter. */
export type MarkFace = { brand: BrandId; label: string } | { brand: null; letter: string; label: string };

function monogram(text: string | null | undefined, label: string): MarkFace {
  const letter = /[a-z0-9]/i.exec(text ?? '')?.[0]?.toUpperCase() ?? '?';
  return { brand: null, letter, label };
}

/** A provider type's mark: its brand, or a monogram of its label for generic and unknown types. */
export function providerFace(type: string | null | undefined): MarkFace {
  const label = providerTypeLabel(type ?? undefined);
  const brand = type ? PROVIDER_BRANDS[type] : undefined;
  return brand ? { brand, label } : monogram(label, label);
}

/** A model's mark: its maker's brand, or a monogram of its name when the maker is unknown. */
export function modelFace(hints: ModelMakerHints): MarkFace {
  const maker = resolveModelMaker(hints);
  if (maker) return { brand: MAKERS[maker].brand, label: MAKERS[maker].label };
  const owner = hints.ownedBy?.trim();
  if (owner) return monogram(owner, owner);
  return monogram(hints.name ?? hints.upstream?.find(Boolean), 'Unknown maker');
}
