import type { components } from '@/lib/api/schema';
import type { SafetyEvent } from '@/lib/api/types';

export type GuardrailSpec = components['schemas']['Guardrail'];
export type GuardrailType = GuardrailSpec['type'];
export type GuardrailAction = NonNullable<GuardrailSpec['action']>;
export type Severity = 'critical' | 'high' | 'medium' | 'low';
export type PiiEntity = NonNullable<NonNullable<GuardrailSpec['pii']>['entities']>[number];
export type PiiPreserve = NonNullable<NonNullable<GuardrailSpec['pii']>['preserve']>[number];
export type SecretDetector = NonNullable<NonNullable<GuardrailSpec['secrets']>['detectors']>[number];
export type Category = NonNullable<GuardrailSpec['categories']>[string];

export const PII_ENTITIES: PiiEntity[] = [
  'email',
  'phone',
  'credit_card',
  'us_ssn',
  'iban',
  'ip_address',
  'ca_sin',
];
export const PII_PRESERVE: PiiPreserve[] = ['email_domain', 'ip_subnet', 'last4', 'country'];
export const SECRET_DETECTORS: SecretDetector[] = [
  'openai_key',
  'anthropic_key',
  'aws_access_key',
  'github_token',
  'slack_token',
  'google_api_key',
  'stripe_key',
  'jwt',
  'pem_private_key',
];
export const CATEGORIES: Category[] = [
  'deny_pattern',
  'allow_pattern',
  'keyword',
  'pii',
  'secret',
  'hate',
  'harassment',
  'violence',
  'sexual',
  'self_harm',
  'illicit',
  'prompt_injection',
  'off_topic',
  'invalid_json',
  'schema_violation',
  'tool_not_allowed',
  'length',
  'uninspectable',
  'custom',
];

export const ENTITY_NAME: Record<string, string> = {
  email: 'Email address',
  phone: 'Phone number',
  credit_card: 'Card number',
  us_ssn: 'US social security number',
  iban: 'IBAN',
  ip_address: 'IP address',
  ca_sin: 'Canadian social insurance number',
  openai_key: 'OpenAI key',
  anthropic_key: 'Anthropic key',
  aws_access_key: 'AWS access key',
  github_token: 'GitHub token',
  slack_token: 'Slack token',
  google_api_key: 'Google API key',
  stripe_key: 'Stripe key',
  jwt: 'JSON web token',
  pem_private_key: 'Private key (PEM)',
};

/**
 * What a test match is, in words: the detector's entity, or "match" for a pattern, whose entity is
 * the label it would be redacted to (`redacted` by default) even when the rule only flags.
 */
export function matchWord(entity: string | null | undefined): string {
  if (!entity || entity === 'redacted') return 'match';
  return ENTITY_NAME[entity] ?? entity;
}

/** A name in running text: lower case, except names and acronyms ("US social security number"). */
export function inText(name: string): string {
  return /^([A-Z]{2}|Canadian|OpenAI|Anthropic|GitHub|Slack|Google|Stripe)/.test(name)
    ? name
    : name.charAt(0).toLowerCase() + name.slice(1);
}

export const PRESERVE_NAME: Record<PiiPreserve, string> = {
  email_domain: 'Email domain',
  ip_subnet: 'IP subnet',
  last4: 'Last four digits',
  country: 'Phone country code',
};

/**
 * The methods the rule editor offers, in the order of its picker; `allowlist` is a pattern rule
 * whose patterns are the only text allowed.
 */
export type Method =
  | 'regex'
  | 'allowlist'
  | 'keywords'
  | 'pii'
  | 'secrets'
  | 'classifier'
  | 'webhook'
  | 'topic'
  | 'llm_rewrite'
  | 'json_schema'
  | 'json_repair'
  | 'template'
  | 'decorator';

