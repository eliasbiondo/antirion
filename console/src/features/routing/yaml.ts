/** A small YAML writer for the routing export: block style, quoted strings only when needed. */

const PLAIN = /^[A-Za-z0-9_./@-][A-Za-z0-9 _./@:-]*$/;
const RESERVED =
  /^(true|false|yes|no|on|off|null|~|-?\d+(\.\d+)?([eE][+-]?\d+)?|0x[0-9a-fA-F]+|\.inf|\.nan)$/i;

function scalar(value: unknown): string {
  if (value === null || value === undefined) return 'null';
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  if (text !== '' && PLAIN.test(text) && !RESERVED.test(text) && !text.endsWith(' ') && !text.includes(': '))
    return text;
  return JSON.stringify(text);
}

function key(name: string): string {
  return PLAIN.test(name) && !RESERVED.test(name) && !name.includes(': ') ? name : JSON.stringify(name);
}

function isMap(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function empty(value: unknown): string | null {
  if (Array.isArray(value) && value.length === 0) return '[]';
  if (isMap(value) && Object.keys(value).length === 0) return '{}';
  return null;
}

function lines(value: unknown, indent: string): string[] {
  if (Array.isArray(value)) {
    return value.flatMap((item) => {
      const e = empty(item);
      if (e !== null) return [`${indent}- ${e}`];
      if (isMap(item) || Array.isArray(item)) {
        const inner = lines(item, `${indent}  `);
        const [first = '', ...rest] = inner;
        return [`${indent}- ${first.slice(indent.length + 2)}`, ...rest];
      }
      return [`${indent}- ${scalar(item)}`];
    });
  }
  if (isMap(value)) {
    return Object.entries(value).flatMap(([k, v]) => {
      if (v === undefined) return [];
      const e = empty(v);
      if (e !== null) return [`${indent}${key(k)}: ${e}`];
      if (isMap(v) || Array.isArray(v)) return [`${indent}${key(k)}:`, ...lines(v, `${indent}  `)];
      return [`${indent}${key(k)}: ${scalar(v)}`];
    });
  }
  return [`${indent}${scalar(value)}`];
}

/** The value as a YAML document. */
export function toYaml(value: unknown): string {
  return `${lines(value, '').join('\n')}\n`;
}
