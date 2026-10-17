import type { components } from '@/lib/api/schema';

import type { ConfigDoc } from './api';

type Change = components['schemas']['Change'];

/** Members a stored version leaves out: the startup-only blocks and the document version. */
const IGNORED = new Set(['version', 'server', 'admin', 'store', 'cluster']);

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(sorted(a)) === JSON.stringify(sorted(b));
}

function sorted(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(sorted);
  if (!isObject(v)) return v;
  return Object.fromEntries(
    Object.keys(v)
      .sort()
      .map((k) => [k, sorted(v[k])]),
  );
}

function wrap(v: unknown): Record<string, unknown> | null {
  if (v === undefined) return null;
  return isObject(v) ? v : { value: v };
}

/**
 * What going from `from` to `to` changes, two levels deep: each resource of a collection
 * (`providers.openai`) and each member of a settings block (`governance.dual_control`). Both
 * documents come from the gateway with their secrets hidden the same way, so a secret that did not
 * change compares equal.
 */
export function compareDocuments(from: ConfigDoc, to: ConfigDoc): Change[] {
  const out: Change[] = [];
  const keys = [...new Set([...Object.keys(from), ...Object.keys(to)])].filter((k) => !IGNORED.has(k)).sort();
  for (const key of keys) {
    const a = from[key];
    const b = to[key];
    if (same(a, b)) continue;
    if (isObject(a) && isObject(b)) {
      for (const member of [...new Set([...Object.keys(a), ...Object.keys(b)])].sort()) {
        const x = a[member];
        const y = b[member];
        if (same(x, y)) continue;
        out.push({
          op: x === undefined ? 'create' : y === undefined ? 'delete' : 'update',
          path: `${key}.${member}`,
          before: wrap(x),
          after: wrap(y),
        });
      }
      continue;
    }
    out.push({
      op: a === undefined ? 'create' : b === undefined ? 'delete' : 'update',
      path: key,
      before: wrap(a),
      after: wrap(b),
    });
  }
  return out;
}
