import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { usePreferences } from '@/stores/preferences';
import { signInAs } from '@/test/utils';

import { LogDrawer } from '../components/log-drawer';
import { RequestLogScreen } from '../components/request-log-screen';
import { useLogs } from '../store';
import { EMPTY_LOG_FILTERS } from '../types';

function record(id: string) {
  return {
    id,
    request_id: id,
    status_code: 200,
    code: null,
    started_at: new Date().toISOString(),
    timing: { total_ms: 420, ttft_ms: 120 },
    cost: { total: '0.0021' },
    cache: { status: 'semantic_hit', similarity: 0.973, layer: 'semantic' },
    usage: { input: 120, output: 40 },
    guardrails: [],
    stream: null,
    model: 'gpt-4o',
    provider: 'openai',
    key: 'web',
    team: 'platform',
    project: 'core',
    end_user: 'ada',
    tags: ['env:prod'],
    method: 'POST',
    endpoint: '/v1/chat/completions',
    client_ip: '10.0.0.1',
    client_geo: { city: 'Lisbon', region: null, country: 'PT' },
    attempts: [
      {
        index: 0,
        deployment: 'gpt-4o-a',
        reason: 'primary',
        outcome: 'upstream_error',
        status_code: 503,
        duration_ms: 90,
      },
      {
        index: 1,
        deployment: 'gpt-4o-b',
        reason: 'fallback',
        outcome: 'success',
        status_code: 200,
        duration_ms: 300,
      },
    ],
    attempt_count: 2,
    request_headers: { 'content-type': 'application/json', 'user-agent': 'sdk' },
    response_headers: {},
    query: { 'api-version': '2024-10-01' },
    trace_id: 'abc123',
    span_id: 'def',
    content: {
      request: {
        body: {
          model: 'gpt-4o',
          messages: [
            { role: 'system', content: 'Be brief.' },
            { role: 'user', content: 'Hi' },
          ],
        },
        truncated: false,
        original_bytes: 96,
      },
      response: {
        body: { choices: [{ message: { role: 'assistant', content: 'Hello!' }, finish_reason: 'stop' }] },
        truncated: false,
        original_bytes: 70,
      },
    },
  };
}

const view = (id: string, extra: Record<string, unknown>) => ({
  id,
  name: id,
  filters: {},
  starred: false,
  shared: false,
  created_by: 'tester',
  columns: [],
  ...extra,
});

