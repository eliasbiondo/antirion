import { describe, expect, it } from 'vitest';

import { blockingStep } from '../components/rule-wizard';
import { blankDraft, describeDraft } from '../draft';

describe('routing wizard', () => {
  it('never ends the live sentence with an ellipsis and a dot', () => {
    const sentence = describeDraft(blankDraft());
    expect(sentence).not.toMatch(/…\.$/);
    expect(sentence.endsWith('…')).toBe(true);
  });

  it('stops Next on the first earlier step that has errors, and shows only those', () => {
    const errors = {
      name: 'Name the rule',
      targets: 'Choose at least one deployment',
      'limits.attempt': 'x',
    };
    expect(blockingStep(errors, 1)).toEqual({ step: 0, errors: { name: 'Name the rule' } });
    expect(blockingStep({ targets: errors.targets, 'limits.attempt': 'x' }, 3)).toEqual({
      step: 2,
      errors: { targets: errors.targets },
    });
    expect(blockingStep({ 'limits.attempt': 'x' }, 3)).toBeNull();
    expect(blockingStep({}, 2)).toBeNull();
  });
});
