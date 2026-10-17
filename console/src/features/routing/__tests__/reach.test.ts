import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';

import { routingApi } from '../api';

describe('what a route meets', () => {
  it('reads the Models that name the route and what it does with unmatched requests', async () => {
    server.use(
      http.get('*/admin/v1/routes/:name/state', () =>
        HttpResponse.json({ items: [], next_cursor: null, unmatched: 'error', models: [] }),
      ),
    );
    expect(await routingApi.state('global')).toEqual({
      items: [],
      reach: { unmatched: 'error', models: [] },
    });
  });

  it('reads an older gateway as saying nothing', async () => {
    server.use(
      http.get('*/admin/v1/routes/:name/state', () => HttpResponse.json({ items: [], next_cursor: null })),
    );
    expect((await routingApi.state('global')).reach).toEqual({ unmatched: null, models: null });
  });
});
