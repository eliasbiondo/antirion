import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { pickOption, signInAs } from '@/test/utils';

import { CacheScreen } from '../components/cache-screen';
import { useCache } from '../store';

const ADMIN = [
  'cache:admin',
  'models:read',
  'models:write',
  'analytics:read',
  'policies:read',
  'security:read',
];
const meta = (etag: string) => ({ etag, generation: 1, created_at: '', updated_at: '', managed_by: 'api' });
const now = () => new Date().toISOString();
const later = (ms: number) => new Date(Date.now() + ms).toISOString();

interface Calls {
  patches: { name: string; body: unknown }[];
  deletes: string[];
  reports: string[];
  rules: { method: string; body: unknown }[];
  invalidations: unknown[];
}

const ENTRY = (i: number, mode: 'exact' | 'semantic') => ({
  key: `${'ab'.repeat(31)}${i.toString(16).padStart(2, '0')}`,
  mode,
  model: 'gpt-4o-mini',
  route: null,
  deployment: 'gpt-4o-mini',
  scope: 'project',
  size_bytes: 500 + i,
  input_tokens: 100,
  output_tokens: 20,
  hits: 3 - i,
  created_at: now(),
  expires_at: later(600_000),
  last_hit_at: now(),
  similarity_threshold: mode === 'semantic' ? 0.93 : null,
  saved_usd: '0.0012',
});

function gateway(options: { dualControl?: boolean; pending?: boolean } = {}): Calls {
  const calls: Calls = { patches: [], deletes: [], reports: [], rules: [], invalidations: [] };
  const models = [
    {
      name: 'gpt-4o-mini',
      spec: {
        target: { deployment: 'gpt-4o-mini' },
        annotations: { note: 'Support bot answers' },
        policies: {
          cache: {
            enabled: true,
            ttl: '15m',
            semantic: { enabled: true, embedding_model: 'embed', threshold: 0.93 },
          },
        },
      },
      metadata: meta('"m1"'),
    },
    { name: 'claude', spec: { target: { deployment: 'claude' } }, metadata: meta('"m2"') },
    { name: 'embed', spec: { target: { deployment: 'embed' } }, metadata: meta('"m3"') },
  ];
  const entries = [ENTRY(0, 'semantic'), ENTRY(1, 'exact'), ENTRY(2, 'exact')];
  server.use(
    http.get('*/admin/v1/runtime/cache', () =>
      HttpResponse.json({
        backend: 'memory',
        healthy: true,
        entries: 3,
        bytes: 1503,
        hits: 5,
        misses: 9,
        semantic: [
          {
            scope: 'model/gpt-4o-mini',
            embedding_model: 'embed',
            threshold: 0.93,
            entries: 1,
            hits: 40,
            mean_similarity_of_hits: 0.9611,
            false_positive_reports: 4,
            false_positive_rate: 0.1,
          },
        ],
      }),
    ),
    http.get('*/admin/v1/settings/cache', () =>
      HttpResponse.json({ spec: { backend: { type: 'memory' } }, metadata: meta('"c"') }),
    ),
    http.get('*/admin/v1/models', () => HttpResponse.json({ items: models, next_cursor: null })),
    http.patch('*/admin/v1/models/:name', async ({ params, request }) => {
      const body = (await request.json()) as { spec: { policies: { cache: Record<string, unknown> } } };
      calls.patches.push({ name: String(params.name), body });
      const m = models.find((x) => x.name === params.name);
      return HttpResponse.json(m);
    }),
    http.get('*/admin/v1/runtime/cache/invalidation_rules', () =>
      HttpResponse.json({
        items: [
          { name: 'flush-on-disable', fires_24h: 4, last_fired_at: now(), last_dropped: 12 },
          { name: 'nightly-refresh', fires_24h: 0, last_fired_at: null, last_dropped: 0 },
        ],
        next_cursor: null,
      }),
    ),
    http.post('*/admin/v1/cache/invalidation_rules', async ({ request }) => {
      calls.rules.push({ method: 'POST', body: await request.json() });
      return HttpResponse.json({}, { status: 201 });
    }),
    http.get('*/admin/v1/cache/entries', ({ request }) => {
      const q = new URL(request.url).searchParams;
      const mode = q.get('mode');
      const items = entries.filter((e) => !mode || e.mode === mode);
      return HttpResponse.json({
        items,
        next_cursor: null,
        enumerable: true,
        total: items.length,
        total_size_bytes: items.reduce((a, e) => a + e.size_bytes, 0),
        total_hits: items.reduce((a, e) => a + e.hits, 0),
      });
    }),
    http.get('*/admin/v1/cache/entries/:key', ({ params }) => {
      const e = entries.find((x) => x.key === params.key);
      return HttpResponse.json({ ...e, response: { choices: [{ message: { content: 'Cached hello' } }] } });
    }),
    http.delete('*/admin/v1/cache/entries/:key', ({ params }) => {
      calls.deletes.push(String(params.key));
      return new HttpResponse(null, { status: 204 });
    }),
    http.post('*/admin/v1/cache/entries/:action', ({ params }) => {
      calls.reports.push(String(params.action));
      return new HttpResponse(null, { status: 204 });
    }),
    http.post('*/admin/v1/cache:invalidate', async ({ request }) => {
      calls.invalidations.push(await request.json());
      return HttpResponse.json({ level: 'model', name: 'gpt-4o-mini', epoch: 1 });
    }),
    http.get('*/admin/v1/pricing/deployments', () =>
      HttpResponse.json({
        items: [
          { deployment: 'gpt-4o-mini', price: { input: '0.15', cache_read: '0.075' }, price_version: 'v' },
        ],
        next_cursor: null,
      }),
    ),
    http.get('*/admin/v1/settings/governance', () =>
      HttpResponse.json({
        spec: { dual_control: { actions: options.dualControl ? ['cache_flush_all'] : [] } },
        metadata: meta('"g"'),
      }),
    ),
    http.get('*/admin/v1/change_requests', () =>
      HttpResponse.json({
        items: options.pending
          ? [
              {
                id: 'cr-1',
                kind: 'action',
                title: 'cache_flush_all',
                author: 'ada',
                created_at: now(),
                changes: [{ method: 'POST', path: '/admin/v1/cache:invalidate', body: { level: 'gateway' } }],
                state: 'open',
                approvals: [],
                approvals_required: 1,
              },
            ]
          : [],
        next_cursor: null,
      }),
    ),
    http.get('*/admin/v1/audit', () => HttpResponse.json({ items: [], next_cursor: null })),
    http.get('*/admin/v1/routes', () => HttpResponse.json({ items: [], next_cursor: null })),
  );
  return calls;
}

