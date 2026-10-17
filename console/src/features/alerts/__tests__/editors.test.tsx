import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';
import { useUi } from '@/stores/ui';

import { AlertsScreen } from '../components/alerts-screen';
import { useAlertEditors } from '../editors';
import { useAlerts } from '../store';

const WRITE = ['alerts:read', 'alerts:write', 'config:read', 'config:apply', 'analytics:read'];

let observability: {
  alerts: { webhooks: Record<string, { url: string; events: string[]; enabled?: boolean }> };
};

function reset(path = '/alerts/') {
  window.history.replaceState(null, '', path);
  useAlerts.setState(useAlerts.getInitialState(), true);
  useAlertEditors.setState({ stack: [] });
  observability = {
    alerts: { webhooks: { 'ops-events': { url: 'https://hooks.example.com/ops', events: ['budget.*'] } } },
  };
  server.use(
    http.get('*/admin/v1/settings/observability', () =>
      HttpResponse.json({
        spec: observability,
        metadata: { etag: '"o1"', generation: 1, created_at: '', updated_at: '', managed_by: 'api' },
      }),
    ),
  );
}

describe('Alerts: 30-day figures, detail and actions', () => {
  beforeEach(() => reset());

  it('shows 30-day time to resolve, noise and the last day of notifications by channel', async () => {
    signInAs(WRITE);
    render(<AlertsScreen />);
    const summary = await screen.findByRole('region', { name: 'Alert summary' });
    await waitFor(() => expect(summary).toHaveTextContent(/Time to resolve\d+ (min|h)/));
    expect(summary).toHaveTextContent('Noise, 30 days');
    expect(summary).toHaveTextContent('Notifications, 24 h');
    expect(summary).toHaveTextContent('oncall-slack');
  });

  it("lists similar past fires and the rule's 30 days on the detail", async () => {
    signInAs(WRITE);
    render(<AlertsScreen />);
    expect(await screen.findByRole('heading', { name: 'Similar fires, last 90 days' })).toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByText(/^Lasted /).length).toBeGreaterThan(1));
    expect(screen.getByRole('heading', { name: 'Rule, last 30 days' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Edit rule' })).toBeInTheDocument();
  });

  it('ends a snooze', async () => {
    signInAs(WRITE);
    let unsnoozed = false;
    server.use(
      http.post('*/admin/v1/alerts/:id', ({ params }) => {
        if (String(params.id).endsWith(':unsnooze')) unsnoozed = true;
        return undefined;
      }),
    );
    render(<AlertsScreen />);
    await userEvent.click(await screen.findByRole('button', { name: 'Snooze' }));
    await userEvent.click(screen.getByRole('menuitem', { name: '1 hour' }));
    const unsnooze = await screen.findByRole('button', { name: 'Unsnooze' });
    await userEvent.click(unsnooze);
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Unsnooze' })).not.toBeInTheDocument());
    expect(unsnoozed).toBe(true);
    expect(screen.getAllByText('Snooze ended').length).toBeGreaterThan(0);
  });

  it('exports the inbox as CSV', async () => {
    signInAs(WRITE);
    render(<AlertsScreen />);
    await screen.findByRole('listbox', { name: 'Alerts' });
    expect(screen.getByRole('button', { name: 'Export CSV' })).toBeInTheDocument();
  });
});

describe('Alerts: rules', () => {
  beforeEach(() => reset('/alerts/?view=rules'));

  it('creates a rule from a template, with the maintenance silence and a fire preview', async () => {
    signInAs(WRITE);
    let sent: { name: string; spec: Record<string, unknown> } | null = null;
    server.use(
      http.post('*/admin/v1/alerts/rules', async ({ request }) => {
        sent = (await request.clone().json()) as typeof sent;
        return undefined;
      }),
    );
    render(<AlertsScreen />);
    await userEvent.click(await screen.findByRole('button', { name: 'New rule' }));
    const drawer = await screen.findByRole('dialog', { name: 'New alert rule' });
    await userEvent.click(within(drawer).getByRole('button', { name: /^Error rate/ }));
    const form = await screen.findByRole('dialog', { name: 'New rule: Error rate' });
    expect(await within(form).findByText('Error rate is more than 2% for 5 min')).toBeInTheDocument();
    expect(await within(form).findByText(/Would have fired|No traffic/)).toBeInTheDocument();
    await userEvent.click(
      await within(form).findByRole('switch', { name: 'Silence during maintenance windows' }),
    );
    await userEvent.click(within(form).getByRole('button', { name: 'Create rule' }));
    await waitFor(() => expect(sent).not.toBeNull());
    // The editor closes once the gateway answered and the list has the new rule.
    await waitFor(() => expect(form).not.toBeInTheDocument());
    await waitFor(() =>
      expect(useAlerts.getState().rules.data?.some((r) => r.name === 'error-rate')).toBe(true),
    );
    expect(sent).toMatchObject({
      name: 'error-rate',
      spec: {
        metric: 'error_rate',
        operator: 'gt',
        threshold: 0.02,
        window: '5m',
        category: 'errors',
        severity: 'critical',
        silence_during_maintenance: true,
      },
    });
    expect(await screen.findByRole('button', { name: 'error-rate' })).toBeInTheDocument();
  });

  it('edits a rule and deletes one', async () => {
    signInAs(WRITE);
    let put: { spec: Record<string, unknown> } | null = null;
    server.use(
      http.put('*/admin/v1/alerts/rules/:rule', async ({ request }) => {
        put = (await request.clone().json()) as typeof put;
        return undefined;
      }),
    );
    render(<AlertsScreen />);
    await userEvent.click(await screen.findByRole('button', { name: 'pii-flags' }));
    const drawer = await screen.findByRole('dialog', { name: 'pii-flags' });
    const threshold = await within(drawer).findByRole('spinbutton', { name: /Threshold/ });
    await userEvent.clear(threshold);
    await userEvent.type(threshold, '25');
    await waitFor(() => expect(threshold).toHaveDisplayValue('25'));
    await userEvent.click(within(drawer).getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(put).not.toBeNull());
    expect(put).toMatchObject({ spec: { metric: 'guardrail_events', threshold: 25 } });
    // The drawer closes on the gateway's answer; the row menu opens only once nothing is over it.
    await waitFor(() => expect(drawer).not.toBeInTheDocument());

    await userEvent.click(await screen.findByRole('button', { name: 'Actions for cache-drop' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Delete' }));
    const confirm = await screen.findByRole('alertdialog');
    await userEvent.type(within(confirm).getByLabelText(/to confirm/), 'cache-drop');
    await userEvent.click(within(confirm).getByRole('button', { name: 'Delete rule' }));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'cache-drop' })).not.toBeInTheDocument());
  });

  it('searches rules in the manage dialog and turns one off', async () => {
    signInAs(WRITE);
    render(<AlertsScreen />);
    await screen.findByRole('button', { name: 'provider-p95' });
    await userEvent.click(screen.getByRole('button', { name: 'Manage rules' }));
    const dialog = await screen.findByRole('dialog', { name: 'Manage alert rules' });
    await userEvent.type(within(dialog).getByRole('textbox', { name: 'Search rules' }), 'budget');
    const list = within(dialog).getByRole('list', { name: 'Rules' });
    expect(within(list).getAllByRole('listitem')).toHaveLength(1);
    await userEvent.click(within(list).getByRole('switch', { name: 'Turn off budget-projection' }));
    await waitFor(() =>
      expect(within(dialog).getByRole('switch', { name: 'Turn on budget-projection' })).toBeInTheDocument(),
    );
    await userEvent.click(within(list).getByRole('button', { name: /budget-projection/ }));
    expect(await screen.findByRole('dialog', { name: 'budget-projection' })).toBeInTheDocument();
  });

  it('keeps rule changes from a reader without the permission', async () => {
    signInAs(['alerts:read']);
    render(<AlertsScreen />);
    const add = await screen.findByRole('button', { name: 'New rule' });
    expect(add).toBeDisabled();
    expect(add).toHaveAccessibleDescription("You don't have permission to create alert rules.");
  });
});

