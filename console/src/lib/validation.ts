import { z } from 'zod';

import { ApiError } from './api/client';

export type FieldErrors = Record<string, string>;

export type Validated<T> = { ok: true; data: T } | { ok: false; errors: FieldErrors };

/** Validates form values with a zod schema; errors are keyed by the field path ("limits.rpm"). */
export function validate<T>(schema: z.ZodType<T>, values: unknown): Validated<T> {
  const result = schema.safeParse(values);
  if (result.success) return { ok: true, data: result.data };
  const errors: FieldErrors = {};
  for (const issue of result.error.issues) {
    const key = issue.path.map(String).join('.');
    if (!(key in errors)) errors[key] = issue.message;
  }
  return { ok: false, errors };
}

/**
 * Maps a 400 `validation_failed` from the gateway onto form fields: a detail at
 * `keys.ci-bot.allowed_ips[0]` lands on the field `allowed_ips`.
 */
export function serverFieldErrors(error: unknown, prefix: string): FieldErrors {
  if (!(error instanceof ApiError)) return {};
  const out: FieldErrors = {};
  for (const d of error.details) {
    if (!d.path.startsWith(prefix)) continue;
    const field = d.path.slice(prefix.length).replace(/^\./, '').split(/[.[]/)[0] ?? '';
    if (field && !(field in out)) out[field] = d.message;
  }
  return out;
}

/** Resource names: lowercase, digits, `-` and `_`, starting with a letter or digit. */
export const resourceName = z
  .string()
  .trim()
  .min(1, 'Required')
  .max(63, 'At most 63 characters')
  .regex(/^[a-z0-9][a-z0-9_-]*$/, 'Lowercase letters, digits, - and _; start with a letter or digit');

/** Model names allow dots, slashes and colons (`gpt-4o`, `meta/llama-3.1:8b`). */
export const modelName = z
  .string()
  .trim()
  .min(1, 'Required')
  .max(128, 'At most 128 characters')
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/, 'Letters, digits and . _ : / -');

export const cidr = z
  .string()
  .trim()
  .regex(
    /^(\d{1,3}(\.\d{1,3}){3}(\/\d{1,2})?|[0-9a-fA-F:]+(\/\d{1,3})?|@[a-z0-9_-]+)$/,
    'An IPv4/IPv6 address or CIDR, or @set',
  );

/** A money amount as the Admin API writes it: a decimal string. */
export const money = z
  .string()
  .trim()
  .regex(/^\d+(\.\d{1,6})?$/, 'A non-negative amount, up to 6 decimals');

export const duration = z
  .string()
  .trim()
  .regex(/^(\d+(ms|s|m|h|d|w))+$/, 'A duration such as 90d, 12h or 30m');

/** Splits a comma or newline separated field into trimmed values. */
export function splitList(value: string): string[] {
  return value
    .split(/[\n,]/)
    .map((v) => v.trim())
    .filter(Boolean);
}