export const METHODS: { value: Method; label: string; description: string }[] = [
  {
    value: 'regex',
    label: 'Pattern',
    description: 'Regular expressions, with an optional checksum (Luhn or IBAN).',
  },
  {
    value: 'allowlist',
    label: 'Allow-list',
    description: 'Only text matching one of the patterns may pass.',
  },
  { value: 'keywords', label: 'Keywords', description: 'Words or phrases, matched whole or anywhere.' },
  {
    value: 'pii',
    label: 'Personal data',
    description: 'Emails, phone and card numbers, IBANs, ID numbers, addresses.',
  },
  {
    value: 'secrets',
    label: 'Secrets',
    description: 'API keys, tokens and private keys, with an optional entropy floor.',
  },
  {
    value: 'classifier',
    label: 'Classifier',
    description: 'A text-classification endpoint scoring each label.',
  },
  {
    value: 'webhook',
    label: 'Webhook',
    description: 'Your own service decides, for names, code or copyright checks.',
  },
  {
    value: 'topic',
    label: 'Topic restriction',
    description: 'Keep requests on allowed topics, or away from denied ones.',
  },
  {
    value: 'llm_rewrite',
    label: 'Rewrite',
    description: 'A Model rewrites the text before it goes on.',
  },
  {
    value: 'json_schema',
    label: 'JSON schema',
    description: 'Answers must match a schema; ask again when they don’t.',
  },
  {
    value: 'json_repair',
    label: 'JSON repair',
    description: 'Fix almost-valid JSON in answers.',
  },
  {
    value: 'template',
    label: 'Prompt templates',
    description: 'Named prompts filled from the request’s variables.',
  },
  {
    value: 'decorator',
    label: 'Prefix and suffix',
    description: 'Text added before and after the last user message.',
  },
];

/** Methods that change requests instead of judging them: no action, error policy or severity. */
export const TRANSFORMS: GuardrailType[] = ['system_prompt', 'decorator', 'template', 'json_repair'];

/** Methods whose findings may replace the matched text. */
const REDACTABLE: GuardrailType[] = [
  'regex',
  'keywords',
  'pii',
  'secrets',
  'webhook',
  'tool_permission',
  'bedrock_guardrails',
  'azure_content_safety',
  'model_armor',
  'lakera',
  'llm_rewrite',
];

/** The harmful-content family: what "output moderation" counts. */
export const MODERATION_TYPES: GuardrailType[] = [
  'moderation',
  'classifier',
  'llm_judge',
  'bedrock_guardrails',
  'azure_content_safety',
  'model_armor',
  'lakera',
];

export const TYPE_LABEL: Record<GuardrailType, string> = {
  regex: 'Pattern',
  keywords: 'Keywords',
  pii: 'Personal data',
  secrets: 'Secrets',
  moderation: 'Moderation',
  webhook: 'Webhook',
  system_prompt: 'System prompt',
  decorator: 'Prefix and suffix',
  llm_judge: 'Model judge',
  json_schema: 'JSON schema',
  tool_permission: 'Tool permissions',
  length: 'Length',
  classifier: 'Classifier',
  bedrock_guardrails: 'Bedrock guardrail',
  azure_content_safety: 'Azure content safety',
  model_armor: 'Model Armor',
  lakera: 'Lakera Guard',
  template: 'Prompt templates',
  llm_rewrite: 'Rewrite',
  json_repair: 'JSON repair',
  topic: 'Topic restriction',
};

/** The category a method reports, when it does not map its own labels. */
export function impliedCategory(spec: Pick<GuardrailSpec, 'type' | 'regex'>): Category | null {
  switch (spec.type) {
    case 'regex':
      return (spec.regex?.allow?.length ?? 0) > 0 ? 'allow_pattern' : 'deny_pattern';
    case 'keywords':
      return 'keyword';
    case 'pii':
      return 'pii';
    case 'secrets':
      return 'secret';
    case 'topic':
      return 'off_topic';
    case 'json_schema':
      return 'schema_violation';
    case 'tool_permission':
      return 'tool_not_allowed';
    case 'length':
      return 'length';
    case 'llm_judge':
      return 'custom';
    default:
      return null;
  }
}

