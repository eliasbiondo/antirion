import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { addEvent, emptyAssembled, type StreamEvent } from '../body';
import { ResponsePanel } from '../components/response-panel';
import type { Run } from '../use-send';

describe('a stream that fails after words arrived', () => {
  it('keeps the partial answer with the error under it', () => {
    const events: StreamEvent[] = [
      { data: { choices: [{ delta: { content: 'Here is a helpful answer' } }] } },
      { data: { choices: [{ delta: {}, finish_reason: 'stop' }] } },
      { data: { error: { message: 'The provider cut the stream short', code: 'upstream_truncated' } } },
    ];
    const answer = events.reduce(addEvent, emptyAssembled());
    expect(answer.text).toBe('Here is a helpful answer');
    expect(answer.error).toBeTruthy();
    const run: Run = {
      status: 'done',
      input: { project: 'default', endpoint: 'chat_completions', body: {}, stream: true },
      head: { status: 200, headers: {}, logId: null, key: 'k' },
      answer,
      body: null,
      events,
      error: null,
      startedAt: 0,
      firstAt: 1,
      endedAt: 2,
      record: null,
    };
    render(<ResponsePanel run={run} onContinue={null} />);
    expect(screen.getByText('Here is a helpful answer')).toBeInTheDocument();
    expect(screen.getByText(/The stream failed after this part of the answer/)).toBeInTheDocument();
    expect(screen.getByText('Stream failed')).toBeInTheDocument();
    expect(screen.queryByText(/Finished: stop/)).not.toBeInTheDocument();
  });
});
