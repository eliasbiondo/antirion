import { describe, expect, it } from 'vitest';

import { bucketStarts } from '../components/overview-panels';
import { configChecks, generationsByDay, mfaCheck, rotationCheck, score } from '../posture';
import {
  actionsFor,
  applyStrictness,
  draftOf,
  emptyDraft,
  maskedSample,
  prepareSimilar,
  specOf,
  strictnessChanges,
  strictnessOf,
} from '../rules';

describe('rule drafts', () => {
  it('writes a pattern rule with a checksum and keeps settings the form does not show', () => {
    const draft = {
      ...emptyDraft('regex'),
      name: 'iban_numbers',
      patterns: 'GB\\d{2}\n[A-Z]{4}',
      validate: 'iban_mod97' as const,
      label: 'IBAN',
      severity: 'high' as const,
    };
    const r = specOf(draft, { type: 'regex', timeout: '500ms', regex: { deny: ['old'] } });
    expect(r.ok && r.spec).toEqual({
      type: 'regex',
      timeout: '500ms',
      enabled: true,
      action: 'block',
      severity: 'high',
      regex: { deny: ['GB\\d{2}', '[A-Z]{4}'], label: 'IBAN', validate: 'iban_mod97' },
    });
  });

  it('says what to fix instead of writing a broken rule', () => {
    const r = specOf({ ...emptyDraft('regex'), name: 'Bad Name', patterns: '([' }, null);
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.errors.name).toMatch(/lowercase/);
      expect(r.errors.patterns).toMatch(/Fix this pattern/);
    }
    const c = specOf(
      { ...emptyDraft('classifier'), name: 'c', url: 'http://x', thresholds: 'jailbreak = 2' },
      null,
    );
    expect(!c.ok && c.errors.url).toBeTruthy();
    expect(!c.ok && c.errors.thresholds).toMatch(/score from 0 to 1/);
  });

  it('writes an allow-list, keywords, secrets with entropy, and a classifier with categories', () => {
    expect(
      specOf({ ...emptyDraft('allowlist'), name: 'a', patterns: '^[a-z ]+$', action: 'flag' }, null),
    ).toMatchObject({
      ok: true,
      spec: { type: 'regex', action: 'flag', on_error: 'allow', regex: { allow: ['^[a-z ]+$'] } },
    });
    expect(actionsFor('allowlist', 'regex')).toEqual(['block', 'flag']);
    expect(
      specOf(
        { ...emptyDraft('keywords'), name: 'k', words: 'jailbreak\n developer mode ', match: 'substring' },
        null,
      ),
    ).toMatchObject({
      ok: true,
      spec: { keywords: { words: ['jailbreak', 'developer mode'], match: 'substring' } },
    });
    expect(
      specOf({ ...emptyDraft('secrets'), name: 's', detectors: ['aws_access_key'], minEntropy: '3.5' }, null),
    ).toMatchObject({
      ok: true,
      spec: { secrets: { detectors: ['aws_access_key'], min_entropy: 3.5 } },
    });
    expect(
      specOf(
        {
          ...emptyDraft('classifier'),
          name: 'c',
          url: 'https://cls.example/score',
          thresholds: 'jailbreak = 0.9',
          categories: 'jailbreak = prompt_injection',
        },
        null,
      ),
    ).toMatchObject({
      ok: true,
      spec: {
        classifier: { url: 'https://cls.example/score', thresholds: { jailbreak: 0.9 } },
        categories: { jailbreak: 'prompt_injection' },
      },
    });
  });

  it('offers topic restriction, rewrite, JSON repair, re-ask, templates, affixes and divert', () => {
    expect(
      specOf(
        {
          ...emptyDraft('topic'),
          name: 't',
          model: 'embed',
          allowedTopics: 'Billing questions',
          threshold: '0.6',
        },
        null,
      ),
    ).toMatchObject({
      ok: true,
      spec: {
        type: 'topic',
        topic: { embedding_model: 'embed', allowed: ['Billing questions'], denied: [], threshold: 0.6 },
      },
    });
    expect(
      specOf({ ...emptyDraft('llm_rewrite'), name: 'r', model: 'small', prompt: 'Remove names' }, null),
    ).toMatchObject({
      ok: true,
      spec: {
        type: 'llm_rewrite',
        action: 'redact',
        execution: 'blocking',
        llm_rewrite: { model: 'small', prompt: 'Remove names' },
      },
    });
    const repair = specOf({ ...emptyDraft('json_repair'), name: 'j' }, null);
    expect(repair.ok && repair.spec).toEqual({ type: 'json_repair', enabled: true, json_repair: {} });
    expect(
      specOf({ ...emptyDraft('json_schema'), name: 'js', useRequestSchema: true, reask: '2' }, null),
    ).toMatchObject({
      ok: true,
      spec: { json_schema: { use_request_schema: true, reask: 2 } },
    });
    expect(
      specOf(
        {
          ...emptyDraft('template'),
          name: 'tp',
          templates: '{"reply":{"messages":[{"role":"system","content":"Hi {{name}}"}]}}',
          templateRequired: true,
        },
        null,
      ),
    ).toMatchObject({
      ok: true,
      spec: { template: { required: true, templates: { reply: { messages: [{ role: 'system' }] } } } },
    });
    const decorator = specOf(
      { ...emptyDraft('decorator'), name: 'd', userSuffix: 'Answer in English.' },
      null,
    );
    expect(decorator.ok && decorator.spec).toEqual({
      type: 'decorator',
      enabled: true,
      decorator: { user_suffix: 'Answer in English.' },
    });
    expect(
      specOf({ ...emptyDraft('pii'), name: 'p', action: 'divert', divertTo: 'safe-model' }, null),
    ).toMatchObject({
      ok: true,
      spec: { action: 'divert', divert_to: 'safe-model' },
    });
    expect(actionsFor('json_repair', 'json_repair')).toEqual([]);
  });

  it('reads a guardrail back into the form', () => {
    const d = draftOf(
      'pii_gateway',
      {
        type: 'pii',
        action: 'redact',
        replacement: 'mask',
        pii: { entities: ['email'], preserve: ['email_domain'] },
      },
      { input: true, output: false },
    );
    expect(d).toMatchObject({
      method: 'pii',
      entities: ['email'],
      preserve: ['email_domain'],
      replacement: 'mask',
      onInput: true,
      onOutput: false,
    });
  });
});