export function methodOf(spec: Pick<GuardrailSpec, 'type' | 'regex' | 'decorator'>): Method | null {
  if (spec.type === 'regex') return (spec.regex?.allow?.length ?? 0) > 0 ? 'allowlist' : 'regex';
  if (spec.type === 'decorator') return 'decorator';
  return METHODS.some((m) => m.value === spec.type) ? (spec.type as Method) : null;
}

/** The stages a guardrail may run at: transforms, rewrites, topics and diverts read prompts only. */
export function stagesFor(
  type: GuardrailType,
  action: GuardrailAction | '' | undefined,
  execution?: GuardrailSpec['execution'],
): { input: boolean; output: boolean } {
  if (type === 'json_schema' || type === 'json_repair') return { input: false, output: true };
  if (
    ['system_prompt', 'decorator', 'template', 'llm_rewrite', 'topic'].includes(type) ||
    action === 'divert' ||
    execution === 'parallel_with_call'
  )
    return { input: true, output: false };
  return { input: true, output: true };
}

export function actionsFor(method: Method | null, type: GuardrailType): GuardrailAction[] {
  if (TRANSFORMS.includes(type)) return [];
  if (method === 'allowlist') return ['block', 'flag'];
  if (type === 'llm_rewrite') return ['redact'];
  const base: GuardrailAction[] = REDACTABLE.includes(type) ? ['block', 'redact', 'flag'] : ['block', 'flag'];
  return type === 'json_schema' ? base : [...base, 'divert'];
}

/** A form's view of a guardrail; lists are one entry per line, numbers are text until saved. */
export interface RuleDraft {
  name: string;
  method: Method | null;
  type: GuardrailType;
  enabled: boolean;
  action: GuardrailAction | '';
  severity: Severity | '';
  description: string;
  divertTo: string;
  patterns: string;
  validate: 'none' | 'luhn' | 'iban_mod97';
  label: string;
  words: string;
  match: 'word' | 'substring';
  entities: PiiEntity[];
  preserve: PiiPreserve[];
  replacement: 'label' | 'mask' | 'hash';
  reversible: boolean;
  detectors: SecretDetector[];
  minEntropy: string;
  url: string;
  webhookMode: 'segments' | 'body';
  thresholds: string;
  categories: string;
  model: string;
  prompt: string;
  allowedTopics: string;
  deniedTopics: string;
  threshold: string;
  schema: string;
  useRequestSchema: boolean;
  reask: string;
  templates: string;
  templateRequired: boolean;
  userPrefix: string;
  userSuffix: string;
  /** Run on prompts and on answers, at the gateway level (`policies.guardrails` input and output). */
  onInput: boolean;
  onOutput: boolean;
}

const lines = (text: string) =>
  text
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean);

function pairs(record: Record<string, unknown> | undefined): string {
  return Object.entries(record ?? {})
    .map(([k, v]) => `${k} = ${String(v)}`)
    .join('\n');
}

export function emptyDraft(method: Method = 'regex'): RuleDraft {
  return {
    name: '',
    method,
    type: method === 'allowlist' ? 'regex' : method,
    enabled: true,
    action:
      method === 'pii'
        ? 'redact'
        : method === 'llm_rewrite'
          ? 'redact'
          : ['json_repair', 'template', 'decorator'].includes(method)
            ? ''
            : 'block',
    severity: '',
    description: '',
    divertTo: '',
    patterns: '',
    validate: 'none',
    label: '',
    words: '',
    match: 'word',
    entities: [...PII_ENTITIES],
    preserve: [],
    replacement: 'label',
    reversible: false,
    detectors: [...SECRET_DETECTORS],
    minEntropy: '',
    url: '',
    webhookMode: 'segments',
    thresholds: '',
    categories: '',
    model: '',
    prompt: '',
    allowedTopics: '',
    deniedTopics: '',
    threshold: '0.5',
    schema: '',
    useRequestSchema: false,
    reask: '0',
    templates: '',
    templateRequired: false,
    userPrefix: '',
    userSuffix: '',
    onInput: !['json_schema', 'json_repair'].includes(method),
    onOutput: ['json_schema', 'json_repair'].includes(method),
  };
}

