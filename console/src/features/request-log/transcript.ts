/**
 * A logged conversation in plain terms, whatever the dialect it was sent in: chat completions,
 * messages with content blocks, responses with input items, or contents with parts.
 */

export interface ToolCall {
  id: string | null;
  name: string;
  /** The arguments as sent: JSON text, pretty-printed when it parses. */
  arguments: string;
}

export interface TranscriptMessage {
  /** `system`, `user`, `assistant`, `tool`, or the dialect's own role. */
  role: string;
  text: string;
  toolCalls: ToolCall[];
  /** For a tool result: the call it answers. */
  toolCallId: string | null;
  /** Images, audio and files are named, not shown. */
  attachments: string[];
  /** Whether the message is the response (the assistant's answer to this request). */
  response: boolean;
}

export interface ResponseView {
  messages: TranscriptMessage[];
  finishReasons: string[];
  /** The error envelope's message and type, when the answer was an error. */
  error: { message: string; type: string | null } | null;
}

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function str(v: unknown): string | null {
  return typeof v === 'string' ? v : null;
}

function pretty(value: unknown): string {
  if (typeof value === 'string') {
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }
  return JSON.stringify(value ?? null, null, 2);
}

function message(role: string, response: boolean, init: Partial<TranscriptMessage> = {}): TranscriptMessage {
  return { role, text: '', toolCalls: [], toolCallId: null, attachments: [], response, ...init };
}

/** Text and attachments of a content value: a string or a list of parts or blocks. */
function content(value: unknown): { text: string; attachments: string[]; calls: ToolCall[] } {
  if (typeof value === 'string') return { text: value, attachments: [], calls: [] };
  const texts: string[] = [];
  const attachments: string[] = [];
  const calls: ToolCall[] = [];
  for (const part of Array.isArray(value) ? value : []) {
    if (typeof part === 'string') {
      texts.push(part);
      continue;
    }
    if (!isObject(part)) continue;
    const type = str(part.type) ?? '';
    const text = str(part.text);
    if (text !== null && (type === '' || type.endsWith('text'))) texts.push(text);
    else if (type === 'tool_use')
      calls.push({ id: str(part.id), name: str(part.name) ?? 'tool', arguments: pretty(part.input) });
    else if (type === 'tool_result') {
      const inner = content(part.content);
      texts.push(inner.text);
    } else if (type === 'refusal' && str(part.refusal)) texts.push(str(part.refusal) ?? '');
    else if (type === 'thinking' || type === 'reasoning') continue;
    else if (isObject(part.functionCall))
      calls.push({
        id: null,
        name: str(part.functionCall.name) ?? 'tool',
        arguments: pretty(part.functionCall.args),
      });
    else if (isObject(part.functionResponse)) texts.push(pretty(part.functionResponse.response));
    else if (text !== null) texts.push(text);
    else if (type || Object.keys(part).length)
      attachments.push(type ? type.replace(/_/g, ' ') : 'attachment');
  }
  return { text: texts.join('\n\n'), attachments, calls };
}

function chatToolCalls(value: unknown): ToolCall[] {
  return (Array.isArray(value) ? value : []).filter(isObject).map((c) => {
    const fn = isObject(c.function) ? c.function : c;
    return { id: str(c.id), name: str(fn.name) ?? 'tool', arguments: pretty(fn.arguments ?? fn.input) };
  });
}

/** One chat or messages-style message (`{role, content, tool_calls}`), Anthropic blocks included. */
function fromMessage(m: Json, response: boolean): TranscriptMessage[] {
  const role = str(m.role) ?? 'user';
  const blocks = Array.isArray(m.content) ? m.content : [];
  const results = blocks.filter((b) => isObject(b) && b.type === 'tool_result') as Json[];
  const others = blocks.filter((b) => !(isObject(b) && b.type === 'tool_result'));
  const out: TranscriptMessage[] = results.map((b) =>
    message('tool', response, { text: content(b.content).text, toolCallId: str(b.tool_use_id) }),
  );
  const c = content(Array.isArray(m.content) ? others : m.content);
  const calls = [...chatToolCalls(m.tool_calls), ...c.calls];
  if (c.text || calls.length || c.attachments.length || results.length === 0)
    out.push(
      message(role === 'model' ? 'assistant' : role, response, {
        text: c.text || str(m.refusal) || '',
        toolCalls: calls,
        toolCallId: str(m.tool_call_id),
        attachments: c.attachments,
      }),
    );
  return out;
}

