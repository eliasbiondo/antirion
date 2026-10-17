/**
 * Hedged requests and response predicates: how the rule wizard and
 * the defaults panel read and write `policies.resilience.hedging` and `retry.retry_on_response`.
 */

import { isRecord, type Json } from './types';

export interface HedgingDraft {
  enabled: boolean;
  /** How long the first attempt runs before another starts, e.g. 2s. */
  delay: string;
  /** Extra attempts at most: 1 or 2. */
  maxExtra: '1' | '2';
}

export type PredicateKind = 'header' | 'finish_reason' | 'empty_content';

export interface PredicateDraft {
  kind: PredicateKind;
  header: string;
  equals: string;
  /** Finish reasons, comma-separated. */
  reasons: string;
}

export const PREDICATE_KINDS: { value: PredicateKind; label: string }[] = [
  { value: 'header', label: 'Header has a value' },
  { value: 'finish_reason', label: 'Finish reason is one of' },
  { value: 'empty_content', label: 'Answer is empty' },
];

export function blankHedging(): HedgingDraft {
  return { enabled: false, delay: '2s', maxExtra: '1' };
}

export function blankPredicate(kind: PredicateKind = 'header'): PredicateDraft {
  return { kind, header: '', equals: '', reasons: '' };
}

export function hedgingFrom(value: Json): HedgingDraft {
  if (!isRecord(value)) return blankHedging();
  return {
    enabled: value.enabled === true,
    delay: typeof value.delay === 'string' ? value.delay : '2s',
    maxExtra: value.max_extra === 2 ? '2' : '1',
  };
}

/** The `hedging` block, or undefined when hedging is off (its default). */
export function hedgingTo(d: HedgingDraft): Json {
  if (!d.enabled) return undefined;
  const delay = d.delay.trim() || '2s';
  return {
    enabled: true,
    ...(delay !== '2s' ? { delay } : {}),
    ...(d.maxExtra === '2' ? { max_extra: 2 } : {}),
  };
}

export function predicatesFrom(value: Json): PredicateDraft[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((p): PredicateDraft[] => {
    if (!isRecord(p)) return [];
    if (typeof p.header === 'string')
      return [
        {
          ...blankPredicate('header'),
          header: p.header,
          equals: typeof p.equals === 'string' ? p.equals : '',
        },
      ];
    if (Array.isArray(p.finish_reason))
      return [{ ...blankPredicate('finish_reason'), reasons: p.finish_reason.map(String).join(', ') }];
    if (p.empty_content === true) return [blankPredicate('empty_content')];
    return [];
  });
}

function reasonsOf(text: string): string[] {
  return text
    .split(',')
    .map((r) => r.trim())
    .filter(Boolean);
}

export function predicatesTo(list: PredicateDraft[]): Json[] {
  return list.map((p) =>
    p.kind === 'header'
      ? { header: p.header.trim(), equals: p.equals }
      : p.kind === 'finish_reason'
        ? { finish_reason: reasonsOf(p.reasons) }
        : { empty_content: true },
  );
}

/** A predicate in words: "header x-fail set to 1", "finish reason length or content_filter". */
export function predicateText(p: PredicateDraft): string {
  if (p.kind === 'header') return `header ${p.header.trim()} set to ${p.equals}`;
  if (p.kind === 'finish_reason') return `finish reason ${reasonsOf(p.reasons).join(' or ')}`;
  return 'no content';
}

/** Whether a predicate is filled in enough to describe. */
export function predicateComplete(p: PredicateDraft): boolean {
  if (p.kind === 'header') return p.header.trim() !== '' && p.equals !== '';
  if (p.kind === 'finish_reason') return reasonsOf(p.reasons).length > 0;
  return true;
}

const DURATION = /^([0-9]+(?:\.[0-9]+)?)(ms|s|m|h|d)$/;
const UNIT_MS: Record<string, number> = { ms: 1, s: 1000, m: 60_000, h: 3_600_000, d: 86_400_000 };
const HEADER = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/;

/** Milliseconds of a duration such as 250ms or 2s, or null when it isn't one. */
export function durationMs(text: string): number | null {
  const m = DURATION.exec(text.trim());
  return m?.[1] && m[2] ? Number(m[1]) * (UNIT_MS[m[2]] ?? 0) : null;
}

/** Errors of the hedging and predicate fields, keyed under `prefix`. */
export function validateExtras(
  hedging: HedgingDraft,
  predicates: PredicateDraft[],
  prefix: string,
): Record<string, string> {
  const out: Record<string, string> = {};
  if (hedging.enabled) {
    const ms = durationMs(hedging.delay);
    if (ms === null) out[`${prefix}hedging.delay`] = 'A number and a unit, e.g. 500ms or 2s';
    else if (ms < 10) out[`${prefix}hedging.delay`] = 'At least 10ms';
  }
  predicates.forEach((p, i) => {
    if (p.kind === 'header') {
      if (!HEADER.test(p.header.trim()))
        out[`${prefix}predicates.${i}`] = 'Enter the header name, e.g. x-fail';
      else if (p.equals === '') out[`${prefix}predicates.${i}`] = 'Enter the value that means failure';
    }
    if (p.kind === 'finish_reason' && reasonsOf(p.reasons).length === 0)
      out[`${prefix}predicates.${i}`] = 'Enter at least one finish reason, e.g. length';
  });
  return out;
}
