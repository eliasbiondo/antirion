/** A stable colour class per provider type, as the mockups colour provider badges and bars. */
const COLORS: Record<string, string> = {
  openai: 'bg-[#10a37f]',
  azure_openai: 'bg-[#0078d4]',
  anthropic: 'bg-[#d97757]',
  bedrock: 'bg-[#ff9900]',
  vertex: 'bg-[#4285f4]',
  gemini: 'bg-[#4285f4]',
  mistral: 'bg-[#fa520f]',
  cohere: 'bg-[#39594d]',
  groq: 'bg-[#f55036]',
  together: 'bg-[#0f6fff]',
  fireworks: 'bg-[#6720ff]',
  deepseek: 'bg-[#4d6bfe]',
  xai: 'bg-[#6b6b6b]',
  openai_compatible: 'bg-[#5b8def]',
  anthropic_compatible: 'bg-[#c4704f]',
};

export function providerColor(type: string | undefined): string {
  return (type && COLORS[type]) ?? 'bg-dim';
}

const LABELS: Record<string, string> = {
  openai: 'OpenAI',
  azure_openai: 'Azure OpenAI',
  anthropic: 'Anthropic',
  bedrock: 'AWS Bedrock',
  vertex: 'Vertex AI',
  gemini: 'Google',
  mistral: 'Mistral',
  cohere: 'Cohere',
  groq: 'Groq',
  together: 'Together',
  fireworks: 'Fireworks',
  deepseek: 'DeepSeek',
  xai: 'xAI',
  openai_compatible: 'OpenAI-compatible',
  anthropic_compatible: 'Anthropic-compatible',
  cerebras: 'Cerebras',
  deepinfra: 'DeepInfra',
  perplexity: 'Perplexity',
  huggingface: 'Hugging Face',
  replicate: 'Replicate',
  databricks: 'Databricks',
  sagemaker: 'SageMaker',
  watsonx: 'watsonx',
  ollama_native: 'Ollama',
  triton: 'Triton',
};

export function providerTypeLabel(type: string | undefined): string {
  return (type && LABELS[type]) ?? type ?? 'unknown';
}