describe('saved views and columns', () => {
  let views: ReturnType<typeof view>[];
  let patched: unknown[];

  beforeEach(() => {
    window.history.replaceState(null, '', '/request-log/');
    useLogs.setState({
      records: emptySlot(),
      aggregates: emptySlot(),
      views: emptySlot(),
      filters: EMPTY_LOG_FILTERS,
      sort: 'started_at',
      order: 'desc',
      facets: {},
    });
    usePreferences.setState({ logColumns: null });
    signInAs(['logs:read', 'analytics:read', 'providers:read', 'models:read', 'keys:read']);
    views = [
      view('mine', {
        filters: { status_class: ['5xx'] },
        sort: 'latency_ms:desc',
        columns: ['time', 'status', 'ttft'],
      }),
      view('theirs', { shared: true, created_by: 'ada' }),
    ];
    patched = [];
    server.use(
      http.get('*/admin/v1/logs', () => HttpResponse.json({ items: [record('a')], next_cursor: null })),
      http.get('*/admin/v1/logs/views', () => HttpResponse.json({ items: views, next_cursor: null })),
      http.post('*/admin/v1/logs/views', async ({ request }) => {
        const body = (await request.json()) as Record<string, unknown>;
        const created = view('new', { ...body, id: 'new' });
        views = [...views, created];
        return HttpResponse.json(created, { status: 201 });
      }),
      http.patch('*/admin/v1/logs/views/:id', async ({ request, params }) => {
        const body = (await request.json()) as Record<string, unknown>;
        patched.push({ id: params.id, ...body });
        const found = views.find((v) => v.id === params.id);
        return HttpResponse.json({ ...found, ...body });
      }),
      http.delete('*/admin/v1/logs/views/:id', ({ params }) => {
        views = views.filter((v) => v.id !== params.id);
        return new HttpResponse(null, { status: 204 });
      }),
    );
  });

  it('applies a view with its filters, sort and columns, and says who shared one', async () => {
    render(<RequestLogScreen />);
    const nav = await screen.findByRole('navigation', { name: 'Saved views' });
    expect(await within(nav).findByText('Shared by ada')).toBeInTheDocument();
    await userEvent.click(within(nav).getByRole('button', { name: /^mine/ }));
    expect(await screen.findByRole('button', { name: 'Remove filter Status: 5xx' })).toBeInTheDocument();
    expect(useLogs.getState().sort).toBe('latency_ms');
    expect(usePreferences.getState().logColumns).toEqual(['time', 'status', 'ttft']);
    expect(await screen.findByRole('columnheader', { name: /TTFT/ })).toBeInTheDocument();
    expect(window.location.search).toContain('status=5xx');
    expect(window.location.search).toContain('sort=latency_ms%3Adesc');
  });

  it('stars a shared view for the reader only, and offers no edits on it', async () => {
    signInAs(['logs:read', 'analytics:read'], 'organization/default');
    render(<RequestLogScreen />);
    const nav = await screen.findByRole('navigation', { name: 'Saved views' });
    await userEvent.click(await within(nav).findByRole('button', { name: 'Star theirs' }));
    expect(patched).toEqual([{ id: 'theirs', starred: true }]);
    expect(within(nav).queryByRole('button', { name: 'Actions for theirs' })).not.toBeInTheDocument();
    expect(within(nav).getByRole('button', { name: 'Actions for mine' })).toBeInTheDocument();
    expect(within(nav).queryByRole('button', { name: 'Star Errors' })).not.toBeInTheDocument();
  });

  it('saves the current filters as a shared view, renames it and deletes it', async () => {
    render(<RequestLogScreen />);
    const nav = await screen.findByRole('navigation', { name: 'Saved views' });
    await userEvent.click(within(nav).getByRole('button', { name: /Errors$/ }));
    await userEvent.click(within(nav).getByRole('button', { name: 'Save view' }));
    const dialog = await screen.findByRole('dialog', { name: 'Save the current view' });
    await userEvent.type(within(dialog).getByLabelText('Name'), 'Failures');
    await userEvent.click(within(dialog).getByRole('switch', { name: /Share with the organization/ }));
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save view' }));
    const saved = views.find((v) => v.id === 'new');
    expect(saved).toMatchObject({
      name: 'Failures',
      shared: true,
      starred: true,
      filters: { status_class: ['4xx', '5xx'] },
      sort: 'started_at:desc',
    });
    expect(await within(nav).findByRole('button', { name: /^Failures/ })).toBeInTheDocument();

    await userEvent.click(within(nav).getByRole('button', { name: 'Actions for Failures' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Rename' }));
    const rename = await screen.findByRole('dialog', { name: 'Rename Failures' });
    const field = within(rename).getByLabelText('Name');
    await userEvent.clear(field);
    await userEvent.type(field, 'All failures');
    await userEvent.click(within(rename).getByRole('button', { name: 'Rename view' }));
    expect(patched).toContainEqual({ id: 'new', name: 'All failures' });

    await userEvent.click(within(nav).getByRole('button', { name: 'Actions for All failures' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Delete' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Delete view' }));
    expect(views.some((v) => v.id === 'new')).toBe(false);
  });

  it('shows and hides columns from the chooser and keeps the choice', async () => {
    render(<RequestLogScreen />);
    await screen.findByRole('columnheader', { name: /Cost/ });
    await userEvent.click(screen.getByRole('button', { name: 'Columns' }));
    const chooser = await screen.findByRole('dialog', { name: 'Columns' });
    await userEvent.click(within(chooser).getByRole('checkbox', { name: 'Cost' }));
    await userEvent.click(within(chooser).getByRole('checkbox', { name: 'API key' }));
    expect(screen.queryByRole('columnheader', { name: /^Cost/ })).not.toBeInTheDocument();
    expect(screen.getByRole('columnheader', { name: 'API key' })).toBeInTheDocument();
    expect(usePreferences.getState().logColumns).not.toContain('cost');
    await userEvent.click(within(chooser).getByRole('button', { name: 'Reset' }));
    expect(usePreferences.getState().logColumns).toBeNull();
  });
});

describe('request drawer', () => {
  beforeEach(() => {
    useLogs.setState({ detail: emptySlot() });
    server.use(
      http.get('*/admin/v1/logs/:id', () => HttpResponse.json(record('a'))),
      http.get('*/admin/v1/keys/:name', () =>
        HttpResponse.json({
          name: 'web',
          spec: { project: 'core', endpoints: ['chat', 'embeddings'] },
          metadata: {},
        }),
      ),
    );
  });

  it('shows the overview facts, the conversation, the request parts and a readable response', async () => {
    signInAs(['logs:read', 'logs:read_content', 'keys:read']);
    render(<LogDrawer id="a" onClose={() => undefined} onOpen={() => undefined} />);
    expect(await screen.findByText(/Lisbon, PT/)).toBeInTheDocument();
    expect(screen.getByText('gpt-4o-a → gpt-4o-b')).toBeInTheDocument();
    expect(screen.getByText(/similarity 0\.973/)).toBeInTheDocument();
    expect(await screen.findByText(/may call chat, embeddings/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole('tab', { name: /Messages/ }));
    expect(screen.getByText('Be brief.')).toBeInTheDocument();
    expect(screen.getByText('Hello!')).toBeInTheDocument();

    await userEvent.click(screen.getByRole('tab', { name: 'Request' }));
    await userEvent.click(screen.getByRole('radio', { name: /Params/ }));
    expect(screen.getByText('api-version')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('radio', { name: /Headers/ }));
    expect(screen.getByText('user-agent')).toBeInTheDocument();

    await userEvent.click(screen.getByRole('tab', { name: 'Response' }));
    expect(screen.getByText('Hello!')).toBeInTheDocument();
    expect(screen.getByText(/Finished: stop/)).toBeInTheDocument();
  });

  it('replays the request after a confirmation and opens the new record', async () => {
    signInAs(['logs:read', 'logs:read_content', 'playground:send']);
    let sent: unknown = null;
    server.use(
      http.post('*/admin/v1/logs/:id\\:replay', async ({ request }) => {
        sent = await request.json();
        return HttpResponse.json({ status: 200, headers: {}, body: {}, log_id: 'b', key: 'pg' });
      }),
    );
    let opened: string | null = null;
    render(<LogDrawer id="a" onClose={() => undefined} onOpen={(id) => (opened = id)} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Replay' }));
    const dialog = await screen.findByRole('dialog', { name: 'Replay this request?' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Send again' }));
    expect(sent).toEqual({});
    await userEvent.click(await screen.findByRole('button', { name: 'Open the new request' }));
    expect(opened).toBe('b');
  });

  it('hides replay from readers who may not send', async () => {
    signInAs(['logs:read', 'logs:read_content']);
    render(<LogDrawer id="a" onClose={() => undefined} onOpen={() => undefined} />);
    await screen.findByText(/Lisbon, PT/);
    expect(screen.queryByRole('button', { name: 'Replay' })).not.toBeInTheDocument();
  });
});
