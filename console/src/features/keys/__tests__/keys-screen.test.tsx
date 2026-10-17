import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { ALL_KEY_PERMISSIONS, signInAs } from '@/test/utils';

import { KeysScreen } from '../components/keys-screen';
import { useKeys } from '../store';

const meta = {
  etag: '"1"',
  generation: 1,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  managed_by: 'api',
  kid: 'k1',
  hint: 'sk-antirion-k1-…abcd',
  created_by: 'ada',
  last_used_at: null,
  disabled_reason: null,
};

/** The first match: the empty state's button comes before the header's. */
function first(elements: HTMLElement[]): HTMLElement {
  const [element] = elements;
  if (!element) throw new Error('no element');
  return element;
}

function liveHandlers(keys: unknown[]) {
  server.use(
    http.get('*/admin/v1/keys', () => HttpResponse.json({ items: keys, next_cursor: null })),
    http.get('*/admin/v1/projects', () =>
      HttpResponse.json({
        items: [{ name: 'default', spec: { team: 'core' }, metadata: meta }],
        next_cursor: null,
      }),
    ),
    http.get('*/admin/v1/teams', () =>
      HttpResponse.json({
        items: [{ name: 'core', spec: { organization: 'default' }, metadata: meta }],
        next_cursor: null,
      }),
    ),
    http.get('*/admin/v1/users', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/admin/v1/runtime/budgets', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/admin/v1/audit', () => HttpResponse.json({ items: [], next_cursor: null })),
  );
}

describe('API keys screen', () => {
  it('lists keys and hides writes from a viewer', async () => {
    useKeys.setState({ items: [], status: 'idle' });
    signInAs(['keys:read']);
    liveHandlers([{ name: 'ci-bot', spec: { project: 'default' }, metadata: meta }]);
    render(<KeysScreen />);
    const list = await screen.findByRole('listbox', { name: 'API keys' });
    expect(within(list).getByText('ci-bot')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Create key' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Rotate' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Edit' })).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'More' }));
    expect(screen.getByRole('menuitem', { name: 'Copy link' })).toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: 'Revoke' })).not.toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: 'Disable' })).not.toBeInTheDocument();
  });

  it('creates a key and shows its secret once', async () => {
    useKeys.setState({ items: [], status: 'idle' });
    signInAs(ALL_KEY_PERMISSIONS);
    liveHandlers([]);
    let created: unknown = null;
    server.use(
      http.post('*/admin/v1/keys', async ({ request }) => {
        created = await request.json();
        return HttpResponse.json(
          {
            name: 'new-key',
            spec: { project: 'default' },
            metadata: meta,
            secret: 'sk-antirion-k1-THESECRET',
          },
          { status: 201 },
        );
      }),
    );
    render(<KeysScreen />);
    const empty = await screen.findByText('No API keys yet');
    expect(empty).toBeInTheDocument();
    await userEvent.click(first(screen.getAllByRole('button', { name: 'Create key' })));
    const drawer = screen.getByRole('dialog', { name: 'Create API key' });
    await userEvent.type(within(drawer).getByLabelText(/^Name/), 'new-key');
    await userEvent.click(within(drawer).getByRole('button', { name: 'Create key' }));
    const reveal = await screen.findByRole('alertdialog', { name: 'Your new API key' });
    expect(reveal).toHaveTextContent('sk-antirion-k1-THESECRET');
    expect(within(reveal).getByText(/ANTIRION_API_KEY/, { selector: 'pre' })).not.toHaveTextContent(
      'THESECRET',
    );
    expect(created).toMatchObject({ name: 'new-key', spec: { project: 'default' } });
    await userEvent.click(within(reveal).getByRole('checkbox'));
    await userEvent.click(within(reveal).getByRole('button', { name: 'Done' }));
    expect(document.body.textContent).not.toContain('THESECRET');
  });

  it('shows the validation message and does not call the API for a bad name', async () => {
    useKeys.setState({ items: [], status: 'idle' });
    signInAs(ALL_KEY_PERMISSIONS);
    liveHandlers([]);
    render(<KeysScreen />);
    await screen.findByText('No API keys yet');
    await userEvent.click(first(screen.getAllByRole('button', { name: 'Create key' })));
    const drawer = screen.getByRole('dialog', { name: 'Create API key' });
    await userEvent.type(within(drawer).getByLabelText(/^Name/), 'Not Valid');
    await userEvent.click(within(drawer).getByRole('button', { name: 'Create key' }));
    expect(await within(drawer).findByText(/Lowercase letters/)).toBeInTheDocument();
  });
});

describe('API key details', () => {
  it('lists an ownerless key as a service key, keeps Edit primary and puts Revoke in More', async () => {
    useKeys.setState({ items: [], status: 'idle' });
    signInAs(ALL_KEY_PERMISSIONS);
    liveHandlers([{ name: 'fresh', spec: { project: 'default' }, metadata: meta }]);
    render(<KeysScreen />);
    const list = await screen.findByRole('listbox', { name: 'API keys' });
    expect(within(list).getByText('Service')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Edit' }).className).toMatch(/bg-accent/);
    expect(screen.getByRole('button', { name: 'Rotate' }).className).not.toMatch(/bg-accent/);
    await userEvent.click(screen.getByRole('button', { name: 'More' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Revoke' }));
    const confirm = screen.getByRole('alertdialog', { name: 'Revoke fresh?' });
    expect(confirm).toHaveTextContent('Apps using this key will stop working immediately');
    // Revoking cannot be undone: the name is typed first, even for a key never used.
    expect(within(confirm).getByRole('button', { name: 'Revoke key' })).toBeDisabled();
    await userEvent.type(within(confirm).getByRole('textbox'), 'fresh');
    expect(within(confirm).getByRole('button', { name: 'Revoke key' })).toBeEnabled();
  });

  it('opens the endpoint list only when the key is limited to some endpoints', async () => {
    useKeys.setState({ items: [], status: 'idle' });
    signInAs(ALL_KEY_PERMISSIONS);
    liveHandlers([]);
    let created: { spec?: { endpoints?: string[]; permissions?: string[] } } | null = null;
    server.use(
      http.post('*/admin/v1/keys', async ({ request }) => {
        created = (await request.json()) as typeof created;
        return HttpResponse.json(
          { name: 'scoped', spec: { project: 'default' }, metadata: meta, secret: 'sk-antirion-k1-X' },
          { status: 201 },
        );
      }),
    );
    render(<KeysScreen />);
    await screen.findByText('No API keys yet');
    await userEvent.click(first(screen.getAllByRole('button', { name: 'Create key' })));
    const drawer = screen.getByRole('dialog', { name: 'Create API key' });
    expect(within(drawer).queryAllByRole('checkbox')).toHaveLength(0);
    await userEvent.type(within(drawer).getByLabelText(/^Name/), 'scoped');
    await userEvent.click(within(drawer).getByRole('radio', { name: 'Only these…' }));
    await userEvent.click(within(drawer).getByRole('checkbox', { name: 'Chat completions' }));
    await userEvent.click(within(drawer).getByRole('button', { name: /Advanced/ }));
    await userEvent.click(within(drawer).getByRole('checkbox', { name: 'Tag requests' }));
    await userEvent.click(within(drawer).getByRole('button', { name: 'Create key' }));
    await screen.findByRole('alertdialog', { name: 'Your new API key' });
    expect(created).toMatchObject({ spec: { endpoints: ['chat'], permissions: ['tags:request'] } });
  });
});
