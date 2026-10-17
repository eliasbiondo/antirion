import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { beforeEach, describe, expect, it } from 'vitest';

import { server } from '@/mocks/node';
import { signInAs } from '@/test/utils';

import { useCatalog, useModels } from '@/features/models/store';
import { emptySlot } from '@/lib/slot';

import { PlaygroundScreen } from '../components/playground-screen';
import { usePlaygroundDraft } from '../draft';

const project = { name: 'lab', spec: { team: 'alpha' }, metadata: {} };
const model = { name: 'm', spec: { target: 'dep' }, metadata: {} };

function sse(frames: string[]): Response {
  const encoder = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const f of frames) controller.enqueue(encoder.encode(f));
      controller.close();
    },
  });
  return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
}

describe('Playground', () => {
  let sent: { accept: string | null; body: unknown } | null = null;
  beforeEach(() => {
    sent = null;
    window.history.replaceState(null, '', '/playground/');
    useModels.setState({ items: [], error: null });
    useCatalog.setState({ deployments: emptySlot(), routes: emptySlot() });
    usePlaygroundDraft.setState({ draft: null });
    server.use(
      http.get('*/admin/v1/projects', () => HttpResponse.json({ items: [project], next_cursor: null })),
      http.get('*/admin/v1/models', () => HttpResponse.json({ items: [model], next_cursor: null })),
      http.get('*/admin/v1/logs/:id', () =>
        HttpResponse.json({ error: { code: 'not_found', message: 'x' } }, { status: 404 }),
      ),
      http.post('*/admin/v1/playground:send', async ({ request }) => {
        sent = { accept: request.headers.get('accept'), body: await request.json() };
        return sse([
          'event: playground\ndata: {"status":200,"headers":{"x-antirion-deployment":"dep","x-antirion-provider":"openai"},"log_id":"log-1","key":"playground-abc"}\n\n',
          'data: {"choices":[{"delta":{"content":"Ahoy "}}]}\n\n',
          'data: {"choices":[{"delta":{"content":"there."},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}\n\n',
          'data: [DONE]\n\n',
        ]);
      }),
    );
  });

  it('is shown only to principals who may send through it', () => {
    signInAs(['models:read']);
    render(<PlaygroundScreen />);
    expect(screen.getByText("You don't have access to the playground")).toBeInTheDocument();
  });

  it('streams an answer, with its usage, the key it used and a link to the record', async () => {
    signInAs(['playground:send', 'models:read', 'projects:read', 'logs:read']);
    const user = userEvent.setup();
    render(<PlaygroundScreen />);
    await waitFor(() => expect(screen.getByRole('combobox', { name: /Model/ })).toHaveTextContent('m'));
    await waitFor(() => expect(screen.getByRole('combobox', { name: /Project/ })).toHaveTextContent('lab'));
    await user.type(screen.getByRole('textbox', { name: 'User message 1' }), 'Hello');
    await user.click(screen.getByRole('button', { name: 'Send' }));
    expect(await screen.findByText('Ahoy there.')).toBeInTheDocument();
    expect(sent).toMatchObject({
      accept: 'text/event-stream, application/json',
      body: {
        project: 'lab',
        endpoint: 'chat_completions',
        stream: true,
        body: { model: 'm', messages: [{ role: 'user', content: 'Hello' }] },
      },
    });
    expect(screen.getByText('5 · 2')).toBeInTheDocument();
    expect(screen.getByText('playground-abc')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'View in request log' })).toHaveAttribute(
      'href',
      expect.stringContaining('request=log-1'),
    );
  });

  it('opens a body handed over by another screen, as JSON when the form cannot hold it', () => {
    signInAs(['playground:send', 'models:read']);
    usePlaygroundDraft.setState({
      draft: {
        endpoint: 'chat_completions',
        body: { model: 'm', messages: [{ role: 'user', content: 'hi' }], tools: [{ type: 'function' }] },
        project: 'lab',
        source: 'log-9',
      },
    });
    render(<PlaygroundScreen />);
    expect(screen.getByText('Opened from a logged request.')).toBeInTheDocument();
    const editor = screen.getByRole('textbox', { name: /Request body/ });
    expect((editor as HTMLTextAreaElement).value).toContain('"tools"');
  });
});
