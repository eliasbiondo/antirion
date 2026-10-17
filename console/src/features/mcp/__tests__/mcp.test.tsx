import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import type { McpGatewayResource, McpServerResource, McpServerStatus } from '../api';
import { McpScreen } from '../components/mcp-screen';
import { REDACTED } from '../fields';
import {
  gatewaySpec,
  gatewayForm,
  serverForm,
  serverHealth,
  serverSpec,
  toolState,
  waitingTools,
} from '../form';
import { useMcp } from '../store';

const meta = {
  etag: '"e1"',
  generation: 1,
  created_at: '2026-10-01T00:00:00Z',
  updated_at: '2026-10-01T00:00:00Z',
  managed_by: 'api' as const,
};

const issues: McpServerResource = {
  name: 'issues',
  spec: {
    transport: 'streamable_http',
    url: 'https://issues.example.com/mcp',
    pin_tool_definitions: true,
    headers: { authorization: REDACTED },
    identity_headers: { 'x-team': 'team' },
  },
  metadata: meta,
};
const billing: McpServerResource = {
  name: 'billing',
  spec: { transport: 'sse', url: 'https://billing.example.com/sse' },
  metadata: meta,
};

function status(over: Partial<McpServerStatus>): McpServerStatus {
  return {
    era_capability: 'unknown',
    last_discovery_at: null,
    last_error: null,
    cached_tools: 0,
    processes: null,
    tools: [],
    approved_at: null,
    last_contact_at: null,
    last_contact_error: null,
    ...over,
  };
}

const FP = (c: string) => c.repeat(64);

const STATUS: Record<string, McpServerStatus> = {
  issues: status({
    last_contact_at: '2026-10-15T10:00:00Z',
    approved_at: '2026-10-15T09:00:00Z',
    tools: [
      { name: 'create_issue', fingerprint: FP('a'), approved: true, hidden: false },
      { name: 'search_issues', fingerprint: FP('b'), approved: false, hidden: true },
    ],
  }),
  billing: status({ last_contact_at: '2026-10-15T10:00:00Z', last_contact_error: 'could not connect' }),
};

const gateway: McpGatewayResource = { name: 'default', spec: { servers: ['issues'] }, metadata: meta };

