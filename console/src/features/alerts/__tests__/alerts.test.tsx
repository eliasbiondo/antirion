import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import { AlertsScreen } from '../components/alerts-screen';
import { describeCondition, severityFloor } from '../describe';
import { useAlerts } from '../store';

function reset() {
  window.history.replaceState(null, '', '/alerts/');
  useAlerts.setState(useAlerts.getInitialState(), true);
}

describe('Alerts screen (contract mocks)', () => {
  beforeEach(reset);

  it('shows the firing alerts and the first one in detail', async () => {
    signInAs(['alerts:read', 'alerts:write']);
    render(<AlertsScreen />);
    const list = await screen.findByRole('listbox', { name: 'Alerts' });
    expect(within(list).getAllByRole('option').length).toBeGreaterThan(1);
    expect(screen.getByRole('heading', { level: 2, name: /gpt-4o p95 latency/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Acknowledge' })).toBeEnabled();
    expect(screen.getByRole('region', { name: 'Alert summary' })).toHaveTextContent('Firing now');
  });

  it('keeps the actions a principal may not take, disabled with the reason', async () => {
    signInAs(['alerts:read']);
    render(<AlertsScreen />);
    await screen.findByRole('listbox', { name: 'Alerts' });
    const ack = screen.getByRole('button', { name: 'Acknowledge' });
    expect(ack).toBeDisabled();
    expect(ack).toHaveAccessibleDescription(
      expect.stringContaining("You don't have permission to acknowledge"),
    );
    expect(screen.getByRole('button', { name: 'Resolve' })).toBeDisabled();
  });

  it('filters by status', async () => {
    signInAs([]);
    render(<AlertsScreen />);
    await screen.findByRole('listbox', { name: 'Alerts' });
    await userEvent.click(screen.getByRole('radio', { name: /Resolved/ }));
    expect(within(screen.getByRole('listbox', { name: 'Alerts' })).getAllByRole('option')).toHaveLength(1);
  });

  it('keeps the inbox filters in the URL, so a copied link reopens them', async () => {
    signInAs([]);
    render(<AlertsScreen />);
    await screen.findByRole('listbox', { name: 'Alerts' });
    await userEvent.click(screen.getByRole('radio', { name: /Resolved/ }));
    expect(new URLSearchParams(window.location.search).get('status')).toBe('resolved');
    await userEvent.click(screen.getByRole('button', { name: /Critical/ }));
    expect(new URLSearchParams(window.location.search).getAll('severity')).toEqual(['critical']);
  });

  it('opens with the filters a link carries', async () => {
    window.history.replaceState(null, '', '/alerts/?status=resolved');
    signInAs([]);
    render(<AlertsScreen />);
    const list = await screen.findByRole('listbox', { name: 'Alerts' });
    expect(within(list).getAllByRole('option')).toHaveLength(1);
    expect(screen.getByRole('radio', { name: /Resolved/ })).toBeChecked();
  });

  it('lists rules with their condition in words', async () => {
    window.history.replaceState(null, '', '/alerts/?view=rules');
    signInAs(['alerts:read']);
    render(<AlertsScreen />);
    expect(await screen.findByText('Latency is more than 3× its baseline for 5 min')).toBeInTheDocument();
    expect(screen.queryByText(/latency_baseline_ratio gt/)).not.toBeInTheDocument();
  });

  it('says once that alerting is not available, without zeros', async () => {
    const missing = () =>
      HttpResponse.json({ error: { code: 'not_found', message: 'no route' } }, { status: 404 });
    server.use(
      http.get('*/admin/v1/alerts', missing),
      http.get('*/admin/v1/alerts/rules', missing),
      http.get('*/admin/v1/alerts/channels', missing),
      http.get('*/admin/v1/runtime/slos', missing),
    );
    signInAs(['alerts:read']);
    render(<AlertsScreen />);
    expect(await screen.findByText("Alerting isn't available on this gateway yet")).toBeInTheDocument();
    expect(screen.queryByText('Firing now')).not.toBeInTheDocument();
    expect(screen.queryByRole('tablist')).not.toBeInTheDocument();
  });

  it('teaches the empty inbox', async () => {
    const empty = () => HttpResponse.json({ items: [], next_cursor: null });
    server.use(http.get('*/admin/v1/alerts', empty));
    signInAs(['alerts:read']);
    render(<AlertsScreen />);
    expect(await screen.findByText('No alerts')).toBeInTheDocument();
    expect(await screen.findByRole('button', { name: /^(Create rule|View rules)$/ })).toBeInTheDocument();
    expect(screen.queryByText('Firing now')).not.toBeInTheDocument();
  });
});

describe('alert wording', () => {
  it('describes metric and event conditions', () => {
    expect(
      describeCondition({
        category: 'errors',
        metric: 'error_rate',
        operator: 'gte',
        threshold: 0.02,
        window: '10m',
      }),
    ).toBe('Error rate is at least 2% for 10 min');
    expect(describeCondition({ category: 'security', events: ['key.leaked'] })).toBe(
      'When key leaked happens',
    );
    expect(severityFloor('warning')).toBe('Warning and above');
    expect(severityFloor(undefined)).toBe('All alerts');
  });
});
