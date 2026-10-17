import { describe, expect, it } from 'vitest';

import { settingsScopeWords } from '../components/sections';

describe('gateway settings', () => {
  it('says whom the open section is for', () => {
    expect(settingsScopeWords('general', { kind: 'gateway' })).toBe('For the default organization');
    expect(settingsScopeWords('notifications', { kind: 'organization', name: 'acme' })).toBe('For acme');
    expect(settingsScopeWords('audit', { kind: 'gateway' })).toBe('For the whole gateway');
    expect(settingsScopeWords('danger', { kind: 'organization', name: 'acme' })).toBe(
      'For the whole gateway',
    );
  });
});
