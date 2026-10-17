import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { useShell } from '@/stores/shell';
import { signInAs } from '@/test/utils';

import { REDACTED } from '@/features/mcp/fields';

import type { AgentResource } from '../api';
import { AgentsScreen } from '../components/agents-screen';
import { agentErrors, agentForm, agentSpec, EMPTY_AGENT } from '../form';
import { useAgents } from '../store';

const meta = {
  etag: '"a1"',
  generation: 1,
  created_at: '2026-10-01T00:00:00Z',
  updated_at: '2026-10-01T00:00:00Z',
  managed_by: 'api' as const,
};

const weather: AgentResource = {
  name: 'weather',
  spec: { card_url: 'https://weather.example.com/.well-known/agent-card.json', public_card: true },
  metadata: meta,
};
const planner: AgentResource = {
  name: 'planner',
  spec: {
    card_url: 'https://agents.example.com/planner/.well-known/agent-card.json',
    protocol: '1.0',
    enabled: false,
    headers: { authorization: REDACTED },
  },
  metadata: meta,
};

function serve() {
  const writes: { method: string; body: unknown; ifMatch: string | null }[] = [];
  server.use(
    http.get('*/admin/v1/a2a/agents', () =>
      HttpResponse.json({ items: [planner, weather], next_cursor: null }),
    ),
    http.put('*/admin/v1/a2a/agents/:name', async ({ request, params }) => {
      const body = (await request.json()) as { spec: AgentResource['spec'] };
      writes.push({ method: 'PUT', body, ifMatch: request.headers.get('if-match') });
      return HttpResponse.json({ name: String(params.name), spec: body.spec, metadata: meta });
    }),
  );
  return writes;
}

describe('agent form', () => {
  it('keeps a stored header and writes only what differs from the defaults', () => {
    const f = agentForm(planner);
    expect(agentSpec({ ...f, requestTimeout: '60s' }, planner)).toEqual({
      card_url: planner.spec.card_url,
      protocol: '1.0',
      enabled: false,
      headers: { authorization: REDACTED },
      request_timeout: '60s',
    });
  });

  it('checks the name, the card URL and durations', () => {
    expect(agentErrors({ ...EMPTY_AGENT, name: 'Weather', cardUrl: 'weather' }, [], true)).toEqual({
      name: expect.stringContaining('Lowercase') as string,
      cardUrl: 'A URL starting with https://',
    });
    expect(
      agentErrors({ ...EMPTY_AGENT, name: 'weather', cardUrl: 'https://x.example' }, ['weather'], true).name,
    ).toBe('weather is already used');
    expect(
      agentErrors({ ...agentForm(weather), requestTimeout: '5 minutes' }, [], false).requestTimeout,
    ).toBeTruthy();
  });
});

describe('Agents screen', () => {
  beforeEach(() => {
    window.history.replaceState(null, '', '/agents/');
    useAgents.setState({ agents: emptySlot() });
    useShell.setState({ status: { data_plane_url: 'https://gw.example.com' } as never });
  });

  it('lists agents with their address, filters the public cards and opens one', async () => {
    serve();
    signInAs(['mcp:read', 'mcp:write']);
    render(<AgentsScreen />);
    const table = await screen.findByRole('table', { name: 'Agents' });
    expect(within(table).getByText('https://gw.example.com/a2a/weather')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Public cards/ }));
    await waitFor(() => expect(within(table).queryByText('planner')).not.toBeInTheDocument());
    expect(window.location.search).toContain('state=public');
    await userEvent.click(within(table).getByRole('row', { name: 'weather: details' }));
    const drawer = await screen.findByRole('dialog', { name: 'weather' });
    expect(within(drawer).getByText('Public')).toBeInTheDocument();
  });

  it('edits an agent with its ETag and keeps the stored header', async () => {
    const writes = serve();
    signInAs(['mcp:read', 'mcp:write']);
    window.history.replaceState(null, '', '/agents/?agent=planner');
    render(<AgentsScreen />);
    const drawer = await screen.findByRole('dialog', { name: 'planner' });
    await userEvent.click(within(drawer).getByRole('button', { name: 'Edit' }));
    expect(within(drawer).getByLabelText('Header 1 value')).toHaveAttribute(
      'placeholder',
      'Stored, hidden. Type to replace',
    );
    await userEvent.type(within(drawer).getByLabelText(/^Request/), '90s');
    await userEvent.click(within(drawer).getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes[0]?.ifMatch).toBe('"a1"');
    expect(writes[0]?.body).toEqual({
      spec: { ...planner.spec, request_timeout: '90s' },
    });
  });

  it('hides writes from readers', async () => {
    serve();
    signInAs(['mcp:read']);
    render(<AgentsScreen />);
    await screen.findByRole('table', { name: 'Agents' });
    expect(screen.queryByRole('button', { name: 'Add agent' })).not.toBeInTheDocument();
    expect(screen.getByRole('switch', { name: /Turn weather off/ })).toBeDisabled();
  });
});