/** The form for an existing guardrail. */
export function draftOf(
  name: string,
  spec: GuardrailSpec,
  attached: { input: boolean; output: boolean },
): RuleDraft {
  const method = methodOf(spec);
  const d = emptyDraft(method ?? 'regex');
  const template = spec.template;
  return {
    ...d,
    name,
    method,
    type: spec.type,
    enabled: spec.enabled !== false,
    action: spec.action ?? (TRANSFORMS.includes(spec.type) ? '' : 'block'),
    severity: spec.severity ?? '',
    description: spec.description ?? '',
    divertTo: spec.divert_to ?? '',
    patterns: ((method === 'allowlist' ? spec.regex?.allow : spec.regex?.deny) ?? []).join('\n'),
    validate: spec.regex?.validate ?? 'none',
    label: spec.regex?.label ?? '',
    words: (spec.keywords?.words ?? []).join('\n'),
    match: spec.keywords?.match ?? 'word',
    entities: spec.pii?.entities ?? [...PII_ENTITIES],
    preserve: spec.pii?.preserve ?? [],
    replacement: spec.replacement ?? 'label',
    reversible: spec.pii?.reversible ?? false,
    detectors: spec.secrets?.detectors ?? [...SECRET_DETECTORS],
    minEntropy: spec.secrets?.min_entropy != null ? String(spec.secrets.min_entropy) : '',
    url: spec.classifier?.url ?? spec.webhook?.url ?? '',
    webhookMode: spec.webhook?.mode ?? 'segments',
    thresholds: pairs(spec.classifier?.thresholds),
    categories: pairs(spec.categories),
    model: spec.llm_rewrite?.model ?? spec.topic?.embedding_model ?? '',
    prompt: spec.llm_rewrite?.prompt ?? '',
    allowedTopics: (spec.topic?.allowed ?? []).join('\n'),
    deniedTopics: (spec.topic?.denied ?? []).join('\n'),
    threshold: spec.topic?.threshold != null ? String(spec.topic.threshold) : '0.5',
    schema: spec.json_schema?.schema !== undefined ? JSON.stringify(spec.json_schema.schema, null, 2) : '',
    useRequestSchema: spec.json_schema?.use_request_schema ?? false,
    reask: String(spec.json_schema?.reask ?? 0),
    templates: template ? JSON.stringify(template.templates, null, 2) : '',
    templateRequired: template?.required ?? false,
    userPrefix: spec.decorator?.user_prefix ?? '',
    userSuffix: spec.decorator?.user_suffix ?? '',
    onInput: attached.input,
    onOutput: attached.output,
  };
}

export type DraftErrors = Partial<Record<keyof RuleDraft, string>>;

function parsePairs(
  text: string,
  value: (v: string) => unknown,
): { ok: true; map: Record<string, unknown> } | { ok: false; line: string } {
  const map: Record<string, unknown> = {};
  for (const line of lines(text)) {
    const m = /^([^=:]+)\s*[=:]\s*(.+)$/.exec(line);
    const parsed = m?.[2] !== undefined ? value(m[2].trim()) : undefined;
    if (!m?.[1] || parsed === undefined) return { ok: false, line };
    map[m[1].trim()] = parsed;
  }
  return { ok: true, map };
}

function ratio(v: string): number | undefined {
  const n = Number(v);
  return Number.isFinite(n) && n >= 0 && n <= 1 ? n : undefined;
}

function category(v: string): Category | undefined {
  return CATEGORIES.find((c) => c === v);
}

function validRegex(p: string): boolean {
  try {
    new RegExp(p);
    return true;
  } catch {
    return false;
  }
}

/**
 * The guardrail a form describes, or what to fix. Settings the form does not show (another
 * method's block, execution, scope, timeouts) are kept from `base`.
 */