/** The i-th element of a query's result, which must be there. */
function nth(list: HTMLElement[], i: number): HTMLElement {
  const el = list[i];
  if (!el) throw new Error(`no element ${i}`);
  return el;
}

function reset(path = '/cache/') {
  window.history.replaceState(null, '', path);
  useCache.setState(useCache.getInitialState(), true);
}

describe('Cache screen', () => {
  beforeEach(() => reset());

  it('compares the hit rate with the day before and shows a model’s settings and 30 days', async () => {
    signInAs(ADMIN);
    gateway();
    render(<CacheScreen />);
    const figures = await screen.findByRole('region', { name: 'Cache figures' });
    await waitFor(() => expect(figures).toHaveTextContent(/Paid to providers, last 24 h\$/));
    expect(figures).toHaveTextContent(/[\d.]+K? of [\d.]+K? cached/);
    await userEvent.click(await screen.findByRole('button', { name: 'Show details of gpt-4o-mini' }));
    const detail = await screen.findByRole('region', { name: 'Settings' });
    expect(detail).toHaveTextContent('Keep responses15 min');
    expect(detail).toHaveTextContent('If the cache is downSend to the provider');
    await waitFor(() => expect(detail).toHaveTextContent('Cached-read discount50% off input'));
    expect(screen.getByText('Support bot answers')).toBeInTheDocument();
    expect(screen.getByRole('region', { name: 'Last 30 days' })).toHaveTextContent('Without the cache');
  });

  it('turns provider prompt caching on as its own switch', async () => {
    signInAs(ADMIN);
    const calls = gateway();
    render(<CacheScreen />);
    await userEvent.click(
      await screen.findByRole('switch', { name: 'Turn on provider prompt caching for claude' }),
    );
    await waitFor(() => expect(calls.patches).toHaveLength(1));
    expect(calls.patches[0]).toEqual({
      name: 'claude',
      body: { spec: { policies: { cache: { prompt_caching: { auto_inject: true } } } } },
    });
  });

  it('writes only the changed settings, with fail closed, per-item embeddings and a key preset', async () => {
    signInAs(ADMIN);
    const calls = gateway();
    render(<CacheScreen />);
    const table = await screen.findByRole('table', { name: 'Caching per model' });
    const row = within(table).getByText('gpt-4o-mini', { selector: 'button div' }).closest('tr');
    if (!row) throw new Error('no row');
    await userEvent.click(within(row).getByRole('button', { name: 'Settings' }));
    const drawer = await screen.findByRole('dialog', { name: 'Cache settings for gpt-4o-mini' });
    await userEvent.click(within(drawer).getByRole('radio', { name: /Also ignore predicted output/ }));
    await userEvent.type(within(drawer).getByLabelText(/Most responses to keep/), '5000');
    await userEvent.click(within(drawer).getByRole('radio', { name: /Refuse requests/ }));
    await userEvent.click(within(drawer).getByRole('switch', { name: /embeddings request on its own/ }));
    await userEvent.click(within(drawer).getByRole('button', { name: 'Save settings' }));
    await waitFor(() => expect(calls.patches).toHaveLength(1));
    expect(calls.patches[0]?.body).toEqual({
      spec: {
        policies: {
          cache: {
            max_entries: 5000,
            key: { ignore_fields: ['/prediction'] },
            fail_mode: 'closed',
            embeddings: { per_item: true },
          },
        },
      },
    });
  });

  it('refuses an ignored field that changes the answer, and needs an embedding model for similar prompts', async () => {
    signInAs(ADMIN);
    const calls = gateway();
    render(<CacheScreen />);
    const table = await screen.findByRole('table', { name: 'Caching per model' });
    const row = within(table).getByText('claude', { selector: 'button div' }).closest('tr');
    if (!row) throw new Error('no row');
    await userEvent.click(within(row).getByRole('button', { name: 'Settings' }));
    const drawer = await screen.findByRole('dialog', { name: 'Cache settings for claude' });
    await userEvent.click(within(drawer).getByRole('radio', { name: /^Custom/ }));
    await userEvent.type(within(drawer).getByLabelText(/Body fields to leave out/), '/temperature');
    await userEvent.click(within(drawer).getByRole('switch', { name: /mean the same thing/ }));
    await userEvent.click(within(drawer).getByRole('button', { name: 'Save settings' }));
    expect(
      await within(drawer).findByText('temperature changes the answer, so it stays in the key'),
    ).toBeVisible();
    expect(within(drawer).getByText('Choose the model that turns prompts into vectors')).toBeVisible();
    await userEvent.clear(within(drawer).getByLabelText(/Body fields to leave out/));
    await userEvent.type(within(drawer).getByLabelText(/Body fields to leave out/), '/request_id');
    await pickOption(within(drawer).getByRole('combobox', { name: 'Embedding model' }), 'embed');
    await userEvent.click(within(drawer).getByRole('button', { name: 'Save settings' }));
    await waitFor(() => expect(calls.patches).toHaveLength(1));
    expect(calls.patches[0]?.body).toMatchObject({
      spec: {
        policies: {
          cache: {
            key: { ignore_fields: ['/request_id'] },
            semantic: { enabled: true, embedding_model: 'embed', threshold: 0.95 },
          },
        },
      },
    });
  });

  it('removes a selection of cached responses and shows the filtered totals', async () => {
    signInAs(ADMIN);
    const calls = gateway();
    reset('/cache/?tab=entries');
    render(<CacheScreen />);
    const table = await screen.findByRole('table', { name: 'Cached responses' });
    expect(await screen.findByText(/Showing 3 of 3/)).toBeInTheDocument();
    expect(screen.getByText('1.5 KB in all')).toBeInTheDocument();
    const boxes = within(table).getAllByRole('checkbox');
    await userEvent.click(nth(boxes, 1));
    await userEvent.click(nth(boxes, 2));
    expect(screen.getByText('2 responses selected')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Remove selected' }));
    const confirm = await screen.findByRole('alertdialog', { name: 'Remove 2 cached responses?' });
    await userEvent.click(within(confirm).getByRole('button', { name: 'Remove responses' }));
    await waitFor(() => expect(calls.deletes).toHaveLength(2));
    await waitFor(() => expect(screen.getByText(/Showing 1 of 1/)).toBeInTheDocument());
  });

  it('shows the cached response only to content readers, and reports a wrong similar match', async () => {
    signInAs([...ADMIN]);
    const calls = gateway();
    reset('/cache/?tab=entries');
    const { unmount } = render(<CacheScreen />);
    const table = await screen.findByRole('table', { name: 'Cached responses' });
    await userEvent.click(nth(within(table).getAllByRole('button', { name: /^abab/ }), 0));
    const drawer = await screen.findByRole('dialog', { name: 'Cached response' });
    expect(await within(drawer).findByText(/Only people who can read request content/)).toBeInTheDocument();
    expect(within(drawer).queryByText(/Cached hello/)).toBeNull();
    await userEvent.click(within(drawer).getByRole('button', { name: 'Report wrong match' }));
    const confirm = await screen.findByRole('alertdialog', { name: 'Report this as a wrong match?' });
    await userEvent.click(within(confirm).getByRole('button', { name: 'Report wrong match' }));
    await waitFor(() => expect(calls.reports).toEqual([`${ENTRY(0, 'semantic').key}:report_false_positive`]));
    unmount();

    reset('/cache/?tab=entries');
    signInAs([...ADMIN, 'logs:read_content']);
    gateway();
    render(<CacheScreen />);
    const again = await screen.findByRole('table', { name: 'Cached responses' });
    await userEvent.click(nth(within(again).getAllByRole('button', { name: /^abab/ }), 1));
    const second = await screen.findByRole('dialog', { name: 'Cached response' });
    expect(await within(second).findByText(/Cached hello/)).toBeInTheDocument();
    expect(within(second).queryByRole('button', { name: 'Report wrong match' })).toBeNull();
  });

  it('opens a cached response from its link, with a copy link, and exports the responses', async () => {
    signInAs([...ADMIN]);
    gateway();
    reset(`/cache/?tab=entries&entry=${ENTRY(1, 'exact').key}`);
    render(<CacheScreen />);
    const drawer = await screen.findByRole('dialog', { name: 'Cached response' });
    expect(within(drawer).getByRole('button', { name: 'Copy link' })).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(new URLSearchParams(window.location.search).get('entry')).toBeNull());
    expect(screen.getByRole('button', { name: 'Export responses (CSV)' })).toBeInTheDocument();
  });

  it('lists rule firings and creates the nightly refresh preset', async () => {
    signInAs(ADMIN);
    const calls = gateway();
    reset('/cache/?tab=rules');
    render(<CacheScreen />);
    const firings = await screen.findByLabelText('Firings of flush-on-disable');
    await waitFor(() => expect(firings).toHaveTextContent('4in 24 h'));
    expect(firings).toHaveTextContent('12 responses');
    expect(screen.getByLabelText('Firings of nightly-refresh')).toHaveTextContent('Never fired');
    await userEvent.click(nth(screen.getAllByRole('button', { name: 'Create rule' }), 0));
    const editor = await screen.findByRole('dialog', { name: 'Create clearing rule' });
    await userEvent.click(within(editor).getByRole('button', { name: /Nightly refresh/ }));
    expect(
      within(editor).getByText(/Every night at 03:00 UTC, clear responses older than 12 h\./),
    ).toBeVisible();
    await userEvent.click(within(editor).getByRole('button', { name: 'Create rule' }));
    await waitFor(() => expect(calls.rules).toHaveLength(1));
    expect(calls.rules[0]?.body).toMatchObject({
      name: 'nightly-refresh',
      spec: {
        trigger: { event: 'schedule', schedule: '0 3 * * *' },
        action: { drop: 'older_than', older_than: '12h' },
      },
    });
  });

  it('shows semantic statistics with a warning when wrong matches are frequent', async () => {
    signInAs(ADMIN);
    gateway();
    reset('/cache/?tab=stats');
    render(<CacheScreen />);
    const table = await screen.findByRole('table', { name: 'Similar-prompt matching' });
    expect(table).toHaveTextContent('0.961');
    expect(table).toHaveTextContent('10% (4)');
    expect(screen.getByText(/raise that threshold/)).toBeInTheDocument();
  });

  it('says a gateway-wide clear needs approval and shows the waiting request', async () => {
    signInAs(ADMIN);
    gateway({ dualControl: true, pending: true });
    render(<CacheScreen />);
    expect(await screen.findByText('Clearing the whole cache is waiting for approval')).toBeInTheDocument();
    expect(screen.getByText(/needs a second person’s approval/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Clear cache…' }));
    const confirm = await screen.findByRole('alertdialog', { name: 'Clear the whole cache?' });
    expect(within(confirm).getByText(/A second person approves this/)).toBeInTheDocument();
  });
});
