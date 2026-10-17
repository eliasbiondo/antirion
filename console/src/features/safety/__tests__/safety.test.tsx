import { render, screen, within } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { emptySlot } from '@/lib/slot';
import { signInAs } from '@/test/utils';

import { safetyApi } from '../api';
import { SafetyScreen } from '../components/safety-screen';
import { useSafety } from '../store';

function reset() {
  useSafety.setState({ events: emptySlot(), guardrails: emptySlot(), runtime: emptySlot() });
}

describe('safety events', () => {
  it('sends only the parameters the contract declares for the list', async () => {
    let query: URLSearchParams | null = null;
    server.use(
      http.get('*/admin/v1/guardrails/events', ({ request }) => {
        query = new URL(request.url).searchParams;
        return HttpResponse.json({ items: [], next_cursor: null });
      }),
    );
    await safetyApi.events('2026-10-14T00:00:00Z', '2026-10-14T06:00:00Z');
    expect([...((query as URLSearchParams | null)?.keys() ?? [])].sort()).toEqual(['from', 'limit', 'to']);
  });

  it('says "not available" on a gateway that answers the path as a guardrail and refuses the window', async () => {
    reset();
    signInAs(['logs:read', 'policies:read']);
    server.use(
      http.get('*/admin/v1/guardrails/events', ({ request }) => {
        const q = new URL(request.url).searchParams;
        if (q.has('from'))
          return HttpResponse.json(
            {
              error: {
                code: 'invalid_parameter',
                message: 'unknown query parameter "from"',
                request_id: 'r1',
              },
            },
            { status: 400 },
          );
        return HttpResponse.json(
          { error: { code: 'not_found', message: 'no guardrail events', request_id: 'r2' } },
          { status: 404 },
        );
      }),
    );
    render(<SafetyScreen />);
    expect(await screen.findByText(/isn't available on this gateway yet/)).toBeInTheDocument();
    expect(screen.queryByText(/unknown query parameter/)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Try again' })).not.toBeInTheDocument();
  });

  it('shows the events in plain words, with the review actions', async () => {
    reset();
    signInAs(['logs:read', 'policies:read', 'safety:review']);
    render(<SafetyScreen />);
    const list = await screen.findByRole('listbox', { name: 'Safety events' });
    expect(within(list).getAllByText(/confidence$/).length).toBeGreaterThan(0);
    expect(screen.getByRole('region', { name: 'Safety summary' })).toHaveTextContent('Values redacted');
    expect(screen.getByRole('button', { name: 'Mark reviewed' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'More' })).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/conf \d/);
  });
});
