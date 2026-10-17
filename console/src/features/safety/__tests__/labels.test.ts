import { describe, expect, it } from 'vitest';

import { bucketLabels } from '../chart';
import { moderationCheck } from '../posture';
import { matchWord } from '../rules';

describe('matchWord', () => {
  it('never calls a flagged pattern match redacted', () => {
    expect(matchWord('redacted')).toBe('match');
    expect(matchWord(null)).toBe('match');
    expect(matchWord('email')).toBe('Email address');
    expect(matchWord('IBAN_LABEL')).toBe('IBAN_LABEL');
  });
});

describe('bucketLabels', () => {
  it('keeps every label distinct over a week of 6-hour buckets', () => {
    const starts = Array.from({ length: 28 }, (_, i) =>
      new Date(Date.UTC(2026, 9, 9) + i * 6 * 3_600_000).toISOString(),
    );
    const labels = bucketLabels(starts, 'UTC');
    expect(new Set(labels).size).toBe(labels.length);
    expect(labels[0]).toBe('Oct 9 00:00');
  });

  it('shows the date alone for daily buckets and the clock within two days', () => {
    const daily = Array.from({ length: 7 }, (_, i) => new Date(Date.UTC(2026, 9, 9 + i)).toISOString());
    expect(bucketLabels(daily, 'UTC')[1]).toBe('Oct 10');
    const hourly = Array.from({ length: 3 }, (_, i) => new Date(Date.UTC(2026, 9, 9, i)).toISOString());
    expect(bucketLabels(hourly, 'UTC')).toEqual(['00:00', '01:00', '02:00']);
  });
});

describe('moderationCheck copy', () => {
  it('counts the Models it leaves out instead of an ellipsis before the period', () => {
    const models = Object.fromEntries(['a', 'b', 'c', 'd', 'e'].map((m) => [m, {}]));
    expect(moderationCheck({ models }).detail).toBe('5 of 5 public Models have none: a, b, c and 2 more.');
  });
});