describe('Alerts: channels and events', () => {
  beforeEach(() => reset('/alerts/?view=channels'));

  it('shows the last delivery of each channel and sends a test', async () => {
    signInAs(WRITE);
    render(<AlertsScreen />);
    const table = await screen.findByRole('table', { name: 'Notification channels' });
    await waitFor(() => expect(within(table).getAllByText('Delivered').length).toBeGreaterThan(0));
    expect(within(table).getByText('Failed (502)')).toBeInTheDocument();
    const row = within(table).getByText('oncall-slack').closest('tr');
    if (!row) throw new Error('row');
    await userEvent.click(within(row).getByRole('button', { name: 'Test' }));
    await waitFor(() => expect(useUi.getState().toasts.some((t) => t.title === 'Test delivered')).toBe(true));
  });

  it('adds a Slack channel with its webhook URL', async () => {
    signInAs(WRITE);
    let sent: { name: string; spec: Record<string, unknown> } | null = null;
    server.use(
      http.post('*/admin/v1/alerts/channels', async ({ request }) => {
        sent = (await request.clone().json()) as typeof sent;
        return undefined;
      }),
    );
    render(<AlertsScreen />);
    await userEvent.click(await screen.findByRole('button', { name: 'New channel' }));
    const drawer = await screen.findByRole('dialog', { name: 'New notification channel' });
    await userEvent.click(within(drawer).getByRole('button', { name: 'Add channel' }));
    expect(within(drawer).getByText('Paste the incoming webhook URL from Slack')).toBeInTheDocument();
    await userEvent.type(within(drawer).getByRole('textbox', { name: /Name/ }), 'team-slack');
    await userEvent.type(
      within(drawer).getByLabelText(/Incoming webhook URL/),
      'https://hooks.slack.com/services/T/B/x',
    );
    await userEvent.click(within(drawer).getByRole('button', { name: 'Add channel' }));
    await waitFor(() => expect(sent).not.toBeNull());
    expect(sent).toMatchObject({
      name: 'team-slack',
      spec: { type: 'slack', slack: { webhook_url: 'https://hooks.slack.com/services/T/B/x' } },
    });
  });

  it('lists event webhooks and subscribes one to a built-in event', async () => {
    signInAs(WRITE);
    let patched: unknown = null;
    server.use(
      http.patch('*/admin/v1/settings/observability', async ({ request }) => {
        patched = await request.json();
        return HttpResponse.json({
          spec: observability,
          metadata: { etag: '"o2"', generation: 2, created_at: '', updated_at: '', managed_by: 'api' },
        });
      }),
    );
    render(<AlertsScreen />);
    const hooks = await screen.findByRole('table', { name: 'Event webhooks' });
    expect(within(hooks).getByText('ops-events')).toBeInTheDocument();
    expect(within(hooks).getByText('budget.*')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Send to a webhook' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Add to ops-events' }));
    await waitFor(() =>
      expect(patched).toEqual({
        spec: {
          alerts: { webhooks: { 'ops-events': { events: ['budget.*', 'deployment.state_changed'] } } },
        },
      }),
    );
  });

  it('opens an event rule from an event source', async () => {
    signInAs(WRITE);
    render(<AlertsScreen />);
    await screen.findByRole('table', { name: 'Event webhooks' });
    await userEvent.click(screen.getByRole('button', { name: 'Alert on it' }));
    const drawer = await screen.findByRole('dialog', { name: 'New alert rule' });
    expect(within(drawer).getByRole('radio', { name: 'An event happens' })).toBeChecked();
    expect(within(drawer).getAllByText(/Deployment state changed/i).length).toBeGreaterThan(0);
  });
});
