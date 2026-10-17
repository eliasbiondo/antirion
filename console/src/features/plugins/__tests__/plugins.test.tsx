import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import type { PluginResource } from '../api';
import { PluginsScreen } from '../components/plugins-screen';
import { EMPTY_PLUGIN, pluginErrors, pluginForm, pluginSpec } from '../form';
import { usePlugins } from '../store';

const meta = {
  etag: '"p1"',
  generation: 1,
  created_at: '2026-10-01T00:00:00Z',
  updated_at: '2026-10-01T00:00:00Z',
  managed_by: 'api' as const,
};

const hook: PluginResource = {
  name: 'pii-webhook',
  spec: {
    type: 'webhook',
    hooks: ['request'],
    on_error: 'fail_open',
    webhook: { url: 'https://hooks.example.com/pii', signing_secret: '[REDACTED]' },
  },
  metadata: meta,
};
const reply: PluginResource = {
  name: 'maintenance',
  spec: {
    type: 'builtin',
    hooks: ['request'],
    builtin: { name: 'static_response' },
    config: { content: 'Down.' },
  },
  metadata: meta,
};

function serve() {
  const patches: unknown[] = [];
  server.use(
    http.get('*/admin/v1/plugins', () => HttpResponse.json({ items: [reply, hook], next_cursor: null })),
    http.get('*/admin/v1/runtime/plugins', () =>
      HttpResponse.json({
        items: [
          {
            name: 'pii-webhook',
            calls: 40,
            failures: 4,
            timeouts: 1,
            p50_ms: 120,
            p99_ms: 790,
            last_error: 'timeout',
          },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/admin/v1/settings/policies', () =>
      HttpResponse.json({
        spec: { plugins: { attach: [{ name: 'pii-webhook', locked: false }] } },
        metadata: { ...meta, etag: '"pol"' },
      }),
    ),
    http.patch('*/admin/v1/settings/policies', async ({ request }) => {
      patches.push({ body: await request.json(), ifMatch: request.headers.get('if-match') });
      return HttpResponse.json({ spec: {}, metadata: meta });
    }),
  );
  return patches;
}

describe('plugin form', () => {
  it('keeps the stored signing secret and drops the blocks of another type', () => {
    const f = pluginForm(hook);
    expect(pluginSpec(f, hook)).toEqual(hook.spec);
    expect(
      pluginSpec({ ...f, type: 'builtin', builtin: 'static_response', config: '{"content":"Hi"}' }, hook),
    ).toEqual({
      type: 'builtin',
      hooks: ['request'],
      on_error: 'fail_open',
      builtin: { name: 'static_response' },
      config: { content: 'Hi' },
    });
  });

  it('refuses credential headers, a static reply without content and a module without its hash', () => {
    const errors = pluginErrors(
      { ...EMPTY_PLUGIN, name: 'x', url: 'https://h.example.com', forwardHeaders: ['Authorization'] },
      [],
      true,
    );
    expect(errors.forwardHeaders).toContain('credential');
    expect(
      pluginErrors({ ...EMPTY_PLUGIN, name: 'x', type: 'builtin', builtin: 'static_response' }, [], true)
        .config,
    ).toBe('A static reply needs {"content": "…"}');
    expect(
      pluginErrors({ ...EMPTY_PLUGIN, name: 'x', type: 'wasm', module: 'file:///m.wasm' }, [], true).sha256,
    ).toBeTruthy();
    expect(
      pluginErrors({ ...EMPTY_PLUGIN, name: 'x', hooks: [], url: 'https://h.example.com' }, [], true).hooks,
    ).toBe('Pick at least one');
  });
});

describe('Plugins screen', () => {
  beforeEach(() => {
    window.history.replaceState(null, '', '/plugins/');
    usePlugins.setState({ plugins: emptySlot(), runtime: emptySlot(), policies: emptySlot() });
  });

  it('joins each plugin with its counters and the gateway attachment, and attaches another', async () => {
    const patches = serve();
    signInAs(['plugins:read', 'plugins:write', 'policies:read', 'policies:write']);
    render(<PluginsScreen />);
    const table = await screen.findByRole('table', { name: 'Plugins' });
    await waitFor(() => expect(within(table).getByText('4 · 10%')).toBeInTheDocument());
    expect(within(table).getByText('timeout')).toBeInTheDocument();
    expect(within(table).getByRole('switch', { name: 'Run pii-webhook on every request' })).toBeChecked();
    await userEvent.click(within(table).getByRole('switch', { name: 'Run maintenance on every request' }));
    // Putting a plugin in the path of every request asks first and says what happens on a failure.
    const ask = await screen.findByRole('alertdialog', { name: 'Run maintenance on every request?' });
    expect(ask).toHaveTextContent(/Every request will wait on maintenance/);
    expect(patches).toHaveLength(0);
    await userEvent.click(within(ask).getByRole('button', { name: 'Run on every request' }));
    await waitFor(() => expect(patches).toHaveLength(1));
    expect(patches[0]).toEqual({
      body: { spec: { plugins: { attach: [{ name: 'pii-webhook', locked: false }, 'maintenance'] } } },
      ifMatch: '"pol"',
    });
  });

  it('opens a plugin and refuses to delete it while it is attached', async () => {
    serve();
    signInAs(['plugins:read', 'plugins:write', 'policies:read']);
    window.history.replaceState(null, '', '/plugins/?plugin=pii-webhook');
    render(<PluginsScreen />);
    const drawer = await screen.findByRole('dialog', { name: 'pii-webhook' });
    expect(await within(drawer).findByText('Its last call failed')).toBeInTheDocument();
    expect(within(drawer).getByRole('button', { name: 'Delete' })).toBeDisabled();
    expect(within(drawer).getByText('Yes, with a stored secret')).toBeInTheDocument();
  });
});