export function specOf(
  draft: RuleDraft,
  base: GuardrailSpec | null,
): { ok: true; spec: GuardrailSpec } | { ok: false; errors: DraftErrors } {
  const errors: DraftErrors = {};
  const type = draft.type;
  const known = draft.method !== null;
  const owned = [
    'regex',
    'keywords',
    'pii',
    'secrets',
    'classifier',
    'webhook',
    'topic',
    'llm_rewrite',
    'json_schema',
    'json_repair',
    'template',
    'decorator',
    'categories',
    'divert_to',
    'replacement',
  ];
  const keep = Object.fromEntries(
    Object.entries(base ?? {}).filter(([k]) => !(known && owned.includes(k))),
  ) as Partial<GuardrailSpec>;
  const spec: GuardrailSpec = { ...keep, type, enabled: draft.enabled };
  if (base === null && !/^[a-z][a-z0-9_]{0,62}$/.test(draft.name))
    errors.name = 'Use lowercase letters, digits and underscores, starting with a letter';
  const transform = TRANSFORMS.includes(type);
  if (transform) {
    delete spec.action;
    delete spec.on_error;
    delete spec.execution;
  } else if (draft.action) {
    spec.action = draft.action;
    if (draft.action === 'flag') spec.on_error = 'allow';
  }
  if (draft.severity) spec.severity = draft.severity;
  else delete spec.severity;
  if (draft.description.trim()) spec.description = draft.description.trim();
  else delete spec.description;
  if (draft.action === 'divert') {
    if (!draft.divertTo.trim()) errors.divertTo = 'Choose the Model that answers instead';
    else spec.divert_to = draft.divertTo.trim();
  }
  switch (draft.method) {
    case 'regex':
    case 'allowlist': {
      const list = lines(draft.patterns);
      if (list.length === 0) errors.patterns = 'Enter at least one pattern, one per line';
      else if (list.some((p) => !validRegex(p)))
        errors.patterns = `Fix this pattern: ${list.find((p) => !validRegex(p)) ?? ''}`;
      spec.regex = {
        ...(draft.method === 'allowlist' ? { allow: list } : { deny: list }),
        ...(draft.label.trim() ? { label: draft.label.trim() } : {}),
        ...(draft.validate !== 'none' ? { validate: draft.validate } : {}),
      };
      if (draft.action === 'redact' && draft.replacement !== 'label') spec.replacement = draft.replacement;
      break;
    }
    case 'keywords': {
      const words = lines(draft.words);
      if (words.length === 0) errors.words = 'Enter at least one word or phrase, one per line';
      spec.keywords = { words, match: draft.match };
      if (draft.action === 'redact' && draft.replacement !== 'label') spec.replacement = draft.replacement;
      break;
    }
    case 'pii':
      if (draft.entities.length === 0) errors.entities = 'Choose at least one kind of personal data';
      spec.pii = {
        entities: draft.entities,
        ...(draft.preserve.length > 0 ? { preserve: draft.preserve } : {}),
        ...(draft.reversible ? { reversible: true } : {}),
      };
      if (draft.replacement !== 'label') spec.replacement = draft.replacement;
      if (
        draft.reversible &&
        (draft.action !== 'redact' || draft.replacement !== 'label' || draft.preserve.length > 0)
      )
        errors.reversible = 'Reversible redaction needs the redact action, labels and nothing kept';
      break;
    case 'secrets': {
      if (draft.detectors.length === 0) errors.detectors = 'Choose at least one kind of secret';
      const entropy = draft.minEntropy.trim() === '' ? null : Number(draft.minEntropy);
      if (entropy !== null && !(Number.isFinite(entropy) && entropy >= 0 && entropy <= 8))
        errors.minEntropy = 'Enter a number from 0 to 8, or leave it empty';
      spec.secrets = { detectors: draft.detectors, ...(entropy !== null ? { min_entropy: entropy } : {}) };
      if (draft.action === 'redact' && draft.replacement !== 'label') spec.replacement = draft.replacement;
      break;
    }
    case 'classifier': {
      if (!draft.url.trim().startsWith('https://')) errors.url = 'Enter the endpoint’s https:// address';
      const thresholds = parsePairs(draft.thresholds, ratio);
      if (!thresholds.ok)
        errors.thresholds = `Write each line as label = a score from 0 to 1 (${thresholds.line})`;
      else if (Object.keys(thresholds.map).length === 0)
        errors.thresholds = 'Enter at least one label = score';
      const cats = parsePairs(draft.categories, category);
      if (!cats.ok) errors.categories = `Write each line as label = category from the list (${cats.line})`;
      spec.classifier = {
        ...(base?.classifier ?? {}),
        url: draft.url.trim(),
        thresholds: thresholds.ok ? (thresholds.map as Record<string, number>) : {},
      };
      if (cats.ok && Object.keys(cats.map).length > 0) spec.categories = cats.map as Record<string, Category>;
      break;
    }
    case 'webhook': {
      if (!draft.url.trim().startsWith('https://')) errors.url = 'Enter the service’s https:// address';
      const cats = parsePairs(draft.categories, category);
      if (!cats.ok) errors.categories = `Write each line as label = category from the list (${cats.line})`;
      spec.webhook = { ...(base?.webhook ?? {}), url: draft.url.trim(), mode: draft.webhookMode };
      if (cats.ok && Object.keys(cats.map).length > 0) spec.categories = cats.map as Record<string, Category>;
      if (draft.action === 'redact' && draft.replacement !== 'label') spec.replacement = draft.replacement;
      break;
    }
    case 'topic': {
      if (!draft.model.trim()) errors.model = 'Choose the embeddings Model that compares topics';
      const allowed = lines(draft.allowedTopics);
      const denied = lines(draft.deniedTopics);
      if (allowed.length + denied.length === 0)
        errors.allowedTopics = 'Describe at least one allowed or denied topic, one per line';
      const t = ratio(draft.threshold);
      if (t === undefined) errors.threshold = 'Enter a similarity from 0 to 1';
      spec.topic = { embedding_model: draft.model.trim(), allowed, denied, threshold: t ?? 0.5 };
      break;
    }
    case 'llm_rewrite':
      if (!draft.model.trim()) errors.model = 'Choose the Model that rewrites';
      if (!draft.prompt.trim()) errors.prompt = 'Tell the Model how to rewrite the text';
      spec.llm_rewrite = {
        ...(base?.llm_rewrite ?? {}),
        model: draft.model.trim(),
        prompt: draft.prompt.trim(),
      };
      spec.action = 'redact';
      spec.execution = 'blocking';
      break;
    case 'json_schema': {
      const reask = Number(draft.reask);
      if (!Number.isInteger(reask) || reask < 0 || reask > 5)
        errors.reask = 'Enter a whole number from 0 to 5';
      let schema: unknown;
      if (!draft.useRequestSchema) {
        try {
          schema = JSON.parse(draft.schema) as unknown;
          if (typeof schema !== 'object' && typeof schema !== 'boolean') throw new Error('not a schema');
        } catch {
          errors.schema = 'Paste a JSON Schema object';
        }
      }
      spec.json_schema = {
        ...(draft.useRequestSchema
          ? { use_request_schema: true }
          : { schema: schema as Record<string, unknown> }),
        ...(reask > 0 ? { reask } : {}),
      };
      break;
    }
    case 'json_repair':
      spec.json_repair = {};
      break;
    case 'template': {
      let templates: unknown;
      try {
        templates = JSON.parse(draft.templates) as unknown;
        if (
          typeof templates !== 'object' ||
          templates === null ||
          Array.isArray(templates) ||
          Object.keys(templates).length === 0
        )
          throw new Error('not a map');
      } catch {
        errors.templates = 'Paste a JSON object of named templates, each with its messages';
      }
      spec.template = {
        templates: (templates ?? {}) as NonNullable<GuardrailSpec['template']>['templates'],
        ...(draft.templateRequired ? { required: true } : {}),
      };
      break;
    }
    case 'decorator':
      if (!draft.userPrefix.trim() && !draft.userSuffix.trim())
        errors.userPrefix = 'Enter a prefix, a suffix or both';
      spec.decorator = {
        ...(base?.decorator ?? {}),
        ...(draft.userPrefix.trim() ? { user_prefix: draft.userPrefix } : {}),
        ...(draft.userSuffix.trim() ? { user_suffix: draft.userSuffix } : {}),
      };
      if (!draft.userPrefix.trim()) delete spec.decorator.user_prefix;
      if (!draft.userSuffix.trim()) delete spec.decorator.user_suffix;
      break;
    case null:
      break;
  }
  return Object.keys(errors).length > 0 ? { ok: false, errors } : { ok: true, spec };
}