describe('prepared edits', () => {
  const pii = draftOf('pii_gateway', { type: 'pii', action: 'redact' }, { input: true, output: false });
  it('allow similar stops checking the entity found, or lets requests through flagged', () => {
    const allow = prepareSimilar(
      'allow',
      { action: 'redact', entities: [{ type: 'ip_address', count: 1 }] },
      pii,
    );
    expect(allow?.draft.entities).not.toContain('ip_address');
    expect(allow?.change).toMatch(/IP address/);
    const regex = draftOf(
      'r',
      { type: 'regex', action: 'block', regex: { deny: ['x'] } },
      { input: true, output: false },
    );
    expect(prepareSimilar('allow', { action: 'block', entities: [] }, regex)?.draft.action).toBe('flag');
  });
  it('block similar blocks instead', () => {
    const block = prepareSimilar('block', { action: 'redact', entities: [{ type: 'email', count: 1 }] }, pii);
    expect(block?.draft.action).toBe('block');
    expect(
      prepareSimilar('block', { action: 'block', entities: [] }, { ...pii, action: 'block' }),
    ).toBeNull();
  });
});

describe('personal data presets', () => {
  it('names the preset a rule matches and what applying another changes', () => {
    const strict = applyStrictness({ type: 'pii', action: 'redact', replacement: 'mask' }, 'strict');
    expect(strictnessOf(strict)).toBe('strict');
    expect(strictnessChanges(strict, 'loose')).toEqual([
      'Stops checking for phone number, IP address, Canadian social insurance number',
      'Keeps email domain, last four digits readable',
      'Masks values instead of labelling them',
    ]);
    const reversible = { type: 'pii' as const, action: 'redact' as const, pii: { reversible: true } };
    expect(strictnessChanges(reversible, 'loose')).toContain(
      'Turns reversible redaction off (it needs labels and nothing kept)',
    );
    expect(applyStrictness(reversible, 'strict').pii?.reversible).toBe(true);
  });
  it('shows how a value looks after redaction', () => {
    expect(maskedSample('email', 'label', ['email_domain'])).toBe('[EMAIL]@example.com');
    expect(maskedSample('credit_card', 'mask', ['last4'])).toBe('•••• •••• •••• 4242');
    expect(maskedSample('ip_address', 'label', [])).toBe('[IP_ADDRESS]');
  });
});

