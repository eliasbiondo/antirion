import type { GuardrailSpec } from './rules';
import { MODERATION_TYPES } from './rules';

/** A configuration document read loosely: only the members the checks need. */
export type Doc = Record<string, unknown>;

type Obj = Record<string, unknown>;

function obj(v: unknown): Obj {
  return typeof v === 'object' && v !== null && !Array.isArray(v) ? (v as Obj) : {};
}

function names(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];
}

function guardrails(doc: Doc): Record<string, GuardrailSpec> {
  return obj(obj(doc.guardrails).definitions) as Record<string, GuardrailSpec>;
}

function enabledOfType(doc: Doc, list: string[], types: string[]): string[] {
  const defs = guardrails(doc);
  return list.filter((n) => {
    const g = defs[n];
    return g !== undefined && g.enabled !== false && types.includes(g.type);
  });
}

export type CheckId = 'content' | 'pii' | 'secrets' | 'moderation' | 'mfa' | 'rotation';

export interface Check {
  id: CheckId;
  label: string;
  /**
   * null when it could not be evaluated (the reader lacks the permission, or the read failed) or
   * does not apply; either way it is left out of the score.
   */
  pass: boolean | null;
  /** Nothing to check yet (nobody has signed in, there is no key): neither a pass nor a failure. */
  notApplicable?: boolean;
  detail: string;
}

export const CHECK_LABEL: Record<CheckId, string> = {
  content: 'Prompts aren’t stored, or zero data retention, in every organization',
  pii: 'A personal-data guardrail checks every prompt at the gateway level',
  secrets: 'A secrets guardrail checks every prompt at the gateway level',
  moderation: 'Answers of every public Model go through output moderation',
  mfa: 'Every person signs in with two-step verification or single sign-on',
  rotation: 'No key is past its rotation age',
};

/** Prompt content off or zero data retention, for each organization in the document. */
export function contentCheck(doc: Doc): Check {
  const gateway = obj(obj(doc.policies).observability);
  const orgs = { default: {}, ...obj(doc.organizations) };
  const storing: string[] = [];
  for (const [name, raw] of Object.entries(orgs)) {
    const own = obj(obj(obj(obj(raw).policies)).observability);
    const zdr = (own.zero_data_retention ?? gateway.zero_data_retention) === true;
    const content = (obj(own.content).enabled ?? obj(gateway.content).enabled) === true;
    if (content && !zdr) storing.push(name);
  }
  return {
    id: 'content',
    label: CHECK_LABEL.content,
    pass: storing.length === 0,
    detail:
      storing.length === 0
        ? 'No organization keeps prompts and answers.'
        : `${storing.join(', ')} ${storing.length === 1 ? 'keeps' : 'keep'} prompts and answers.`,
  };
}

function gatewayCheck(doc: Doc, id: 'pii' | 'secrets'): Check {
  const input = names(obj(obj(doc.policies).guardrails).input);
  const found = enabledOfType(doc, input, [id]);
  return {
    id,
    label: CHECK_LABEL[id],
    pass: found.length > 0,
    detail:
      found.length > 0 ? `${found.join(', ')} on prompts.` : 'None runs on prompts at the gateway level.',
  };
}

/** Each public, enabled Model has a moderation-family guardrail among its output guardrails. */
export function moderationCheck(doc: Doc): Check {
  const gatewayOut = names(obj(obj(doc.policies).guardrails).output);
  const models = Object.entries(obj(doc.models)).filter(([, m]) => {
    const spec = obj(m);
    return spec.visibility !== 'internal' && spec.enabled !== false;
  });
  const missing = models
    .filter(([, m]) => {
      const own = obj(obj(obj(m).policies).guardrails);
      const excluded = names(own.exclude);
      const out = [...gatewayOut, ...names(own.output)].filter((n) => !excluded.includes(n));
      return enabledOfType(doc, out, MODERATION_TYPES).length === 0;
    })
    .map(([n]) => n);
  return {
    id: 'moderation',
    label: CHECK_LABEL.moderation,
    pass: missing.length === 0,
    detail:
      models.length === 0
        ? 'There are no public Models.'
        : missing.length === 0
          ? models.length === 1
            ? 'The one public Model.'
            : `All ${models.length} public Models.`
          : `${missing.length} of ${models.length} public Models have none: ${missing.slice(0, 3).join(', ')}${missing.length > 3 ? ` and ${missing.length - 3} more` : ''}.`,
  };
}

export interface PersonFacts {
  kind?: string;
  enabled?: boolean;
  lastLogin: string | null;
  method: string | null;
  mfa: string[];
}

/** Every enabled person who has signed in used a second factor or single sign-on. */
export function mfaCheck(people: PersonFacts[] | null): Check {
  if (!people)
    return { id: 'mfa', label: CHECK_LABEL.mfa, pass: null, detail: 'Your role can’t read people.' };
  const signedIn = people.filter((p) => p.kind !== 'service' && p.enabled !== false && p.lastLogin);
  if (signedIn.length === 0)
    return {
      id: 'mfa',
      label: CHECK_LABEL.mfa,
      pass: null,
      notApplicable: true,
      detail: 'Nobody has signed in yet.',
    };
  const weak = signedIn.filter(
    (p) => p.mfa.length === 0 && !['oidc', 'oauth', 'saml'].includes(p.method ?? ''),
  ).length;
  return {
    id: 'mfa',
    label: CHECK_LABEL.mfa,
    pass: weak === 0,
    detail:
      weak === 0
        ? signedIn.length === 1
          ? 'The one person who signed in.'
          : `All ${signedIn.length} people who signed in.`
        : `${weak} of ${signedIn.length} people signed in without either.`,
  };
}

export function rotationCheck(due: number | null, total: number): Check {
  if (due === null)
    return { id: 'rotation', label: CHECK_LABEL.rotation, pass: null, detail: 'Your role can’t read keys.' };
  if (total === 0)
    return {
      id: 'rotation',
      label: CHECK_LABEL.rotation,
      pass: null,
      notApplicable: true,
      detail: 'There are no keys yet.',
    };
  return {
    id: 'rotation',
    label: CHECK_LABEL.rotation,
    pass: due === 0,
    detail:
      due === 0
        ? total === 1
          ? 'The one key.'
          : `All ${total} keys.`
        : `${due} ${due === 1 ? 'key needs' : 'keys need'} rotating.`,
  };
}

/** The configuration checks of one document. */
export function configChecks(doc: Doc): Check[] {
  return [contentCheck(doc), gatewayCheck(doc, 'pii'), gatewayCheck(doc, 'secrets'), moderationCheck(doc)];
}

/** The share of checks that pass among those that could be evaluated; null when none could. */
export function score(checks: Check[]): number | null {
  const known = checks.filter((c) => c.pass !== null);
  if (known.length === 0) return null;
  return known.filter((c) => c.pass).length / known.length;
}

export interface GenerationAt {
  generation: number;
  created_at: string;
}

/**
 * For each of the last `days` days (oldest first), the generation in force at the end of that day:
 * the newest one created before then; null before the first.
 */
export function generationsByDay(list: GenerationAt[], now: number, days = 30): (number | null)[] {
  const sorted = [...list].sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at));
  const dayEnd = (i: number) => {
    const d = new Date(now);
    d.setUTCHours(23, 59, 59, 999);
    return d.getTime() - (days - 1 - i) * 86_400_000;
  };
  return Array.from({ length: days }, (_, i) => {
    const end = Math.min(dayEnd(i), now);
    let found: number | null = null;
    for (const g of sorted) if (Date.parse(g.created_at) <= end) found = g.generation;
    return found;
  });
}
