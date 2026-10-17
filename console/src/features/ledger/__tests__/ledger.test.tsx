import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import type { LedgerLine } from '../api';
import { LedgerTab } from '../components/ledger-tab';
import { RequestLedger } from '../components/request-ledger';

function line(over: Partial<LedgerLine>): LedgerLine {
  return {
    request_id: '01a1-log',
    line: 0,
    purpose: 'primary',
    shadow: false,
    status: 'success',
    settled_at: '2026-10-15T10:00:00Z',
    team: 'platform',
    key: 'web-app',
    model: 'gpt-4o',
    usage: { input: 1200, output: 300 },
    usage_source: 'upstream',
    billed_usd: '0.0060',
    cost_usd: '0.0060',
    budgets: [{ level: 'team', owner: 'platform', name: 'monthly' }],
    ...over,
  };
}

describe('Request ledger', () => {
  it('lists the lines of the record by its log id, with the budgets charged and the total', async () => {
    const asked: string[] = [];
    server.use(
      http.get('*/admin/v1/reports/spend/requests/:id', ({ params }) => {
        asked.push(String(params.id));
        return HttpResponse.json({
          items: [
            line({}),
            line({
              line: 1,
              purpose: 'retry',
              status: 'failed_with_usage',
              billed_usd: '0.0010',
              budgets: [],
            }),
          ],
          next_cursor: null,
        });
      }),
    );
    signInAs(['analytics:read']);
    render(<RequestLedger id="01a1-log" />);
    const table = await screen.findByRole('table', { name: 'Cost ledger lines' });
    expect(asked).toEqual(['01a1-log']);
    expect(within(table).getByText(/charged to team\/platform\/monthly/)).toBeInTheDocument();
    expect(within(table).getByText('Failed, charged its usage')).toBeInTheDocument();
    expect(screen.getByText('$0.007')).toBeInTheDocument();
  });

  it('says a request may still be settling when it has no line, and hides from non-readers', async () => {
    server.use(
      http.get('*/admin/v1/reports/spend/requests/:id', () =>
        HttpResponse.json({ error: { code: 'not_found', message: 'x' } }, { status: 404 }),
      ),
    );
    signInAs(['analytics:read']);
    const { unmount } = render(<RequestLedger id="nope" />);
    expect(await screen.findByText(/It may still be settling/)).toBeInTheDocument();
    unmount();
    signInAs(['logs:read']);
    render(<RequestLedger id="nope" />);
    expect(screen.queryByText('Cost ledger')).not.toBeInTheDocument();
  });
});

describe('Ledger tab', () => {
  beforeEach(() => window.history.replaceState(null, '', '/budgets/?view=ledger'));

  it('reads the window with the filters of the URL, pages with Show more and links each line to its request', async () => {
    const queries: URLSearchParams[] = [];
    server.use(
      http.get('*/admin/v1/reports/spend/ledger', ({ request }) => {
        const q = new URL(request.url).searchParams;
        queries.push(q);
        return q.get('cursor')
          ? HttpResponse.json({ items: [line({ request_id: 'second-page' })], next_cursor: null })
          : HttpResponse.json({ items: [line({})], next_cursor: 'c2' });
      }),
    );
    signInAs(['analytics:read']);
    window.history.replaceState(null, '', '/budgets/?view=ledger&team=platform&status=success');
    render(<LedgerTab />);
    const table = await screen.findByRole('table', { name: 'Ledger lines' });
    expect(queries[0]?.get('team')).toBe('platform');
    expect(queries[0]?.get('status')).toBe('success');
    expect(queries[0]?.get('from')).toBeTruthy();
    expect(within(table).getByRole('link', { name: '01a1-log' })).toHaveAttribute(
      'href',
      expect.stringContaining('request=01a1-log') as string,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Show more' }));
    await waitFor(() => expect(within(table).getByRole('link', { name: 'second-page' })).toBeInTheDocument());
    expect(queries[1]?.get('cursor')).toBe('c2');
    expect(screen.getByText(/2 lines/)).toBeInTheDocument();
  });

  it('applies a typed filter on Enter', async () => {
    const queries: URLSearchParams[] = [];
    server.use(
      http.get('*/admin/v1/reports/spend/ledger', ({ request }) => {
        queries.push(new URL(request.url).searchParams);
        return HttpResponse.json({ items: [], next_cursor: null });
      }),
    );
    signInAs(['analytics:read']);
    render(<LedgerTab />);
    expect(await screen.findByText('No charges in this time range')).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText('End user'), 'cust-1{Enter}');
    await waitFor(() => expect(queries.at(-1)?.get('end_user')).toBe('cust-1'));
    expect(window.location.search).toContain('end_user=cust-1');
  });
});
