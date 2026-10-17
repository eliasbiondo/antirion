import { describe, expect, it } from 'vitest';

import { noteCountFits, noteTitle } from '../chart-notes';

describe('annotation counts', () => {
  it('draws a count only where it ends before the next flag and inside the plot', () => {
    expect(noteCountFits([100, 120, 200], [49, 10, 3], 400)).toEqual([false, true, true]);
    expect(noteCountFits([100, 160], [49, 1], 400)).toEqual([true, false]);
    expect(noteCountFits([395], [12], 400)).toEqual([false]);
  });

  it('names the first events in the tooltip', () => {
    expect(noteTitle(['a', 'b'])).toBe('a\nb');
    expect(noteTitle(['a', 'b', 'c', 'd', 'e'])).toBe('a\nb\nc\nand 2 more');
  });
});
