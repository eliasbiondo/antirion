import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { useSession } from '@/stores/session';
import { useShell } from '@/stores/shell';
import { signInAs } from '@/test/utils';

import { TestModePanel } from '../components/test-mode-panel';

function admin() {
  const me = signInAs(['config:read']);
  useSession.setState({
    me: {
      ...me,
      bindings: [
        { id: 'rb', principal: { type: 'user', id: 'tester' }, role: 'gateway_admin', scope: 'gateway' },
      ],
    },
  });
}

describe('Test-mode tools', () => {
  beforeEach(() => useShell.setState({ status: { test_mode: true } as never }));

  it('is absent outside test mode and for principals who are not gateway admins', () => {
    useShell.setState({ status: { test_mode: false } as never });
    admin();
    const { unmount } = render(<TestModePanel />);
    expect(screen.queryByText('Test mode')).not.toBeInTheDocument();
    unmount();
    useShell.setState({ status: { test_mode: true } as never });
    signInAs(['config:read']);
    render(<TestModePanel />);
    expect(screen.queryByText('Test mode')).not.toBeInTheDocument();
  });

  it('advances the clock, resets the chosen state and sets the seed', async () => {
    const calls: { path: string; body: unknown }[] = [];
    server.use(
      http.get('*/admin/v1/test/clock', () => HttpResponse.json({ now: '2026-10-15T10:00:00Z' })),
      http.post('*/admin/v1/test/clock', async ({ request }) => {
        calls.push({ path: 'clock', body: await request.json() });
        return HttpResponse.json({ now: '2026-10-16T10:00:00Z' });
      }),
      http.post('*/admin/v1/test/reset', async ({ request }) => {
        calls.push({ path: 'reset', body: await request.json() });
        return new HttpResponse(null, { status: 204 });
      }),
      http.put('*/admin/v1/test/seed', async ({ request }) => {
        calls.push({ path: 'seed', body: await request.json() });
        return new HttpResponse(null, { status: 204 });
      }),
    );
    admin();
    render(<TestModePanel />);
    expect(await screen.findByText(/Oct 15, 2026/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('radio', { name: '1 day' }));
    await userEvent.click(screen.getByRole('button', { name: 'Advance' }));
    await waitFor(() => expect(calls[0]).toEqual({ path: 'clock', body: { advance: '1d' } }));
    expect(await screen.findByText(/Oct 16, 2026/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole('checkbox', { name: 'Request log' }));
    await userEvent.click(screen.getByRole('checkbox', { name: 'Audit log' }));
    await userEvent.click(screen.getByRole('button', { name: 'Reset selected' }));
    const dialog = await screen.findByRole('alertdialog', { name: 'Reset the selected state?' });
    expect(within(dialog).getByText(/configuration, keys and accounts are kept/)).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole('button', { name: 'Reset state' }));
    await waitFor(() =>
      expect(calls[1]).toEqual({
        path: 'reset',
        body: { scopes: ['counters', 'budgets', 'health', 'affinity', 'cache', 'sessions'] },
      }),
    );

    await userEvent.type(screen.getByLabelText('Seed'), '42');
    await userEvent.click(screen.getByRole('button', { name: 'Set seed' }));
    await waitFor(() => expect(calls[2]).toEqual({ path: 'seed', body: { seed: 42 } }));
  });
});
