/**
 * Audit entries in words: an action code ("a2a.agents.create", "settings.policies.update",
 * "GET /admin/v1/auth/me/preferences") as a short sentence, and an actor as a name. Every screen
 * that lists audit entries uses these, so no raw code reaches a reader.
 */

const VERBS: Record<string, string> = {
  GET: 'Read',
  POST: 'Create',
  PUT: 'Replace',
  PATCH: 'Update',
  DELETE: 'Delete',
};

const PAST: Record<string, string> = {
  create: 'created',
  update: 'updated',
  patch: 'updated',
  replace: 'replaced',
  delete: 'deleted',
  adjust: 'adjusted',
  rotate: 'rotated',
  rotate_secret: 'secret rotated',
  rotate_secrets: 'secrets rotated',
  reset: 'reset',
  apply: 'applied',
  revoke: 'revoked',
  enable: 'enabled',
  disable: 'disabled',
  review: 'reviewed',
  invite: 'invited',
  offboard: 'offboarded',
  pause: 'paused',
  resume: 'resumed',
  approve: 'approved',
  deny: 'denied',
  cancel: 'cancelled',
  mint: 'made',
  send: 'sent',
  test: 'tested',
  resync: 'synced',
  secret_revealed: 'secret shown',
};

/** Whole codes whose words are not the sum of their parts. */
const EXACT: Record<string, string> = {
  'playground.mint': 'Playground key made',
  'playground.send': 'Playground request sent',
  'keys.secret_revealed': 'Secret shown',
  'keys.expiring': 'Expiry reminder sent',
  'keys.rotation_due': 'Rotation reminder sent',
  'keys.anomaly_flagged': 'Unusual key activity flagged',
  'keys.anomaly_dismissed': 'Unusual key activity dismissed',
  'config.apply': 'Configuration applied',
  'config.rollback': 'Configuration rolled back',
};

/** Product words and acronyms as written in copy. */
const NOUNS: Record<string, string> = {
  mcp: 'MCP',
  a2a: '',
  oidc: 'OIDC',
  saml: 'SAML',
  scim: 'SCIM',
  smtp: 'SMTP',
  api: 'API',
  role_bindings: 'role',
  alerts: 'alert',
  audit: 'audit',
};

function words(segment: string): string {
  return segment.replace(/[_-]/g, ' ');
}

function singular(noun: string): string {
  if (noun.endsWith('ies')) return `${noun.slice(0, -3)}y`;
  if (noun.endsWith('sses')) return noun.slice(0, -2);
  return noun.endsWith('s') && !noun.endsWith('ss') ? noun.slice(0, -1) : noun;
}

function noun(segment: string): string {
  return segment in NOUNS ? (NOUNS[segment] ?? '') : words(singular(segment));
}

function sentence(text: string): string {
  const t = text.replace(/\s+/g, ' ').trim();
  return t.charAt(0).toUpperCase() + t.slice(1);
}

