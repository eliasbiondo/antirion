import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { useAnalytics } from '@/features/analytics/store';
import { useLogs } from '@/features/request-log/store';
import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import { OverviewScreen } from '../components/overview-screen';
import { useOverviewRuntime } from '../runtime';

const empty = () => HttpResponse.json({ items: [], next_cursor: null });

function newGateway(analytics: 'empty' | 'unavailable') {
  server.use(
    http.get('*/admin/v1/analytics/usage', () =>
      analytics === 'empty'
        ? empty()
        : HttpResponse.json({ error: { code: 'not_found', message: 'no route' } }, { status: 404 }),
    ),
    http.get('*/admin/v1/logs', empty),
    http.get('*/admin/v1/providers', empty),
    http.get('*/admin/v1/models', empty),
    http.get('*/admin/v1/keys', empty),
    http.get('*/admin/v1/runtime/providers', empty),
    http.get('*/admin/v1/runtime/budgets', empty),
  );
}

describe('Overview on a new gateway', () => {
  beforeEach(() => {
    useAnalytics.setState({ usage: {} });
    useLogs.setState({ records: emptySlot() });
    useOverviewRuntime.setState({ budgets: emptySlot(), providers: emptySlot() });
    signInAs(['logs:read', 'analytics:read', 'providers:read', 'models:read', 'keys:read']);
  });

  it('keeps its layout: blank tiles, a Get started checklist with real links, the requests table', async () => {
    newGateway('empty');
    render(<OverviewScreen />);
    expect(await screen.findByRole('heading', { name: 'Get started' })).toBeInTheDocument();
    expect(await screen.findAllByText('After the first request')).toHaveLength(6);
    expect(screen.queryByText('$0.00')).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Add provider' })).toHaveAttribute(
      'href',
      expect.stringContaining('/providers'),
    );
    expect(screen.getByRole('link', { name: 'Add model' })).toHaveAttribute(
      'href',
      expect.stringContaining('/models'),
    );
    expect(screen.getByRole('link', { name: 'Create key' })).toHaveAttribute(
      'href',
      expect.stringContaining('/keys'),
    );
    expect(screen.getByText(/curl .*\/v1\/chat\/completions/)).toBeInTheDocument();
    expect(await screen.findByText('No requests yet')).toBeInTheDocument();
  });

  it('says once, in plain words, that usage analytics is not available', async () => {
    newGateway('unavailable');
    render(<OverviewScreen />);
    expect(
      await screen.findByText("Usage analytics isn't available on this gateway yet"),
    ).toBeInTheDocument();
    expect(await screen.findAllByText('Not available')).toHaveLength(6);
    expect(screen.queryByText(/GET \/analytics/)).not.toBeInTheDocument();
  });
});

