import { describe, expect, it } from 'vitest';

import {
  addEvent,
  assembleBody,
  buildBody,
  composerErrors,
  emptyAssembled,
  emptyComposer,
  endpointOfPath,
  parseBody,
  splitEvents,
  type StreamEvent,
} from '../body';

describe('playground bodies', () => {
  it('writes each dialect: system, conversation, parameters', () => {
    const c = {
      ...emptyComposer('chat_completions', 'm'),
      system: 'Be brief.',
      turns: [{ role: 'user' as const, text: 'hi' }],
      temperature: '0.2',
      maxTokens: '64',
      stop: 'END',
    };
    expect(buildBody(c)).toEqual({
      model: 'm',
      messages: [
        { role: 'system', content: 'Be brief.' },
        { role: 'user', content: 'hi' },
      ],
      max_tokens: 64,
      stop: ['END'],
      temperature: 0.2,
    });
    expect(buildBody({ ...c, endpoint: 'messages' })).toEqual({
      model: 'm',
      system: 'Be brief.',
      messages: [{ role: 'user', content: 'hi' }],
      max_tokens: 64,
      stop_sequences: ['END'],
      temperature: 0.2,
    });
    expect(buildBody({ ...c, endpoint: 'responses' })).toEqual({
      model: 'm',
      instructions: 'Be brief.',
      input: 'hi',
      max_output_tokens: 64,
      temperature: 0.2,
    });
    expect(buildBody({ ...c, endpoint: 'embeddings', input: 'a\nb' })).toEqual({
      model: 'm',
      input: ['a', 'b'],
    });
  });

  it('checks the form before sending', () => {
    const c = emptyComposer('messages', 'm');
    expect(composerErrors(c).turns).toBeDefined();
    expect(composerErrors({ ...c, maxTokens: '' }).maxTokens).toBe('Messages needs a maximum.');
    expect(composerErrors({ ...c, temperature: '3' }).temperature).toBeDefined();
    expect(composerErrors({ ...c, turns: [{ role: 'user', text: 'x' }] })).toEqual({});
  });

  it('reads a body back into the form, or keeps it as JSON when the form cannot say it all', () => {
    const plain = { model: 'm', stream: true, messages: [{ role: 'user', content: 'hello' }] };
    const read = parseBody('chat_completions', plain);
    expect(read.exact).toBe(true);
    expect(read.composer.turns).toEqual([{ role: 'user', text: 'hello' }]);
    expect(read.composer.stream).toBe(true);
    const tools = { ...plain, tools: [{ type: 'function' }] };
    expect(parseBody('chat_completions', tools).exact).toBe(false);
    expect(endpointOfPath('/v1/messages')).toBe('messages');
    expect(endpointOfPath('/v1/images/generations')).toBeNull();
  });

  it('splits an event stream across reads', () => {
    const first = splitEvents('event: playground\ndata: {"status":200}\n\ndata: {"a"');
    expect(first.events).toEqual([{ event: 'playground', data: { status: 200 } }]);
    const second = splitEvents(`${first.rest}:1}\n\ndata: [DONE]\n\n`);
    expect(second.events).toEqual([{ data: { a: 1 } }, { data: '[DONE]' }]);
    expect(second.rest).toBe('');
  });

  it('assembles streamed answers of every dialect', () => {
    const chat: StreamEvent[] = [
      { data: { choices: [{ delta: { content: 'Hel' } }] } },
      {
        data: {
          choices: [{ delta: { content: 'lo' }, finish_reason: 'stop' }],
          usage: { prompt_tokens: 3, completion_tokens: 2 },
        },
      },
      { data: '[DONE]' },
    ];
    const a = chat.reduce(addEvent, emptyAssembled());
    expect(a).toMatchObject({ text: 'Hello', finish: 'stop', usage: { input: 3, output: 2 } });
    const messages: StreamEvent[] = [
      { event: 'message_start', data: { type: 'message_start', message: { usage: { input_tokens: 9 } } } },
      { event: 'content_block_delta', data: { type: 'content_block_delta', delta: { text: 'Hi' } } },
      {
        event: 'message_delta',
        data: { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 1 } },
      },
    ];
    expect(messages.reduce(addEvent, emptyAssembled())).toMatchObject({
      text: 'Hi',
      finish: 'end_turn',
      usage: { input: 9, output: 1 },
    });
    const responses: StreamEvent[] = [
      { data: { type: 'response.output_text.delta', delta: 'Yo' } },
      {
        data: {
          type: 'response.completed',
          response: { status: 'completed', usage: { input_tokens: 4, output_tokens: 1 } },
        },
      },
    ];
    expect(responses.reduce(addEvent, emptyAssembled())).toMatchObject({ text: 'Yo', finish: 'completed' });
  });

  it('reads whole answers, embeddings and errors', () => {
    const text = () => 'answer';
    expect(
      assembleBody({ choices: [{ finish_reason: 'length' }], usage: { prompt_tokens: 1 } }, text),
    ).toMatchObject({
      text: 'answer',
      finish: 'length',
      usage: { input: 1 },
    });
    expect(assembleBody({ data: [{ embedding: [0.1, 0.2, 0.3] }] }, text).vectors).toEqual({
      count: 1,
      dimensions: 3,
      sample: [0.1, 0.2, 0.3],
    });
    expect(assembleBody({ error: { message: 'no such model' } }, text).error).toBe('no such model');
  });
});
