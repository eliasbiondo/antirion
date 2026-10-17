import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import { RequestLogScreen } from '../components/request-log-screen';
import { useLogs } from '../store';
import { EMPTY_LOG_FILTERS } from '../types';

const empty = () => HttpResponse.json({ items: [], next_cursor: null });

function record(id: string, status: number) {
  return {
    id,
    request_id: id,
    status_code: status,
    code: status >= 400 ? 'upstream_unavailable' : null,
    started_at: new Date().toISOString(),
    timing: { total_ms: 420 },
    cost: { total: '0.0021' },
    cache: { status: 'miss' },
    usage: { input: 120, output: 40 },
    guardrails: [],
    stream: null,
    model: 'gpt-4o',
    provider: 'openai',
    key: 'web',
    team: 'platform',
    end_user: 'ada',
    tags: [],
  };
}

describe('Request log screen', () => {
  beforeEach(() => {
    window.history.replaceState(null, '', '/request-log/');
    useLogs.setState({
      records: emptySlot(),
      aggregates: emptySlot(),
      filters: EMPTY_LOG_FILTERS,
      facets: {},
    });
    signInAs(['logs:read', 'analytics:read', 'providers:read', 'models:read', 'keys:read']);
  });

  it('teaches the first request on a gateway that has served none', async () => {
    server.use(
      http.get('*/admin/v1/logs', empty),
      http.get('*/admin/v1/analytics/usage', empty),
      http.get('*/admin/v1/providers', empty),
      http.get('*/admin/v1/models', empty),
      http.get('*/admin/v1/keys', empty),
    );
    render(<RequestLogScreen />);
    expect(await screen.findByText('No requests yet')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Copy a sample request' })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Create an API key' })).toBeInTheDocument();
    expect(screen.queryByRole('region', { name: 'Requests in the range' })).not.toBeInTheDocument();
  });

  it('shows the range figures, a starred view as removable chips, and clears them', async () => {
    server.use(
      http.get('*/admin/v1/logs', () =>
        HttpResponse.json({ items: [record('a', 200), record('b', 503)], next_cursor: null }),
      ),
    );
    render(<RequestLogScreen />);
    expect(await screen.findByRole('region', { name: 'Requests in the range' })).toHaveTextContent(
      'Error rate',
    );
    await userEvent.click(screen.getByRole('button', { name: /^Starred Errors$|Errors/ }));
    const chip = await screen.findByRole('button', { name: 'Remove filter Status: 4xx, 5xx' });
    expect(chip).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Clear all' }));
    expect(screen.queryByRole('button', { name: /Remove filter/ })).not.toBeInTheDocument();
  });

  it('labels live updates and pauses them', async () => {
    server.use(http.get('*/admin/v1/logs', empty));
    render(<RequestLogScreen />);
    const live = screen.getByRole('button', { name: 'Pause live updates' });
    expect(live).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(live);
    expect(screen.getByRole('button', { name: 'Resume live updates' })).toHaveAttribute(
      'aria-pressed',
      'false',
    );
    useLogs.setState({ live: true });
  });
});
