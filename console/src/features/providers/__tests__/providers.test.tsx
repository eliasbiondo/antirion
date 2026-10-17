import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { emptySlot } from '@/lib/slot';
import { server } from '@/mocks/node';
import { useUi } from '@/stores/ui';
import { pickOption, signInAs } from '@/test/utils';

import { ProvidersScreen } from '../components/providers-screen';
import { EMPTY_PROVIDER, providerSchema, suggestName, toProviderSpec } from '../form';
import { keyState, providerKeyStatus, rotation } from '../keys';
import { useProviders } from '../store';

const TYPES = [
  {
    type: 'openai',
    core: true,
    default_base_url: 'https://api.openai.com/v1',
    dialects: ['openai_chat'],
    auth_methods: ['api_key'],
  },
  {
    type: 'bedrock',
    core: true,
    default_base_url: 'https://bedrock-runtime.{region}.amazonaws.com',
    dialects: ['bedrock_converse'],
    auth_methods: ['aws_sigv4', 'api_key'],
  },
  {
    type: 'openai_compatible',
    core: true,
    default_base_url: null,
    dialects: ['openai_chat'],
    auth_methods: ['api_key', 'none'],
  },
];

const meta = {
  etag: '"1"',
  generation: 1,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  managed_by: 'api',
};

function handlers(providers: unknown[]) {
  server.use(
    http.get('*/admin/v1/providers', () => HttpResponse.json({ items: providers, next_cursor: null })),
    http.get('*/admin/v1/provider-types', () => HttpResponse.json({ items: TYPES, next_cursor: null })),
    http.get('*/admin/v1/runtime/providers', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/admin/v1/deployments', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/admin/v1/runtime/deployments', () => HttpResponse.json({ items: [], next_cursor: null })),
  );
}

function reset() {
  useProviders.setState({
    providers: emptySlot(),
    status: emptySlot(),
    types: emptySlot(),
    deployments: emptySlot(),
    health: emptySlot(),
  });
}

describe('provider form', () => {
  it('suggests a free name for a type', () => {
    expect(suggestName('openai', [])).toBe('openai');
    expect(suggestName('openai', ['openai', 'openai-2'])).toBe('openai-3');
  });

  it('writes only what the operator set, with the type blocks it needs', () => {
    expect(toProviderSpec({ ...EMPTY_PROVIDER, type: 'openai', name: 'openai', apiKey: ' sk-1 ' })).toEqual({
      type: 'openai',
      auth: { method: 'api_key', api_key: 'sk-1' },
    });
    expect(
      toProviderSpec({
        ...EMPTY_PROVIDER,
        type: 'bedrock',
        name: 'b',
        method: 'aws_sigv4',
        region: 'us-east-1',
      }),
    ).toEqual({ type: 'bedrock', aws: { region: 'us-east-1' }, auth: { method: 'aws_sigv4' } });
  });

  it('asks for a base URL only when the type has no default, and for the key with api_key', () => {
    const compatible = TYPES[2];
    const result = providerSchema(compatible, ['taken']).safeParse({
      ...EMPTY_PROVIDER,
      type: 'openai_compatible',
      name: 'taken',
    });
    expect(result.success).toBe(false);
    const paths = result.error?.issues.map((i) => i.path.join('.')) ?? [];
    expect(paths).toEqual(expect.arrayContaining(['name', 'baseUrl', 'apiKey']));
    expect(
      providerSchema(TYPES[0], []).safeParse({ ...EMPTY_PROVIDER, type: 'openai', name: 'o', apiKey: 'k' })
        .success,
    ).toBe(true);
  });
});

