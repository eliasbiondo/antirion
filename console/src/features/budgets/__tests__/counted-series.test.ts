import { describe, expect, it } from 'vitest';

import { countedSeries } from '../sources';

describe('the counted budgets month', () => {
  it('adds adjustments as steps on their day and projects to the counters month end', () => {
    const { actual, projected, spent } = countedSeries([1, 2, 3, 4], [0, 100, 0, 0], 5, 3, 120);
    expect(actual).toEqual([1, 103, 106, null, null]);
    expect(spent).toBe(106);
    expect(projected).toEqual([null, null, 106, 113, 120]);
  });

  it('never projects below what is spent', () => {
    const { projected } = countedSeries([5], [0], 3, 1, 1);
    expect(projected).toEqual([5, 5, 5]);
  });
});
