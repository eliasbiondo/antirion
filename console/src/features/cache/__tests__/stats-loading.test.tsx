import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';

import { StatsTab } from '../components/stats-tab';
import { useCache } from '../store';

describe('cache statistics while loading', () => {
  it('keeps its sections, with placeholder rows, until the figures arrive', () => {
    useCache.setState({ stats: { ...emptySlot(), status: 'loading' }, status: emptySlot() });
    render(<StatsTab />);
    for (const title of ['Top saving teams', 'Provider prompt caching by prefix', 'Overrides'])
      expect(screen.getByRole('heading', { name: title })).toBeInTheDocument();
    expect(screen.getAllByRole('status').length).toBeGreaterThanOrEqual(3);
  });
});