describe('Settings › Providers', () => {
  it('teaches the first step on a new gateway and adds a provider', async () => {
    reset();
    signInAs(['providers:read', 'providers:write', 'models:read']);
    handlers([]);
    let posted: unknown = null;
    server.use(
      http.post('*/admin/v1/providers', async ({ request }) => {
        posted = await request.json();
        return HttpResponse.json(
          {
            name: 'openai',
            spec: { type: 'openai', auth: { api_key: '[REDACTED]' } },
            metadata: { ...meta, secret_hints: { 'auth.api_key': '…1234' } },
          },
          { status: 201 },
        );
      }),
    );
    render(<ProvidersScreen />);
    expect(await screen.findByText('No providers yet')).toBeInTheDocument();
    // The page header's primary action, and the empty state's.
    expect(screen.getAllByRole('button', { name: 'Add provider' })).toHaveLength(2);
    const [headerAdd] = screen.getAllByRole('button', { name: 'Add provider' });
    await userEvent.click(headerAdd ?? document.body);
    const drawer = screen.getByRole('dialog', { name: 'Add provider' });
    await userEvent.click(within(drawer).getByRole('button', { name: 'Add provider' }));
    expect(within(drawer).getByText('Select a provider', { selector: '[role=alert]' })).toBeInTheDocument();
    await pickOption(await within(drawer).findByRole('combobox', { name: /^Provider/ }), 'OpenAI');
    expect(within(drawer).getByLabelText(/^Name/)).toHaveValue('openai');
    expect(within(drawer).getByText('Leave blank to use https://api.openai.com/v1.')).toBeInTheDocument();
    await userEvent.type(within(drawer).getByLabelText(/^API key/), 'sk-test-1234');
    await userEvent.click(within(drawer).getByRole('button', { name: 'Add provider' }));
    expect(await screen.findByRole('table', { name: 'Providers' })).toBeInTheDocument();
    expect(posted).toEqual({
      name: 'openai',
      spec: { type: 'openai', auth: { method: 'api_key', api_key: 'sk-test-1234' } },
    });
    expect(screen.queryByRole('dialog', { name: 'Add provider' })).not.toBeInTheDocument();
  });

  it('asks before turning a provider off, and the toast undoes it', async () => {
    reset();
    signInAs(['providers:read', 'providers:write', 'models:read']);
    handlers([{ name: 'openai', spec: { type: 'openai' }, metadata: meta }]);
    const patches: unknown[] = [];
    server.use(
      http.patch('*/admin/v1/providers/openai', async ({ request }) => {
        const body = (await request.json()) as { spec: { enabled: boolean } };
        patches.push(body.spec.enabled);
        return HttpResponse.json({ name: 'openai', spec: { type: 'openai', ...body.spec }, metadata: meta });
      }),
    );
    render(<ProvidersScreen />);
    await userEvent.click(await screen.findByRole('switch', { name: /openai/ }));
    const dialog = await screen.findByRole('alertdialog', { name: 'Turn openai off?' });
    expect(patches).toEqual([]);
    expect(dialog).toHaveTextContent('No deployment uses it yet');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Turn off' }));
    await waitFor(() => expect(patches).toEqual([false]));
    const toast = useUi.getState().toasts.find((t) => t.title === 'openai turned off');
    expect(toast?.action?.label).toBe('Undo');
    toast?.action?.onClick();
    await waitFor(() => expect(patches).toEqual([false, true]));
  });

  it('hides writes from a viewer', async () => {
    reset();
    signInAs(['providers:read']);
    handlers([{ name: 'openai', spec: { type: 'openai' }, metadata: meta }]);
    render(<ProvidersScreen />);
    expect(screen.getByRole('heading', { level: 1, name: 'Providers' })).toBeInTheDocument();
    const table = await screen.findByRole('table', { name: 'Providers' });
    expect(within(table).getByText('openai', { selector: '.mono' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Add provider' })).not.toBeInTheDocument();
    expect(screen.getByRole('switch', { name: /openai/ })).toBeDisabled();
  });
});

const DAY = 86_400_000;

describe('provider keys', () => {
  it('measures the oldest key against the rotation policy', () => {
    const p = {
      name: 'o',
      spec: { type: 'openai', credential_rotation_max_age: '30d' },
      metadata: {
        ...meta,
        credential_rotated_at: { a: '2026-01-01T00:00:00Z', b: '2026-01-20T00:00:00Z' },
        rotation_due: true,
      },
    };
    const r = rotation(p as never, Date.parse('2026-02-05T00:00:00Z'));
    expect(r).toEqual({ ageDays: 35, maxDays: 30, due: true });
    expect(rotation({ ...p, metadata: meta } as never, Date.now())).toBeNull();
    expect(Math.round((r?.ageDays ?? 0) * DAY)).toBe(35 * DAY);
  });

  it('says a key is not working when any deployment set it aside', () => {
    const health = [
      {
        name: 'd1',
        keys: [
          { key: 'a', scope_state: 'alive', pair_state: 'cooling', cooldown_until: '2026-01-01T00:01:00Z' },
        ],
      },
      {
        name: 'd2',
        keys: [
          {
            key: 'a',
            scope_state: 'dead',
            dead_reason: 'quota',
            recheck_at: '2026-01-01T01:00:00Z',
            pair_state: 'dead',
          },
        ],
      },
      {
        name: 'd3',
        keys: [
          { key: 'b', scope_state: 'alive', pair_state: 'alive', last_success_at: '2026-01-01T00:00:00Z' },
          { key: 'c', scope_state: 'alive', pair_state: 'alive', last_success_at: null },
        ],
      },
    ];
    expect(keyState('a', ['d1', 'd2'], health as never)).toEqual({
      state: 'not_working',
      reason: 'quota',
      until: '2026-01-01T01:00:00Z',
    });
    expect(keyState('a', ['d1'], health as never).state).toBe('resting');
    expect(keyState('b', ['d3'], health as never).state).toBe('working');
    expect(keyState('b', ['d3'], null).state).toBe('unknown');
    // Nothing failing is not evidence: a key that never answered is untested.
    expect(keyState('c', ['d3'], health as never).state).toBe('untested');
  });

  it('reads Working in the table only once a key has answered', () => {
    const row = (state: 'working' | 'untested') => ({
      name: 'k',
      hint: undefined,
      weight: null,
      rotatedAt: null,
      health: { state, reason: null, until: null },
      readings: null,
    });
    expect(providerKeyStatus('valid', [row('untested')])).toBe('untested');
    expect(providerKeyStatus('valid', [row('working'), row('untested')])).toBe('valid');
    expect(providerKeyStatus('failed', [row('working')])).toBe('failed');
  });
});

describe('Providers › keys and limits', () => {
  it('opens a provider with its tier, the age, limits and state of each key, and puts a key back in use', async () => {
    reset();
    signInAs(['providers:read', 'providers:write', 'models:read', 'health:read', 'health:write']);
    const now = new Date();
    const provider = {
      name: 'openai',
      spec: {
        type: 'openai',
        account_tier: 'Tier 4',
        credential_rotation_max_age: '30d',
        auth: {
          api_keys: [
            { name: 'primary', key: '[REDACTED]' },
            { name: 'backup', key: '[REDACTED]' },
          ],
        },
      },
      metadata: {
        ...meta,
        secret_hints: { 'auth.api_keys[0].key': '…0001', 'auth.api_keys[1].key': '…0002' },
        credential_rotated_at: {
          primary: new Date(now.getTime() - 40 * DAY).toISOString(),
          backup: new Date(now.getTime() - 2 * DAY).toISOString(),
        },
        rotation_due: true,
      },
    };
    handlers([provider]);
    const resets: string[] = [];
    server.use(
      http.get('*/admin/v1/runtime/providers', () =>
        HttpResponse.json({
          items: [
            {
              name: 'openai',
              type: 'openai',
              credential: { method: 'api_key', state: 'valid' },
              account_tier: 'Tier 4',
              rate_limits: null,
              credentials: [
                {
                  name: 'primary',
                  rate_limits: {
                    requests: { limit: 500, remaining: 120, reset_at: null },
                    observed_at: now.toISOString(),
                  },
                },
              ],
            },
          ],
          next_cursor: null,
        }),
      ),
      http.get('*/admin/v1/deployments', () =>
        HttpResponse.json({
          items: [{ name: 'gpt', spec: { provider: 'openai', model: 'gpt' }, metadata: meta }],
          next_cursor: null,
        }),
      ),
      http.get('*/admin/v1/runtime/deployments', () =>
        HttpResponse.json({
          items: [
            {
              name: 'gpt',
              state: 'closed',
              trips: 0,
              consecutive: 0,
              window: { requests: 1, failures: 0 },
              open_until: null,
              cooldown_until: null,
              disabled_until: null,
              keys: [
                { key: 'primary', scope_state: 'alive', pair_state: 'alive' },
                { key: 'backup', scope_state: 'dead', dead_reason: 'credential', pair_state: 'dead' },
              ],
            },
          ],
          next_cursor: null,
        }),
      ),
      http.post('*/admin/v1/runtime/providers/:provider/keys/:key', ({ params }) => {
        resets.push(`${String(params.provider)}/${String(params.key)}`);
        return new HttpResponse(null, { status: 204 });
      }),
    );
    render(<ProvidersScreen />);
    const table = await screen.findByRole('table', { name: 'Providers' });
    expect(await within(table).findByText('40 days old')).toBeInTheDocument();
    expect(within(table).getByText('Replace now')).toBeInTheDocument();
    await userEvent.click(within(table).getByRole('row', { name: 'openai: keys and limits' }));
    const drawer = await screen.findByRole('dialog', { name: 'openai' });
    expect(within(drawer).getByText('Tier 4')).toBeInTheDocument();
    expect(within(drawer).getByText('Replace keys every 30 days')).toBeInTheDocument();
    expect(within(drawer).getByText(/120 of 500 left/)).toBeInTheDocument();
    expect(within(drawer).getByText('Not working')).toBeInTheDocument();
    expect(within(drawer).getByText(/the provider refused it/)).toBeInTheDocument();
    await userEvent.click(within(drawer).getByRole('button', { name: 'Use it again now' }));
    await waitFor(() => expect(resets).toEqual(['openai/backup:reset']));
  });
});

describe('providers', () => {
  it("can't test a provider with no deployment, and exports the table", async () => {
    reset();
    signInAs(['providers:read', 'providers:write', 'models:read']);
    handlers([{ name: 'lonely', spec: { type: 'openai' }, metadata: meta }]);
    render(<ProvidersScreen />);
    await screen.findByRole('table', { name: 'Providers' });
    expect(screen.getByRole('button', { name: 'Export CSV' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Actions for lonely' }));
    expect(await screen.findByRole('menuitem', { name: /Test connection/ })).toHaveAttribute(
      'aria-disabled',
      'true',
    );
  });

  it('says what the base URL accepts', () => {
    const parse = (baseUrl: string) =>
      providerSchema(TYPES[2], []).safeParse({
        ...EMPTY_PROVIDER,
        type: 'openai_compatible',
        name: 'x',
        apiKey: 'k',
        baseUrl,
      });
    expect(parse('http://127.0.0.1:8080/v1').success).toBe(true);
    const bad = parse('ftp://x');
    expect(bad.success).toBe(false);
    expect(bad.error?.issues[0]?.message).toBe('Enter a URL that starts with https:// or http://');
  });
});