/**
 * "Allow similar" and "Block similar" as a prepared edit of the guardrail that produced an event:
 * the change in words and the form with it applied, to review and save.
 */
export function prepareSimilar(
  kind: 'allow' | 'block',
  event: Pick<SafetyEvent, 'entities' | 'action'>,
  draft: RuleDraft,
): { draft: RuleDraft; change: string } | null {
  const found = (event.entities ?? []).map((e) => e.type);
  if (kind === 'allow') {
    if (draft.type === 'pii') {
      const drop = draft.entities.filter((e) => found.includes(e));
      const left = draft.entities.filter((e) => !found.includes(e));
      if (drop.length > 0 && left.length > 0)
        return {
          draft: { ...draft, entities: left },
          change: `Stop checking for ${drop.map((e) => inText(ENTITY_NAME[e] ?? e)).join(', ')}.`,
        };
    }
    if (draft.type === 'secrets') {
      const drop = draft.detectors.filter((d) => found.includes(d));
      const left = draft.detectors.filter((d) => !found.includes(d));
      if (drop.length > 0 && left.length > 0)
        return {
          draft: { ...draft, detectors: left },
          change: `Stop checking for ${drop.map((d) => ENTITY_NAME[d] ?? d).join(', ')}.`,
        };
    }
    if (draft.action === 'flag' || !draft.action) return null;
    return {
      draft: { ...draft, action: 'flag' },
      change: 'Let similar requests through and only flag them, instead of blocking or changing them.',
    };
  }
  if (draft.action === 'block' || !draft.action) return null;
  const entities =
    draft.type === 'pii'
      ? [...new Set([...draft.entities, ...PII_ENTITIES.filter((e) => found.includes(e))])]
      : draft.entities;
  return {
    draft: { ...draft, action: 'block', entities, reversible: false },
    change: 'Block similar requests instead of letting them through.',
  };
}

