import { render, screen, within } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { useAlerts } from '@/features/alerts/store';
import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import { AnalyticsScreen } from '../components/analytics-screen';
import { useAnalytics } from '../store';

describe('Analytics screen', () => {
  beforeEach(() => {
    useAnalytics.setState({ usage: {} });
    useAlerts.setState({ alerts: emptySlot() });
    signInAs(['analytics:read', 'alerts:read']);
  });

  it('draws the month, the range figures and the breakdowns (contract mocks)', async () => {
    render(<AnalyticsScreen />);
    expect(await screen.findByText('Projected spend this month')).toBeInTheDocument();
    expect(await screen.findByRole('table', { name: 'Top models by spend' })).toBeInTheDocument();
    expect(screen.getByRole('table', { name: 'Provider health' })).toBeInTheDocument();
    expect(screen.getByRole('list', { name: 'Cache hit rate by team' })).toBeInTheDocument();
    const chart = screen.getByRole('group', { name: 'Cost per request against p95 latency, by model' });
    // Every model's bubble is a link to it, named with its figures, and labelled with its maker's mark.
    const bubbles = within(chart).getAllByRole('link');
    expect(bubbles.length).toBeGreaterThan(0);
    expect(bubbles[0]).toHaveAccessibleName(/per request, p95 .+ requests$/);
    expect(chart.querySelectorAll('[data-label]')).toHaveLength(
      chart.querySelectorAll('[data-model]').length,
    );
    // The busiest hours are square cells, the legend's swatches the same size.
    const cell = document.querySelector('[data-cell]');
    expect(cell).toHaveClass('aspect-square', 'h-(--cell)', 'w-(--cell)');
    expect(document.querySelector('[data-swatch]')).toHaveClass('h-(--cell)', 'w-(--cell)');
  });

  it('details the figures, shows signals by category and the pressure against the 80 % line', async () => {
    signInAs(['analytics:read', 'alerts:read', 'notifications:send']);
    render(<AnalyticsScreen />);
    expect(await screen.findByRole('region', { name: 'Details of the key figures' })).toBeInTheDocument();
    expect(screen.getByText('Most common fallback')).toBeInTheDocument();
    expect(await screen.findByRole('list', { name: 'Pressure by provider' })).toBeInTheDocument();
    expect(screen.getByText(/hot · \d+ vs/)).toBeInTheDocument();
    expect(await screen.findAllByRole('link', { name: 'Review requests' })).not.toHaveLength(0);
    expect(screen.getByRole('button', { name: 'Export CSV' })).toBeInTheDocument();
    expect(screen.getByText(/You don't have access to budgets/)).toBeInTheDocument();
  });

  it('keeps its layout with no requests: blank tiles, never zeros as figures', async () => {
    const empty = () => HttpResponse.json({ items: [], next_cursor: null });
    server.use(http.get('*/admin/v1/analytics/usage', empty));
    render(<AnalyticsScreen />);
    expect(await screen.findAllByText('No requests in this range')).not.toHaveLength(0);
    expect(screen.queryByText('$0.0000')).not.toBeInTheDocument();
  });

  it('says once, in plain words, that analytics is not available', async () => {
    server.use(
      http.get('*/admin/v1/analytics/usage', () =>
        HttpResponse.json({ error: { code: 'not_found', message: 'no route' } }, { status: 404 }),
      ),
    );
    render(<AnalyticsScreen />);
    expect(await screen.findByText("Analytics isn't available on this gateway yet")).toBeInTheDocument();
    expect(screen.queryByText('Projected spend this month')).not.toBeInTheDocument();
    // The endpoint is only for operators, behind a closed "Details".
    const endpoint = screen.queryByText(/\/analytics\/usage/);
    if (endpoint) expect(endpoint.closest('details')).not.toHaveAttribute('open');
  });
});
