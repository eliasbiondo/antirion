'use client';

import { Badge } from '@/components/ui/badge';

/** One change of a diff: what it was, what it becomes. */
export function ChangeRow({
  change,
}: {
  change: { op: string; path: string; before: unknown; after: unknown };
}) {
  const tone = change.op === 'create' ? 'success' : change.op === 'delete' ? 'error' : 'warn';
  return (
    <li className="border-b border-line py-2 last:border-0">
      <div className="flex items-center gap-2 text-ui">
        <Badge tone={tone} caps>
          {change.op}
        </Badge>
        <span className="mono text-fg">{change.path}</span>
      </div>
      <div className="mt-1 grid grid-cols-2 gap-2">
        <pre
          className="mono max-h-40 overflow-auto rounded-sm bg-[color-mix(in_oklab,var(--ant-error)_7%,transparent)] p-2 text-caption whitespace-pre-wrap text-fg"
          aria-label="Before"
        >
          {change.before === null ? '—' : JSON.stringify(change.before, null, 2)}
        </pre>
        <pre
          className="mono max-h-40 overflow-auto rounded-sm bg-[color-mix(in_oklab,var(--ant-success)_7%,transparent)] p-2 text-caption whitespace-pre-wrap text-fg"
          aria-label="After"
        >
          {change.after === null ? '—' : JSON.stringify(change.after, null, 2)}
        </pre>
      </div>
    </li>
  );
}
