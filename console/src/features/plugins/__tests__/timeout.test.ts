import { describe, expect, it } from 'vitest';

import { EMPTY_PLUGIN, pluginErrors } from '../form';

describe('plugin timeout', () => {
  const plugin = { ...EMPTY_PLUGIN, name: 'p', url: 'https://hooks.example.com/p' };

  it('is checked against the gateway limit of 30s in the form', () => {
    expect(pluginErrors({ ...plugin, timeout: '99s' }, [], true).timeout).toBe('At most 30s');
    expect(pluginErrors({ ...plugin, timeout: '1m' }, [], true).timeout).toBe('At most 30s');
    expect(pluginErrors({ ...plugin, timeout: '30s' }, [], true).timeout).toBeUndefined();
    expect(pluginErrors({ ...plugin, timeout: '500ms' }, [], true).timeout).toBeUndefined();
    expect(pluginErrors({ ...plugin, timeout: 'soon' }, [], true).timeout).toBe(
      'A duration such as 500ms, 30s, 5m or 1h',
    );
  });
});