describe('Overview with traffic, budgets and provider readings', () => {
  beforeEach(() => {
    useAnalytics.setState({ usage: {} });
    useLogs.setState({ records: emptySlot() });
    useOverviewRuntime.setState({ budgets: emptySlot(), providers: emptySlot() });
    signInAs(['logs:read', 'analytics:read', 'providers:read', 'models:read', 'keys:read', 'policies:read']);
  });

  it('shows the month against the budget, team budgets and provider headroom', async () => {
    const start = new Date();
    const monthStart = new Date(Date.UTC(start.getUTCFullYear(), start.getUTCMonth(), 1));
    const nextMonth = new Date(Date.UTC(start.getUTCFullYear(), start.getUTCMonth() + 1, 1));
    const counter = (level: string, owner: string, amount: string, spent: string, state: string) => ({
      level,
      owner,
      name: 'monthly',
      partition: null,
      unit: 'usd',
      mode: 'soft',
      amount,
      effective_amount: amount,
      spent,
      reserved: '0',
      remaining: '0',
      period_start: monthStart.toISOString(),
      resets_at: nextMonth.toISOString(),
      state,
    });
    const bucket = (group: Record<string, string>, requests: number, cost: string) => ({
      start: monthStart.toISOString(),
      group,
      requests,
      errors: 0,
      cost_usd: cost,
      cache_saved_usd: '0',
    });
    server.use(
      http.get('*/admin/v1/analytics/usage', ({ request }) => {
        const by = new URL(request.url).searchParams.get('group_by');
        return HttpResponse.json({
          items:
            by === 'team'
              ? [bucket({ team: 'platform' }, 10, '12'), bucket({ team: 'data' }, 3, '2')]
              : by === 'provider'
                ? [bucket({ provider: 'openai' }, 10, '12')]
                : by === 'model'
                  ? [bucket({ model: 'gpt-4o' }, 10, '12')]
                  : [bucket({}, 13, '14')],
          next_cursor: null,
        });
      }),
      http.get('*/admin/v1/runtime/budgets', () =>
        HttpResponse.json({
          items: [
            counter('gateway', 'gateway', '100', '40', 'on_pace'),
            counter('team', 'platform', '15', '12', 'at_risk'),
          ],
          next_cursor: null,
        }),
      ),
      http.get('*/admin/v1/runtime/providers', () =>
        HttpResponse.json({
          items: [
            {
              name: 'openai',
              type: 'openai',
              credential: { method: 'api_key', state: 'valid' },
              account_tier: 'tier-3',
              rate_limits: {
                requests: { limit: 500, remaining: 50 },
                observed_at: start.toISOString(),
                stale: false,
              },
            },
          ],
          next_cursor: null,
        }),
      ),
      http.get('*/admin/v1/logs', empty),
      http.get('*/admin/v1/providers', empty),
      http.get('*/admin/v1/models', empty),
      http.get('*/admin/v1/keys', empty),
    );
    render(<OverviewScreen />);
    expect(await screen.findByText('$40.00')).toBeInTheDocument();
    expect(await screen.findByText(/of \$100(\.00)? budget/)).toBeInTheDocument();
    const teams = await screen.findByRole('list', { name: 'Spend by team this month' });
    expect(
      within(teams).getByRole('meter', { name: /platform: \$12\.00 of \$15\.00, at risk/ }),
    ).toBeInTheDocument();
    expect(
      within(teams).getByRole('meter', { name: /data: \$2\.00, no monthly budget/ }),
    ).toBeInTheDocument();
    const headroom = await screen.findByRole('list', { name: 'Provider headroom' });
    expect(within(headroom).getByText('tier-3', { exact: false })).toBeInTheDocument();
    expect(within(headroom).getByText('90%')).toBeInTheDocument();
    expect(screen.getByText('1 near the limit')).toBeInTheDocument();
  });

  it('filters the recent requests with the quick filters', async () => {
    const seen: string[] = [];
    server.use(
      http.get('*/admin/v1/analytics/usage', empty),
      http.get('*/admin/v1/runtime/budgets', empty),
      http.get('*/admin/v1/runtime/providers', empty),
      http.get('*/admin/v1/logs', ({ request }) => {
        seen.push(new URL(request.url).search);
        return empty();
      }),
      http.get('*/admin/v1/providers', empty),
      http.get('*/admin/v1/models', empty),
      http.get('*/admin/v1/keys', empty),
    );
    render(<OverviewScreen />);
    await userEvent.click(await screen.findByRole('button', { name: 'Errors only' }));
    await userEvent.click(screen.getByRole('button', { name: 'Flagged' }));
    expect(seen.at(-1)).toContain('status_class=4xx');
    expect(seen.at(-1)).toContain('flagged=true');
    expect(screen.getByRole('link', { name: /View all/ })).toHaveAttribute(
      'href',
      expect.stringMatching(/status=4xx.*flagged=1|flagged=1.*status=4xx/),
    );
    await userEvent.click(screen.getByRole('button', { name: 'Clear all' }));
    expect(screen.getByRole('button', { name: 'Errors only' })).toHaveAttribute('aria-pressed', 'false');
  });
});