/** Responses-style input or output items. */
function fromItems(items: unknown[], response: boolean): TranscriptMessage[] {
  const out: TranscriptMessage[] = [];
  for (const item of items) {
    if (typeof item === 'string') {
      out.push(message('user', response, { text: item }));
      continue;
    }
    if (!isObject(item)) continue;
    const type = str(item.type);
    if (type === 'function_call' || type === 'custom_tool_call') {
      const call = {
        id: str(item.call_id) ?? str(item.id),
        name: str(item.name) ?? 'tool',
        arguments: pretty(item.arguments ?? item.input),
      };
      const last = out.at(-1);
      if (last?.role === 'assistant' && last.response === response) last.toolCalls.push(call);
      else out.push(message('assistant', response, { toolCalls: [call] }));
    } else if (type === 'function_call_output' || type === 'custom_tool_call_output')
      out.push(message('tool', response, { text: pretty(item.output), toolCallId: str(item.call_id) }));
    else if (type === 'reasoning') continue;
    else if (item.role !== undefined || type === 'message') out.push(...fromMessage(item, response));
  }
  return out;
}

/** Gemini-style `contents` with `parts`. */
function fromContents(contents: unknown[], response: boolean): TranscriptMessage[] {
  return contents.filter(isObject).flatMap((c) => {
    const parts = Array.isArray(c.parts) ? c.parts : [];
    const results = parts.filter((p) => isObject(p) && isObject(p.functionResponse)) as Json[];
    const out = results.map((p) =>
      message('tool', response, {
        text: pretty((p.functionResponse as Json).response),
        toolCallId: str((p.functionResponse as Json).name),
      }),
    );
    const rest = content(parts.filter((p) => !(isObject(p) && isObject(p.functionResponse))));
    if (rest.text || rest.calls.length || rest.attachments.length)
      out.push(
        message(c.role === 'model' ? 'assistant' : (str(c.role) ?? 'user'), response, {
          text: rest.text,
          toolCalls: rest.calls,
          attachments: rest.attachments,
        }),
      );
    return out;
  });
}

/** The messages a request body sent, in order, with its system instructions first. */
export function requestMessages(body: unknown): TranscriptMessage[] {
  if (!isObject(body))
    return typeof body === 'string' && body ? [message('user', false, { text: body })] : [];
  const out: TranscriptMessage[] = [];
  const system = body.system ?? body.instructions ?? body.systemInstruction ?? body.system_instruction;
  if (system !== undefined && system !== null) {
    const text = isObject(system) ? content(system.parts).text : content(system).text;
    if (text) out.push(message('system', false, { text }));
  }
  if (Array.isArray(body.messages))
    out.push(...body.messages.filter(isObject).flatMap((m) => fromMessage(m, false)));
  else if (Array.isArray(body.contents)) out.push(...fromContents(body.contents, false));
  else if (Array.isArray(body.input)) out.push(...fromItems(body.input, false));
  else if (typeof body.input === 'string') out.push(message('user', false, { text: body.input }));
  else if (typeof body.prompt === 'string') out.push(message('user', false, { text: body.prompt }));
  return out;
}

/** The answer a response body holds, with its finish reasons, or its error. */
export function responseView(body: unknown): ResponseView {
  const view: ResponseView = { messages: [], finishReasons: [], error: null };
  if (!isObject(body)) {
    if (typeof body === 'string' && body) view.messages.push(message('assistant', true, { text: body }));
    return view;
  }
  const error = isObject(body.error) ? body.error : null;
  if (error) {
    view.error = { message: str(error.message) ?? pretty(error), type: str(error.type) ?? str(error.code) };
    return view;
  }
  const finish = (v: unknown) => {
    if (typeof v === 'string' && !view.finishReasons.includes(v)) view.finishReasons.push(v);
  };
  if (Array.isArray(body.choices)) {
    for (const choice of body.choices.filter(isObject)) {
      if (isObject(choice.message)) view.messages.push(...fromMessage(choice.message, true));
      else if (typeof choice.text === 'string')
        view.messages.push(message('assistant', true, { text: choice.text }));
      finish(choice.finish_reason);
    }
  } else if (Array.isArray(body.output)) {
    view.messages.push(...fromItems(body.output, true));
    finish(body.status);
  } else if (Array.isArray(body.candidates)) {
    for (const c of body.candidates.filter(isObject)) {
      if (isObject(c.content)) view.messages.push(...fromContents([c.content], true));
      finish(c.finishReason);
    }
  } else if (Array.isArray(body.content) || body.role !== undefined) {
    view.messages.push(...fromMessage({ ...body, role: str(body.role) ?? 'assistant' }, true));
    finish(body.stop_reason);
  } else if (isObject(body.output) && isObject(body.output.message)) {
    view.messages.push(...fromMessage(body.output.message, true));
    finish(body.stopReason);
  }
  return view;
}

/** The logged body of a content part (`content.request` or `content.response`), when stored. */
export function storedBody(
  part: unknown,
): { body: unknown; truncated: boolean; bytes: number | null } | null {
  if (!isObject(part) || !('body' in part)) return null;
  return {
    body: part.body,
    truncated: part.truncated === true,
    bytes: typeof part.original_bytes === 'number' ? part.original_bytes : null,
  };
}