/** "keys.create" → "Key created"; "a2a.agents.create" → "Agent created"; "GET /…/preferences" → "Read preferences". */
export function auditActionLabel(action: string): string {
  if (EXACT[action]) return EXACT[action];
  const http = /^(GET|POST|PUT|PATCH|DELETE)\s+(\S+)/.exec(action);
  if (http) {
    const [, method = 'GET', path = ''] = http;
    const raw = path
      .replace(/^\/admin\/v1\//, '')
      .split('/')
      .filter(Boolean);
    const lastRaw = raw[raw.length - 1] ?? path;
    const op = lastRaw.includes(':') ? lastRaw.split(':')[1] : undefined;
    const kinds = raw.map((p) => p.split(':')[0] ?? p).filter((p) => p && !p.startsWith('{'));
    return sentence(
      op
        ? `${words(op)} ${words(kinds.find((k) => k !== 'runtime') ?? '')}`
        : `${VERBS[method] ?? method} ${words(kinds[kinds.length - 1] ?? lastRaw)}`,
    );
  }
  const parts = action.split('.');
  if (parts.length >= 2 && parts.every((p) => /^[a-z0-9_]+$/.test(p))) {
    const verb = parts[parts.length - 1] ?? '';
    const subject = parts.slice(0, -1);
    const done = PAST[verb] ?? words(verb);
    // `settings.policies.update` → "Policies settings updated": a singleton keeps its plural name.
    if (subject[0] === 'settings' && subject.length === 2)
      return sentence(`${words(subject[1] ?? '')} settings ${done}`);
    return sentence(`${subject.map(noun).join(' ')} ${done}`);
  }
  return action;
}

/**
 * "bootstrap" → "Bootstrap token"; a system actor reads "The gateway (key reminders)"; a token
 * named by its id reads as whose token it is when `tokens` knows it ("ada's token"), else
 * "A token (wjqf…)". Every screen that says who did something uses it, so they read the same.
 */
export function auditActorLabel(
  actor: { type?: string | null; id: string | null },
  tokens: Record<string, string> = {},
): string {
  const id = actor.id ?? '';
  if (id === 'bootstrap') return 'Bootstrap token';
  if (actor.type === 'system') return `The gateway (${words(id)})`;
  const token = tokens[id];
  if (token) return token;
  if (actor.type === 'token') return `A token (${id.slice(0, 8)}…)`;
  return id || 'Someone';
}

/** The actor after "by": "by Bootstrap token", "by ada", "by the gateway (key reminders)". */
export function auditActorBy(
  actor: { type?: string | null; id: string | null },
  tokens: Record<string, string> = {},
): string {
  const label = auditActorLabel(actor, tokens);
  return /^(The gateway|A token)/.test(label) ? label.charAt(0).toLowerCase() + label.slice(1) : label;
}

/** The actor's kind, when it adds something to the label ("token", "user"). */
export function auditActorKind(actor: { type?: string | null; id: string }): string | null {
  if (actor.id === 'bootstrap' || actor.type === 'system') return null;
  return actor.type ?? null;
}

/** The top-level fields two specs differ in (an absent field and `null` are the same). */
function changedFields(before: unknown, after: unknown): string[] | null {
  const isObject = (v: unknown): v is Record<string, unknown> =>
    typeof v === 'object' && v !== null && !Array.isArray(v);
  if (!isObject(before) || !isObject(after)) return null;
  const keys = new Set([...Object.keys(before), ...Object.keys(after)]);
  return [...keys].filter((k) => JSON.stringify(before[k] ?? null) !== JSON.stringify(after[k] ?? null));
}

/**
 * Whether an edit only turned something on or off: `true` on, `false` off, `null` otherwise. The
 * change may name the `enabled` field itself, or be the whole spec before and after with only
 * `enabled` differing (an absent `enabled` is on).
 */
export function auditToggle(changes: unknown): boolean | null {
  if (!Array.isArray(changes) || changes.length === 0) return null;
  if (changes.length === 1) {
    const c = changes[0] as { before?: unknown; after?: unknown };
    const fields = changedFields(c.before, c.after);
    if (fields?.length === 1 && fields[0] === 'enabled')
      return (c.after as Record<string, unknown>).enabled !== false;
  }
  const toggles = changes.filter(
    (c): c is { path: string; after: boolean } =>
      typeof c === 'object' &&
      c !== null &&
      typeof (c as { path?: unknown }).path === 'string' &&
      /(^|\.)enabled$/.test((c as { path: string }).path) &&
      typeof (c as { after?: unknown }).after === 'boolean',
  );
  return toggles.length === changes.length ? (toggles[0]?.after ?? null) : null;
}

function names(v: unknown): string[] {
  return Array.isArray(v)
    ? v.flatMap((x) =>
        typeof x === 'string'
          ? [x]
          : typeof x === 'object' && x !== null && 'name' in x
            ? [String((x as { name: unknown }).name)]
            : [],
      )
    : [];
}

function dig(v: unknown, path: string[]): unknown {
  return path.reduce<unknown>(
    (acc, k) => (typeof acc === 'object' && acc !== null ? (acc as Record<string, unknown>)[k] : undefined),
    v,
  );
}

/** What was put in or taken out of the path of every request by a write to the gateway's policies. */
function attachmentWords(changes: unknown): string | null {
  if (!Array.isArray(changes) || changes.length !== 1) return null;
  const c = changes[0] as { before?: unknown; after?: unknown };
  const lists: { path: string[]; noun: string; where: string }[] = [
    { path: ['plugins', 'attach'], noun: 'Plugin', where: 'every request' },
    { path: ['guardrails', 'input'], noun: 'Guardrail', where: 'every request' },
    { path: ['guardrails', 'output'], noun: 'Guardrail', where: 'every answer' },
  ];
  const said: string[] = [];
  for (const l of lists) {
    const before = names(dig(c.before, l.path));
    const after = names(dig(c.after, l.path));
    for (const n of after.filter((x) => !before.includes(x)))
      said.push(`${l.noun} ${n} turned on for ${l.where}`);
    for (const n of before.filter((x) => !after.includes(x)))
      said.push(`${l.noun} ${n} taken off ${l.where}`);
  }
  if (said.length === 0) return null;
  const fields = changedFields(c.before, c.after) ?? [];
  if (fields.some((f) => f !== 'plugins' && f !== 'guardrails')) return null;
  return said.join('; ');
}

/**
 * An audit entry in words, from its action and, where they say more, its changes: turning
 * something on or off ("Plugin audit turned off") and attaching it to every request ("Plugin audit
 * turned on for every request") instead of "Plugin updated" or "Policies settings updated".
 */
export function auditEntryLabel(entry: {
  action: string;
  resource?: string | null;
  changes?: unknown;
}): string {
  if (entry.action === 'settings.policies.update') {
    const attached = attachmentWords(entry.changes);
    if (attached) return attached;
  }
  const edit = /^([a-z0-9_.]+)\.(update|patch|replace)$/.exec(entry.action);
  const toggled = edit ? auditToggle(entry.changes) : null;
  const name = entry.resource?.split('/').slice(1).join('/');
  if (edit && toggled !== null && name) {
    const subject = (edit[1] ?? '').split('.').map(noun).join(' ');
    return sentence(`${subject} ${name} turned ${toggled ? 'on' : 'off'}`);
  }
  return auditActionLabel(entry.action);
}
