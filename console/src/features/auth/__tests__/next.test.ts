import { describe, expect, it } from 'vitest';

import { safeNext } from '../next';

describe('safeNext', () => {
  it('keeps console paths', () => {
    expect(safeNext('/keys/?key=a')).toBe('/keys/?key=a');
    expect(safeNext('/')).toBe('/');
  });

  it('refuses anything a browser could read as another site', () => {
    for (const bad of [
      null,
      'https://evil.example',
      '//evil.example',
      '/\\evil.example',
      '/\t/evil.example',
      '/\n/evil.example',
      '/ /evil.example',
      'keys',
    ]) {
      expect(safeNext(bad)).toBe('/');
    }
  });
});
