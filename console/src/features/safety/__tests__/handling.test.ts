import { describe, expect, it } from 'vitest';

import type { GuardrailResource } from '@/lib/api/types';

import { handling, shares } from '../components/handling';

function guardrail(name: string, spec: Record<string, unknown>): GuardrailResource {
  return { name, spec, metadata: {} } as unknown as GuardrailResource;
}

describe('how an event was handled', () => {
  const guardrails = [
    guardrail('pii', { type: 'pii', action: 'redact', pii: { reversible: true } }),
    guardrail('plain-pii', { type: 'pii', action: 'redact' }),
    guardrail('topics', { type: 'topic', action: 'divert', divert_to: 'safe-model' }),
  ];

  it('names a reversible redaction', () => {
    expect(handling({ action: 'redact', guardrail: 'pii', stage: 'input' }, guardrails)?.title).toBe(
      'Reversible redaction',
    );
    expect(handling({ action: 'redact', guardrail: 'plain-pii', stage: 'input' }, guardrails)).toBeNull();
  });

  it('names the Model a diverted request was served by', () => {
    const note = handling({ action: 'divert', guardrail: 'topics', stage: 'input' }, guardrails);
    expect(note?.title).toBe('Served by safe-model');
    expect(handling({ action: 'divert', guardrail: 'gone', stage: 'input' }, null)?.title).toBe(
      'Served by the safe Model',
    );
    expect(handling({ action: 'block', guardrail: 'topics', stage: 'input' }, guardrails)).toBeNull();
  });
});

describe('topic shares', () => {
  it('turns counts into shares of the whole', () => {
    expect(
      shares([
        { topic: 'medical advice', events: 3 },
        { topic: 'outside allowed topics', events: 1 },
      ]),
    ).toEqual([
      { name: 'medical advice', value: 0.75 },
      { name: 'outside allowed topics', value: 0.25 },
    ]);
  });
});
