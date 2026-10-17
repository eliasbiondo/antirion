type ClassValue = string | false | null | undefined | 0;

/** Joins class names, skipping falsy entries. */
export function cn(...values: ClassValue[]): string {
  return values.filter(Boolean).join(' ');
}
