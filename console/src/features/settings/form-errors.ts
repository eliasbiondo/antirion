import { ApiError } from '@/lib/api/client';
import type { FieldErrors } from '@/lib/validation';

/**
 * The gateway's validation details as form field errors: each detail whose configuration path
 * starts with one of `paths` (`auth.admin.local.password_policy.min_length`) lands on that field,
 * the most specific path first.
 */
export function detailErrors(error: unknown, paths: Record<string, string>): FieldErrors {
  if (!(error instanceof ApiError)) return {};
  const order = Object.keys(paths).sort((a, b) => b.length - a.length);
  const out: FieldErrors = {};
  for (const d of error.details) {
    const prefix = order.find(
      (p) => d.path === p || d.path.startsWith(`${p}.`) || d.path.startsWith(`${p}[`),
    );
    const field = prefix ? paths[prefix] : undefined;
    if (field && !(field in out)) out[field] = d.message.charAt(0).toUpperCase() + d.message.slice(1);
  }
  return out;
}

/** Comma, space or line separated values, trimmed, without blanks or repeats. */
export function splitList(text: string): string[] {
  return [
    ...new Set(
      text
        .split(/[\s,]+/)
        .map((v) => v.trim())
        .filter(Boolean),
    ),
  ];
}

/** Whether two JSON values are the same. */
export function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** A config duration ("15m", "1.5h", "7d"). */
export const DURATION_RE = /^[0-9]+(\.[0-9]+)?(ms|s|m|h|d)$/;

/** A configuration name: letters, digits, `.`, `_` and `-`, starting with a letter or digit. */
export const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;

/** An http(s) address, as the configuration's operator URLs take. */
export const URL_RE = /^https?:\/\/[^\s/?#]+([/?#]\S*)?$/i;

/**
 * A JSON merge patch that turns `before` into `after`: members `after` lacks become null, objects
 * are compared member by member, and the members named in `keep` (secrets the form leaves blank)
 * are left as they are.
 */
export function mergePatch(
  before: Record<string, unknown> | undefined,
  after: Record<string, unknown>,
  keep: string[] = [],
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const isObj = (v: unknown): v is Record<string, unknown> =>
    typeof v === 'object' && v !== null && !Array.isArray(v);
  for (const [k, v] of Object.entries(after)) {
    const old = before?.[k];
    if (isObj(v) && isObj(old)) {
      const inner = mergePatch(
        old,
        v,
        keep.filter((p) => p.startsWith(`${k}.`)).map((p) => p.slice(k.length + 1)),
      );
      if (Object.keys(inner).length) out[k] = inner;
    } else if (!same(v, old)) out[k] = v;
  }
  for (const k of Object.keys(before ?? {})) {
    if (!(k in after) && !keep.includes(k)) out[k] = null;
  }
  return out;
}

/** A configuration scalar as form text: strings and numbers as they are, anything else as `fallback`. */
export function str(value: unknown, fallback = ''): string {
  return typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean'
    ? String(value)
    : fallback;
}
