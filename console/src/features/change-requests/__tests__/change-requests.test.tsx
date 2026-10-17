import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ChangeRequest } from '@/lib/api/client';
import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { useSession } from '@/stores/session';
import { signInAs } from '@/test/utils';

import { ChangeRequestsScreen } from '../components/change-requests-screen';
import { OpenChangeRequestDialog } from '../components/open-change-request-dialog';
import { useChangeRequests } from '../store';

function request(over: Partial<ChangeRequest>): ChangeRequest {
  return {
    id: 'cr_1',
    kind: 'config',
    title: 'Change route default',
    description: 'Send support traffic to the cheaper deployment.',
    author: 'alice',
    created_at: '2026-10-14T10:00:00Z',
    base_generation: 7,
    changes: [{ method: 'PATCH', path: '/routes/default', body: { spec: { type: 'conditional' } } }],
    diff: [{ op: 'update', path: 'routes.default.type', before: null, after: null }],
    state: 'open',
    approvals: [],
    approvals_required: 1,
    expires_at: '2026-10-15T10:00:00Z',
    applied_generation: null,
    ...over,
  };
}

function serve(items: ChangeRequest[]) {
  const store = [...items];
  const actions: string[] = [];
  server.use(
    http.get('*/admin/v1/change_requests', () => HttpResponse.json({ items: store, next_cursor: null })),
    http.post('*/admin/v1/change_requests/:segment', ({ params }) => {
      const [id, action] = String(params.segment).split(':');
      actions.push(`${id}:${action}`);
      const i = store.findIndex((r) => r.id === id);
      const current = store[i];
      if (!current) return HttpResponse.json({}, { status: 404 });
      const next: ChangeRequest =
        action === 'approve'
          ? { ...current, state: 'approved', approvals: [{ by: 'tester', at: '2026-10-14T11:00:00Z' }] }
          : action === 'apply'
            ? { ...current, state: 'applied', applied_generation: 8 }
            : { ...current, state: action === 'reject' ? 'rejected' : 'cancelled' };
      store[i] = next;
      return HttpResponse.json(next);
    }),
  );
  return actions;
}

describe('Change requests screen', () => {
  beforeEach(() => {
    window.history.replaceState(null, '', '/change-requests/');
    useChangeRequests.setState({ list: emptySlot() });
  });

  it('lists pending requests and lets a reviewer approve, then apply', async () => {
    const actions = serve([
      request({}),
      request({ id: 'cr_2', title: 'Delete route legacy', state: 'applied', applied_generation: 5 }),
    ]);
    signInAs(['config:read']);
    render(<ChangeRequestsScreen />);
    const list = await screen.findByRole('listbox', { name: 'Change requests' });
    expect(within(list).getAllByRole('option')).toHaveLength(1);
    expect(screen.getByRole('tab', { name: /Pending/ })).toHaveTextContent('1');
    expect(screen.getByRole('heading', { level: 2, name: 'Change route default' })).toBeInTheDocument();
    expect(screen.getByText('PATCH /routes/default')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Approve' }));
    expect(actions).toEqual(['cr_1:approve']);
    await userEvent.click(await screen.findByRole('button', { name: 'Apply change' }));
    const dialog = await screen.findByRole('alertdialog', { name: /Apply Change route default/ });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Apply change' }));
    expect(actions).toEqual(['cr_1:approve', 'cr_1:apply']);
  });

  it('keeps review away from the author, who may cancel', async () => {
    serve([request({ author: 'tester' })]);
    signInAs(['config:read']);
    render(<ChangeRequestsScreen />);
    await screen.findByRole('listbox', { name: 'Change requests' });
    expect(screen.getByRole('button', { name: 'Approve' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Reject' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Cancel request' })).toBeEnabled();
  });

  it('treats every credential of the author as the author, and says secrets are hidden', async () => {
    serve([
      request({
        author: 'ada',
        changes: [{ method: 'PATCH', path: '/settings/smtp', body: { spec: { password: '[REDACTED]' } } }],
      }),
    ]);
    const me = signInAs(['config:read']);
    useSession.setState({
      me: {
        ...me,
        principal: { type: 'token', id: 'kid-2', authenticated_by: 'bearer' },
        user: { name: 'ada' },
      },
    });
    render(<ChangeRequestsScreen />);
    await screen.findByRole('listbox', { name: 'Change requests' });
    expect(screen.getByRole('button', { name: 'Approve' })).toBeDisabled();
    expect(screen.getByText(/Secret values are hidden/)).toBeInTheDocument();
  });

  it('shows closed requests on their tab, filtered in the URL', async () => {
    serve([request({}), request({ id: 'cr_2', title: 'Delete route legacy', state: 'rejected' })]);
    signInAs(['config:read']);
    render(<ChangeRequestsScreen />);
    await screen.findByRole('listbox', { name: 'Change requests' });
    await userEvent.click(screen.getByRole('tab', { name: /Closed/ }));
    expect(window.location.search).toContain('view=closed');
  });

  it('says so to a principal that may not read them', () => {
    signInAs([]);
    render(<ChangeRequestsScreen />);
    expect(screen.getByText(/You don't have access to change requests/)).toBeInTheDocument();
  });
});

describe('Opening a change request for a refused write', () => {
  it('sends the same call as the change, with the title the reviewer will read', async () => {
    let sent: unknown = null;
    server.use(
      http.post('*/admin/v1/change_requests', async ({ request: r }) => {
        sent = await r.json();
        return HttpResponse.json(request({ id: 'cr_9' }), { status: 201 });
      }),
    );
    signInAs(['config:read']);
    const opened = vi.fn();
    render(
      <OpenChangeRequestDialog
        change={{ method: 'PATCH', path: '/routes/default', body: { spec: { type: 'conditional' } } }}
        onClose={() => undefined}
        onOpened={opened}
      />,
    );
    const dialog = screen.getByRole('dialog', { name: 'This change needs a reviewer' });
    expect(within(dialog).getByLabelText('Title')).toHaveValue('Change route default');
    await userEvent.type(within(dialog).getByLabelText(/What and why/), 'Cheaper at night');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Open change request' }));
    expect(sent).toEqual({
      title: 'Change route default',
      description: 'Cheaper at night',
      changes: [{ method: 'PATCH', path: '/routes/default', body: { spec: { type: 'conditional' } } }],
    });
    expect(opened).toHaveBeenCalledWith('cr_9');
  });
});
