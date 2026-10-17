/**
 * Only same-site console paths are followed after sign-in: one `/` then anything but `/` or `\`,
 * with no backslash, control character or whitespace anywhere (a browser drops tabs and newlines
 * from a URL, so `/\t/host` would become `//host`).
 */
export function safeNext(value: string | null): string {
  if (!value || !/^\/(?![/\\])/.test(value)) return '/';
  if (/[\\\u0000-\u001f\u007f\s]/.test(value)) return '/';
  return value;
}