/** What a PII value looks like after redaction, with the parts the guardrail keeps. */
export function maskedSample(
  entity: string,
  replacement: 'label' | 'mask' | 'hash',
  preserve: PiiPreserve[],
): string {
  const label = `[${entity.toUpperCase()}]`;
  const base = replacement === 'hash' ? '#3f9a1c…' : replacement === 'mask' ? '••••••' : label;
  switch (entity) {
    case 'email':
      return preserve.includes('email_domain')
        ? `${replacement === 'label' ? '[EMAIL]' : base}@example.com`
        : base;
    case 'credit_card':
      return preserve.includes('last4')
        ? `${replacement === 'label' ? '[CREDIT_CARD]' : '•••• •••• ••••'} 4242`
        : base;
    case 'ip_address':
      return preserve.includes('ip_subnet') ? '203.0.113.x' : base;
    case 'phone':
      return preserve.includes('country') ? `+1 ${replacement === 'label' ? '[PHONE]' : '••• •••••'}` : base;
    case 'iban':
    case 'us_ssn':
    case 'ca_sin':
      return preserve.includes('last4') ? `${replacement === 'label' ? label : '••••'} 6789` : base;
    default:
      return base;
  }
}

/** How each kind of data is found. */
export function detectionMethod(entity: string, minEntropy?: number | null): string {
  switch (entity) {
    case 'credit_card':
    case 'ca_sin':
      return 'Pattern and Luhn checksum';
    case 'iban':
      return 'Pattern and mod-97 checksum';
    case 'ip_address':
      return 'Address parser';
    case 'email':
    case 'phone':
    case 'us_ssn':
      return 'Pattern';
    default:
      return minEntropy != null ? `Prefix and entropy ≥ ${minEntropy}` : 'Known prefix';
  }
}