describe('posture', () => {
  const doc = {
    policies: {
      guardrails: { input: ['pii', 'secrets'], output: ['mod'] },
      observability: { content: { enabled: false } },
    },
    organizations: { acme: { policies: { observability: { content: { enabled: true } } } } },
    models: { a: {}, b: { visibility: 'internal' }, c: { policies: { guardrails: { exclude: ['mod'] } } } },
    guardrails: {
      definitions: {
        pii: { type: 'pii' },
        secrets: { type: 'secrets', enabled: false },
        mod: { type: 'moderation', moderation: { model: 'omni' } },
      },
    },
  };
  it('checks the configuration', () => {
    const checks = configChecks(doc);
    expect(checks.map((c) => [c.id, c.pass])).toEqual([
      ['content', false],
      ['pii', true],
      ['secrets', false],
      ['moderation', false],
    ]);
    expect(checks[0]?.detail).toBe('acme keeps prompts and answers.');
    expect(checks[3]?.detail).toMatch(/1 of 2 public Models have none: c/);
  });
  it('checks people and keys, and scores what could be read', () => {
    const people = mfaCheck([
      { lastLogin: '2026-10-01T00:00:00Z', method: 'local', mfa: ['totp'] },
      { lastLogin: '2026-10-01T00:00:00Z', method: 'oidc', mfa: [] },
      { lastLogin: '2026-10-01T00:00:00Z', method: 'token', mfa: [] },
      { lastLogin: null, method: null, mfa: [] },
      { kind: 'service', lastLogin: '2026-10-01T00:00:00Z', method: 'token', mfa: [] },
    ]);
    expect(people.pass).toBe(false);
    expect(people.detail).toBe('1 of 3 people signed in without either.');
    expect(rotationCheck(null, 0).pass).toBeNull();
    expect(score([people, rotationCheck(0, 4), rotationCheck(null, 0)])).toBe(0.5);
  });
  it('leaves a check with nothing to check out of the score, as not applicable', () => {
    const nobody = mfaCheck([{ lastLogin: null, method: null, mfa: [] }]);
    expect(nobody).toMatchObject({ pass: null, notApplicable: true, detail: 'Nobody has signed in yet.' });
    const noKeys = rotationCheck(0, 0);
    expect(noKeys).toMatchObject({ pass: null, notApplicable: true });
    expect(score([nobody, noKeys, rotationCheck(0, 4)])).toBe(1);
  });
  it('takes the generation in force at the end of each day', () => {
    const now = Date.parse('2026-10-15T12:00:00Z');
    const byDay = generationsByDay(
      [
        { generation: 1, created_at: '2026-10-10T08:00:00Z' },
        { generation: 2, created_at: '2026-10-12T23:00:00Z' },
        { generation: 3, created_at: '2026-10-15T11:00:00Z' },
      ],
      now,
      7,
    );
    expect(byDay).toEqual([null, 1, 1, 2, 2, 2, 3]);
  });
  it('fills quiet buckets of a window', () => {
    expect(
      bucketStarts('2026-10-15T10:00:00Z', '2026-10-15T13:00:00Z', '1h', ['2026-10-15T11:00:00.000Z']),
    ).toEqual(['2026-10-15T10:00:00.000Z', '2026-10-15T11:00:00.000Z', '2026-10-15T12:00:00.000Z']);
  });
});
