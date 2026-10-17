import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';

import { logsApi } from '../api';
import { EMPTY_LOG_FILTERS, filtersToQuery } from '../types';

const bucket = (group: Record<string, string | null>, requests: number) => ({
  start: '2026-10-16T00:00:00Z',
  group,
  requests,
  errors: 0,
  cost_usd: '0',
});

describe('the Model facet', () => {
  it('lists a name no Model has by the name asked for, and filters it with requested_model', async () => {
    let groupBy: string[] = [];
    server.use(
      http.get('*/admin/v1/analytics/usage', ({ request }) => {
        groupBy = new URL(request.url).searchParams.getAll('group_by');
        return HttpResponse.json({
          items: [
            bucket({ model: 'gpt-4o', requested_model: 'gpt-4o' }, 40),
            bucket({ model: 'gpt-4o', requested_model: 'fast' }, 10),
            bucket({ model: null, requested_model: 'gpt-5-preview' }, 121),
            // Refused before routing (an exhausted budget): the Model's own name, no `model`.
            bucket({ model: null, requested_model: 'gpt-4o' }, 5),
          ],
          next_cursor: null,
        });
      }),
    );
    const values = await logsApi.facets(
      {
        from: '',
        to: '',
        filters: EMPTY_LOG_FILTERS,
        sort: 'started_at',
        order: 'desc',
        organization: null,
        tag: null,
      },
      'model',
    );
    expect(groupBy).toEqual(['model', 'requested_model']);
    expect(values).toEqual([
      { value: 'gpt-5-preview', count: 121, requested: true },
      { value: 'gpt-4o', count: 55 },
    ]);
    expect(filtersToQuery({ ...EMPTY_LOG_FILTERS, requestedModel: ['gpt-5-preview'] })).toEqual({
      requested_model: ['gpt-5-preview'],
    });
  });
});