function serve() {
  const calls: { method: string; path: string; body?: unknown }[] = [];
  server.use(
    http.get('*/admin/v1/mcp/servers', () =>
      HttpResponse.json({ items: [billing, issues], next_cursor: null }),
    ),
    http.get('*/admin/v1/mcp/servers/:name/status', ({ params }) =>
      HttpResponse.json(STATUS[String(params.name)] ?? status({})),
    ),
    http.get('*/admin/v1/mcp/gateways', () => HttpResponse.json({ items: [gateway], next_cursor: null })),
    http.get('*/admin/v1/runtime/mcp/sessions', () =>
      HttpResponse.json({
        items: [
          {
            gateway: 'default',
            session_mode: 'stateful',
            issued: 7,
            rejected: 1,
            revoked: 0,
            key_ids: ['2', '1'],
          },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/admin/v1/settings/mcp', () =>
      HttpResponse.json({ spec: { session: { max_lifetime: '12h' } }, metadata: meta }),
    ),
    http.post('*/admin/v1/mcp/servers/:action', async ({ params, request }) => {
      const action = String(params.action);
      const body = action.includes(':') ? undefined : await request.json();
      calls.push({ method: 'POST', path: action, body });
      if (action === 'issues:approve_tools')
        return HttpResponse.json({ server: 'issues', approved_at: '2026-10-15T11:00:00Z', tools: [] });
      return HttpResponse.json(
        { name: 'docs', spec: (body as { spec: unknown }).spec, metadata: meta },
        { status: 201 },
      );
    }),
    http.post('*/admin/v1/mcp/servers', async ({ request }) => {
      const body = (await request.json()) as { name: string; spec: McpServerResource['spec'] };
      calls.push({ method: 'POST', path: 'servers', body });
      return HttpResponse.json({ name: body.name, spec: body.spec, metadata: meta }, { status: 201 });
    }),
  );
  return calls;
}

describe('MCP forms', () => {
  it('keeps hidden header values and fields the form does not show, and drops what was cleared', () => {
    const f = serverForm(issues);
    expect(f.headers).toEqual([{ name: 'authorization', value: '', stored: true }]);
    const spec = serverSpec({ ...f, url: 'https://new.example.com/mcp', pin: false, prefix: '' }, issues);
    expect(spec).toEqual({
      transport: 'streamable_http',
      url: 'https://new.example.com/mcp',
      headers: { authorization: REDACTED },
      identity_headers: { 'x-team': 'team' },
    });
  });

  it('says what each tool approval means and counts the ones waiting', () => {
    const s = STATUS.issues;
    const [first, second] = s?.tools ?? [];
    expect(first && toolState(first).label).toBe('Approved');
    expect(second && toolState(second).label).toBe('Changed, hidden');
    expect(toolState({ name: 't', fingerprint: FP('c'), approved: null, hidden: false }).label).toBe(
      'Not approved',
    );
    expect(waitingTools(s)).toBe(1);
  });

  it('reads health from the last contact, not from the protocol check', () => {
    expect(serverHealth(billing, STATUS.billing)).toBe('failing');
    expect(
      serverHealth(issues, status({ last_error: 'server/discover was not answered', last_contact_at: 'x' })),
    ).toBe('working');
    expect(serverHealth(issues, status({}))).toBe('unlisted');
    expect(serverHealth({ ...issues, spec: { ...issues.spec, enabled: false } }, STATUS.issues)).toBe('off');
  });

  it('writes a gateway with its servers in order and only the settings that differ from the defaults', () => {
    const f = gatewayForm(gateway);
    expect(gatewaySpec({ ...f, servers: ['issues', 'billing'], toolSearch: true }, gateway)).toEqual({
      servers: ['issues', 'billing'],
      tool_search: true,
    });
    expect(
      gatewaySpec({ ...f, oauth: true, authorizationServers: ['https://idp.example.com'] }, null),
    ).toEqual({
      servers: ['issues'],
      oauth: { authorization_servers: ['https://idp.example.com'] },
    });
  });
});

describe('MCP screen', () => {
  beforeEach(() => {
    window.history.replaceState(null, '', '/mcp/');
    useMcp.setState({
      servers: emptySlot(),
      statuses: {},
      gateways: emptySlot(),
      sessions: emptySlot(),
      settings: emptySlot(),
    });
  });

  it('lists servers with their status and tools waiting, and filters the failing ones from the summary', async () => {
    serve();
    signInAs(['mcp:read', 'mcp:write']);
    render(<McpScreen />);
    const table = await screen.findByRole('table', { name: 'MCP servers' });
    await waitFor(() => expect(within(table).getByText('1 waiting')).toBeInTheDocument());
    expect(within(table).getAllByText('Failing').length).toBeGreaterThan(0);
    const summary = screen.getByRole('region', { name: 'MCP summary' });
    expect(within(summary).getByText('Sessions issued').nextSibling).toHaveTextContent('7');
    await userEvent.click(within(summary).getByRole('button', { name: /Failing/ }));
    await waitFor(() => expect(within(table).queryByText('issues')).not.toBeInTheDocument());
    expect(window.location.search).toContain('state=failing');
  });

  it('opens a server with its tools and approves them', async () => {
    const calls = serve();
    signInAs(['mcp:read', 'mcp:write']);
    window.history.replaceState(null, '', '/mcp/?server=issues');
    render(<McpScreen />);
    const drawer = await screen.findByRole('dialog', { name: 'issues' });
    expect(await within(drawer).findByText('Changed, hidden')).toBeInTheDocument();
    expect(within(drawer).getByRole('button', { name: 'Delete' })).toBeDisabled();
    await userEvent.click(within(drawer).getByRole('button', { name: 'Approve current tools' }));
    const confirm = await screen.findByRole('alertdialog', { name: /Approve the current tools of issues/ });
    await userEvent.click(within(confirm).getByRole('button', { name: 'Approve tools' }));
    await waitFor(() => expect(calls.some((c) => c.path === 'issues:approve_tools')).toBe(true));
  });

  it('adds a server with the fields the operator set', async () => {
    const calls = serve();
    signInAs(['mcp:read', 'mcp:write']);
    render(<McpScreen />);
    await screen.findByRole('table', { name: 'MCP servers' });
    await userEvent.click(screen.getByRole('button', { name: 'Add MCP server' }));
    const drawer = await screen.findByRole('dialog', { name: 'Add MCP server' });
    await userEvent.type(within(drawer).getByLabelText(/^Name/), 'docs');
    await userEvent.type(within(drawer).getByLabelText(/^URL/), 'https://docs.example.com/mcp');
    await userEvent.click(within(drawer).getByRole('button', { name: 'Add header' }));
    await userEvent.type(within(drawer).getByLabelText('Header 1 name'), 'authorization');
    await userEvent.type(within(drawer).getByLabelText('Header 1 value'), 'Bearer t');
    await userEvent.click(within(drawer).getByRole('button', { name: 'Add MCP server' }));
    await waitFor(() =>
      expect(calls.find((c) => c.path === 'servers')?.body).toEqual({
        name: 'docs',
        spec: {
          transport: 'streamable_http',
          url: 'https://docs.example.com/mcp',
          headers: { authorization: 'Bearer t' },
        },
      }),
    );
  });

  it('shows the sessions of each gateway with the session lifetime', async () => {
    serve();
    signInAs(['mcp:read']);
    window.history.replaceState(null, '', '/mcp/?tab=sessions');
    render(<McpScreen />);
    const table = await screen.findByRole('table', { name: 'MCP sessions by gateway' });
    expect(within(table).getByText('2 · signs new')).toBeInTheDocument();
    expect(screen.getByText('12h')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Add MCP server' })).not.toBeInTheDocument();
  });

  it('says so when the gateway does not serve MCP', async () => {
    server.use(
      http.get('*/admin/v1/mcp/servers', () =>
        HttpResponse.json({ error: { code: 'not_found' } }, { status: 404 }),
      ),
      http.get('*/admin/v1/mcp/gateways', () =>
        HttpResponse.json({ error: { code: 'not_found' } }, { status: 404 }),
      ),
      http.get('*/admin/v1/runtime/mcp/sessions', () => HttpResponse.json({ items: [], next_cursor: null })),
      http.get('*/admin/v1/settings/mcp', () => HttpResponse.json({ spec: {}, metadata: meta })),
    );
    signInAs(['mcp:read']);
    render(<McpScreen />);
    expect(await screen.findByText("MCP isn't available on this gateway yet")).toBeInTheDocument();
  });
});
