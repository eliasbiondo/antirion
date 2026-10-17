import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { useUi } from '@/stores/ui';
import { signInAs } from '@/test/utils';

import { EraseEndUserButton, RequestLogMoreMenu } from '../components/erase-end-user';

function serve(deleted: number) {
  const calls: { endUser: string | null; reason: string | null }[] = [];
  server.use(
    http.delete('*/admin/v1/logs', ({ request }) => {
      calls.push({
        endUser: new URL(request.url).searchParams.get('end_user'),
        reason: request.headers.get('x-antirion-change-reason'),
      });
      return HttpResponse.json({ deleted });
    }),
  );
  return calls;
}

describe('End-user erasure', () => {
  it('erases from the drawer after the id is typed again, and says how many records went', async () => {
    const calls = serve(12);
    signInAs(['logs:read', 'logs:delete']);
    let erased = false;
    render(<EraseEndUserButton endUser="cust-4821" onErased={() => (erased = true)} />);
    await userEvent.click(screen.getByRole('button', { name: 'Erase records' }));
    const dialog = await screen.findByRole('alertdialog', { name: 'Erase the records of cust-4821?' });
    expect(within(dialog).getByText(/Cost ledger lines and usage figures stay/)).toBeInTheDocument();
    const confirm = within(dialog).getByRole('button', { name: 'Erase records' });
    expect(confirm).toBeDisabled();
    await userEvent.type(within(dialog).getByRole('textbox'), 'cust-4821');
    await userEvent.click(confirm);
    await waitFor(() => expect(calls).toEqual([{ endUser: 'cust-4821', reason: null }]));
    await waitFor(() =>
      expect(useUi.getState().toasts.some((t) => t.title === '12 records erased')).toBe(true),
    );
    expect(erased).toBe(true);
  });

  it('takes the id from the header menu and reports when nothing matched', async () => {
    const calls = serve(0);
    signInAs(['logs:read', 'logs:delete']);
    render(<RequestLogMoreMenu />);
    await userEvent.click(screen.getByRole('button', { name: 'More actions' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: "Erase an end user's records" }));
    const dialog = await screen.findByRole('alertdialog', { name: "Erase an end user's records?" });
    await userEvent.type(within(dialog).getByLabelText(/End user ID/), 'cust-0');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Erase records' }));
    await waitFor(() => expect(calls[0]?.endUser).toBe('cust-0'));
    await waitFor(() =>
      expect(useUi.getState().toasts.some((t) => t.title === 'Nothing to erase')).toBe(true),
    );
  });

  it('is offered only with logs:delete', () => {
    signInAs(['logs:read']);
    render(
      <>
        <EraseEndUserButton endUser="cust-1" />
        <RequestLogMoreMenu />
      </>,
    );
    expect(screen.queryByRole('button', { name: 'Erase records' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'More actions' })).not.toBeInTheDocument();
  });
});
