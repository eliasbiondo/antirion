'use client';

import { useRef, useState } from 'react';

import { Button, IconButton } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/form';
import { Popover } from '@/components/ui/overlay';
import { cn } from '@/lib/cn';

import { columnOf, isStandard, LOG_COLUMNS, moveColumn, toggleColumn } from '../columns';

/**
 * Which columns the table shows and in what order: a checkbox per column and arrows to move a
 * shown one. The choice is the user's own and follows them (preferences); views keep one too.
 */
export function ColumnChooser({
  columns,
  onChange,
}: {
  columns: string[];
  /** The new choice; null for the standard set. */
  onChange: (columns: string[] | null) => void;
}) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const hidden = LOG_COLUMNS.filter((c) => !columns.includes(c.id));
  const set = (next: string[]) => onChange(isStandard(next) ? null : next);
  return (
    <>
      <Button
        ref={anchor}
        icon="columns"
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={() => setOpen(!open)}
      >
        <span className="max-md:sr-only">Columns</span>
      </Button>
      <Popover
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        align="right"
        label="Columns"
        className="w-[280px]"
      >
        <div className="flex items-center justify-between border-b border-line px-3 py-2">
          <span className="text-ui font-semibold text-fg">Columns</span>
          <button
            type="button"
            disabled={isStandard(columns)}
            onClick={() => onChange(null)}
            className="rounded-xs text-caption text-muted hover:text-fg disabled:invisible"
          >
            Reset
          </button>
        </div>
        <ul aria-label="Shown columns" className="flex flex-col p-1">
          {columns.map((id, i) => {
            const c = columnOf(id);
            if (!c) return null;
            return (
              <li key={id} className="flex min-h-8 items-center gap-1 rounded-sm px-2 py-1 hover:bg-hover">
                <Checkbox
                  className="min-w-0 flex-1"
                  checked
                  disabled={c.fixed}
                  onChange={() => set(toggleColumn(columns, id))}
                  label={
                    <span className="flex min-w-0 flex-col">
                      <span className="truncate">{c.label}</span>
                      {c.note ? <span className="text-caption text-dim">{c.note}</span> : null}
                    </span>
                  }
                />
                <IconButton
                  icon="arrowUp"
                  size={14}
                  label={`Move ${c.label} up`}
                  disabled={i === 0}
                  className={cn(i === 0 && 'invisible')}
                  onClick={() => set(moveColumn(columns, id, -1))}
                />
                <IconButton
                  icon="arrowDown"
                  size={14}
                  label={`Move ${c.label} down`}
                  disabled={i === columns.length - 1}
                  className={cn(i === columns.length - 1 && 'invisible')}
                  onClick={() => set(moveColumn(columns, id, 1))}
                />
              </li>
            );
          })}
        </ul>
        {hidden.length > 0 ? (
          <>
            <div className="border-t border-line px-3 pt-2 pb-1 text-caption text-muted">Hidden</div>
            <ul aria-label="Hidden columns" className="flex flex-col p-1 pt-0">
              {hidden.map((c) => (
                <li key={c.id} className="flex h-8 items-center rounded-sm px-2 hover:bg-hover">
                  <Checkbox
                    className="min-w-0 flex-1"
                    checked={false}
                    onChange={() => set(toggleColumn(columns, c.id))}
                    label={<span className="truncate">{c.label}</span>}
                  />
                </li>
              ))}
            </ul>
          </>
        ) : null}
      </Popover>
    </>
  );
}
