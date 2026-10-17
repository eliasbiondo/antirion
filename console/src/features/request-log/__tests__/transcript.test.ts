import { describe, expect, it } from 'vitest';

import { requestMessages, responseView, storedBody } from '../transcript';

describe('a logged conversation in plain terms', () => {
  it('reads chat completions with a tool call, its result and the answer', () => {
    const sent = requestMessages({
      model: 'gpt-4o',
      messages: [
        { role: 'system', content: 'Be brief.' },
        {
          role: 'user',
          content: [
            { type: 'text', text: 'Revenue?' },
            { type: 'image_url', image_url: {} },
          ],
        },
        {
          role: 'assistant',
          content: null,
          tool_calls: [{ id: 'c1', type: 'function', function: { name: 'query', arguments: '{"m":9}' } }],
        },
        { role: 'tool', tool_call_id: 'c1', content: '{"revenue":1}' },
      ],
    });
    expect(sent.map((m) => m.role)).toEqual(['system', 'user', 'assistant', 'tool']);
    expect(sent[1]?.text).toBe('Revenue?');
    expect(sent[1]?.attachments).toEqual(['image url']);
    expect(sent[2]?.toolCalls[0]).toEqual({ id: 'c1', name: 'query', arguments: '{\n  "m": 9\n}' });
    expect(sent[3]?.toolCallId).toBe('c1');

    const answer = responseView({
      choices: [{ message: { role: 'assistant', content: 'About 1.' }, finish_reason: 'stop' }],
    });
    expect(answer.messages[0]).toMatchObject({ role: 'assistant', text: 'About 1.', response: true });
    expect(answer.finishReasons).toEqual(['stop']);
  });

  it('reads messages with content blocks, tool use and tool results', () => {
    const sent = requestMessages({
      system: [{ type: 'text', text: 'Rules' }],
      messages: [
        { role: 'user', content: 'Hi' },
        { role: 'assistant', content: [{ type: 'tool_use', id: 't1', name: 'lookup', input: { q: 'x' } }] },
        { role: 'user', content: [{ type: 'tool_result', tool_use_id: 't1', content: 'found' }] },
      ],
    });
    expect(sent.map((m) => m.role)).toEqual(['system', 'user', 'assistant', 'tool']);
    expect(sent[2]?.toolCalls[0]?.name).toBe('lookup');
    expect(sent[3]).toMatchObject({ text: 'found', toolCallId: 't1' });
    const answer = responseView({
      role: 'assistant',
      content: [{ type: 'text', text: 'Done' }],
      stop_reason: 'end_turn',
    });
    expect(answer.messages[0]?.text).toBe('Done');
    expect(answer.finishReasons).toEqual(['end_turn']);
  });

  it('reads responses input items and output, and contents with parts', () => {
    const sent = requestMessages({
      instructions: 'Short answers',
      input: [
        { role: 'user', content: [{ type: 'input_text', text: 'Weather?' }] },
        { type: 'function_call', call_id: 'f1', name: 'weather', arguments: '{}' },
        { type: 'function_call_output', call_id: 'f1', output: 'sunny' },
      ],
    });
    expect(sent.map((m) => m.role)).toEqual(['system', 'user', 'assistant', 'tool']);
    const out = responseView({
      status: 'completed',
      output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Sunny.' }] }],
    });
    expect(out.messages[0]?.text).toBe('Sunny.');

    const parts = requestMessages({
      systemInstruction: { parts: [{ text: 'Be kind' }] },
      contents: [
        { role: 'user', parts: [{ text: 'Hello' }] },
        { role: 'model', parts: [{ functionCall: { name: 'f', args: { a: 1 } } }] },
      ],
    });
    expect(parts.map((m) => m.role)).toEqual(['system', 'user', 'assistant']);
    expect(parts[2]?.toolCalls[0]?.name).toBe('f');
  });

  it('shows an error envelope as an error, and a prompt string as a user message', () => {
    expect(responseView({ error: { message: 'Rate limited', type: 'rate_limit_error' } }).error).toEqual({
      message: 'Rate limited',
      type: 'rate_limit_error',
    });
    expect(requestMessages({ prompt: 'Once upon' })[0]).toMatchObject({ role: 'user', text: 'Once upon' });
    expect(requestMessages({ input: ['a', 'b'] })).toHaveLength(2);
    expect(requestMessages(null)).toEqual([]);
  });

  it('reads the stored body with its truncation', () => {
    expect(storedBody({ body: 'abc', truncated: true, original_bytes: 9000 })).toEqual({
      body: 'abc',
      truncated: true,
      bytes: 9000,
    });
    expect(storedBody(null)).toBeNull();
  });
});