export type Strictness = 'loose' | 'balanced' | 'strict';

/** The strictness presets over a PII guardrail's entities, kept parts and replacement. */
export const STRICTNESS: Record<
  Strictness,
  {
    label: string;
    entities: PiiEntity[];
    preserve: PiiPreserve[];
    replacement: 'label' | 'mask';
    summary: string;
  }
> = {
  loose: {
    label: 'Loose',
    entities: ['email', 'credit_card', 'us_ssn', 'iban'],
    preserve: ['email_domain', 'last4'],
    replacement: 'mask',
    summary:
      'Direct identifiers and financial numbers only; email domains and last four digits stay readable.',
  },
  balanced: {
    label: 'Balanced',
    entities: ['email', 'phone', 'credit_card', 'us_ssn', 'iban', 'ca_sin'],
    preserve: ['last4'],
    replacement: 'label',
    summary: 'Every identifier but IP addresses; values become labels, last four digits stay readable.',
  },
  strict: {
    label: 'Strict',
    entities: [...PII_ENTITIES],
    preserve: [],
    replacement: 'label',
    summary: 'Every kind of personal data, including IP addresses; nothing of a value is kept.',
  },
};

function sameSet<T>(a: readonly T[], b: readonly T[]): boolean {
  return a.length === b.length && a.every((x) => b.includes(x));
}

export function strictnessOf(spec: GuardrailSpec): Strictness | null {
  const entities = spec.pii?.entities ?? PII_ENTITIES;
  const preserve = spec.pii?.preserve ?? [];
  const replacement = spec.replacement ?? 'label';
  for (const [key, p] of Object.entries(STRICTNESS) as [Strictness, (typeof STRICTNESS)[Strictness]][])
    if (sameSet(entities, p.entities) && sameSet(preserve, p.preserve) && replacement === p.replacement)
      return key;
  return null;
}

/** What applying a preset changes, in words; empty when nothing does. */
export function strictnessChanges(spec: GuardrailSpec, to: Strictness): string[] {
  const p = STRICTNESS[to];
  const entities = spec.pii?.entities ?? PII_ENTITIES;
  const preserve = spec.pii?.preserve ?? [];
  const replacement = spec.replacement ?? 'label';
  const out: string[] = [];
  const name = (e: string) => inText(ENTITY_NAME[e] ?? e);
  const added = p.entities.filter((e) => !entities.includes(e));
  const removed = entities.filter((e) => !p.entities.includes(e));
  if (added.length > 0) out.push(`Starts checking for ${added.map(name).join(', ')}`);
  if (removed.length > 0) out.push(`Stops checking for ${removed.map(name).join(', ')}`);
  const keep = p.preserve.filter((x) => !preserve.includes(x));
  const drop = preserve.filter((x) => !p.preserve.includes(x));
  if (keep.length > 0) out.push(`Keeps ${keep.map((x) => inText(PRESERVE_NAME[x])).join(', ')} readable`);
  if (drop.length > 0) out.push(`No longer keeps ${drop.map((x) => inText(PRESERVE_NAME[x])).join(', ')}`);
  if (replacement !== p.replacement)
    out.push(
      p.replacement === 'mask'
        ? 'Masks values instead of labelling them'
        : 'Labels values instead of masking them',
    );
  if (spec.pii?.reversible && (p.replacement !== 'label' || p.preserve.length > 0))
    out.push('Turns reversible redaction off (it needs labels and nothing kept)');
  return out;
}

export function applyStrictness(spec: GuardrailSpec, to: Strictness): GuardrailSpec {
  const p = STRICTNESS[to];
  const reversible = Boolean(spec.pii?.reversible) && p.replacement === 'label' && p.preserve.length === 0;
  const next: GuardrailSpec = {
    ...spec,
    pii: {
      entities: p.entities,
      ...(p.preserve.length > 0 ? { preserve: p.preserve } : {}),
      ...(reversible ? { reversible } : {}),
    },
  };
  if (p.replacement === 'label') delete next.replacement;
  else next.replacement = p.replacement;
  return next;
}
