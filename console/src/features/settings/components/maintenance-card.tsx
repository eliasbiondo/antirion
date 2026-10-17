'use client';

import { useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { type Day, DatePicker, parseTime, TimeField } from '@/components/ui/date-picker';
import { Field, Input } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { MenuButton } from '@/components/ui/menu';
import { CancelButton, Dialog } from '@/components/ui/overlay';
import { EmptyState, ErrorState, InlineError } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { useEdited, useNow } from '@/lib/hooks';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { type MaintenanceWindow, useDanger } from '../danger-store';
import { Card } from './parts';

/** A date and a time on the UTC clock as RFC 3339, or null when either is missing. */
export function utcInstant(day: Day, time: string): string | null {
  const t = parseTime(time);
  const d = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (!t || !d) return null;
  const ms = Date.UTC(Number(d[1]), Number(d[2]) - 1, Number(d[3]), t.h, t.m);
  return new Date(ms).toISOString().replace('.000Z', 'Z');
}

function splitUtc(value: string | undefined): { day: Day; time: string } {
  if (!value) return { day: '', time: '' };
  const iso = new Date(value).toISOString();
  return { day: iso.slice(0, 10), time: iso.slice(11, 16) };
}

function windowState(w: MaintenanceWindow, now: number): 'now' | 'upcoming' | 'past' {
  const start = Date.parse(w.start);
  const end = Date.parse(w.end);
  if (end <= now) return 'past';
  return start <= now ? 'now' : 'upcoming';
}

function WindowDialog({
  open,
  initial,
  onClose,
  onSave,
}: {
  open: boolean;
  initial: MaintenanceWindow | null;
  onClose: () => void;
  onSave: (w: MaintenanceWindow) => Promise<void>;
}) {
  const start = splitUtc(initial?.start);
  const end = splitUtc(initial?.end);
  const [startDay, setStartDay] = useState(
    () => start.day || new Date(Date.now() + 86_400_000).toISOString().slice(0, 10),
  );
  const [startTime, setStartTime] = useState(start.time || '02:00');
  const [endDay, setEndDay] = useState(end.day);
  const [endTime, setEndTime] = useState(end.time || '04:00');
  const [description, setDescription] = useState(initial?.description ?? '');
  const [problem, setProblem] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const edited = useEdited({ startDay, startTime, endDay, endTime, description });

  async function submit() {
    const s = utcInstant(startDay, startTime);
    const e = utcInstant(endDay || startDay, endTime);
    if (!s || !e) {
      setProblem('Choose a start and an end, with times such as 02:00');
      return;
    }
    if (Date.parse(e) <= Date.parse(s)) {
      setProblem('End the window after it starts');
      return;
    }
    setProblem(null);
    setBusy(true);
    setError(null);
    try {
      await onSave({ start: s, end: e, ...(description.trim() ? { description: description.trim() } : {}) });
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      dirty={edited}
      open={open}
      onClose={onClose}
      title={initial ? 'Edit maintenance window' : 'Add maintenance window'}
      description="Times are on the UTC clock."
      footer={
        <>
          <CancelButton />
          <Button variant="primary" busy={busy} onClick={() => void submit()}>
            {initial ? 'Save window' : 'Add window'}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <InlineError error={error} />
        <div className="grid grid-cols-[minmax(0,1fr)_88px] gap-3">
          <Field label="Starts (UTC)" controlId="mw-start-day">
            <DatePicker id="mw-start-day" value={startDay} onChange={setStartDay} />
          </Field>
          <Field label="Time" controlId="mw-start-time">
            <TimeField id="mw-start-time" value={startTime} onChange={setStartTime} />
          </Field>
          <Field label="Ends (UTC)" controlId="mw-end-day" error={problem ?? undefined}>
            <DatePicker id="mw-end-day" value={endDay || startDay} onChange={setEndDay} />
          </Field>
          <Field label="Time" controlId="mw-end-time">
            <TimeField id="mw-end-time" value={endTime} onChange={setEndTime} />
          </Field>
        </div>
        <Field label="What's planned" optional>
          <Input
            value={description}
            maxLength={500}
            placeholder="Database upgrade"
            onChange={(e) => setDescription(e.target.value)}
          />
        </Field>
      </div>
    </Dialog>
  );
}

/** Maintenance windows: the current or next one from the status, and the list edited in governance. */
export function MaintenanceCard() {
  const governance = useDanger((s) => s.governance);
  const status = useDanger((s) => s.status);
  const saveGovernance = useDanger((s) => s.saveGovernance);
  const timeZone = usePreferences((s) => s.timeZone);
  const canEdit = useCan('patchGovernanceSettings');
  const [editing, setEditing] = useState<{ index: number | null } | null>(null);
  const [removing, setRemoving] = useState<number | null>(null);
  const windows = governance.data?.spec.maintenance_windows ?? [];
  const next = status.data?.maintenance_window ?? null;
  const now = useNow(60_000);
  const order = windows
    .map((w, i) => ({ w, i }))
    .sort((a, b) => Date.parse(a.w.start) - Date.parse(b.w.start));

  const save = (list: MaintenanceWindow[]) => saveGovernance({ maintenance_windows: list });

  return (
    <Card
      title="Maintenance windows"
      description="Planned work. Alert rules set to stay quiet during maintenance notify no one while a window is on."
      actions={
        canEdit ? (
          <Button size="sm" icon="plus" onClick={() => setEditing({ index: null })}>
            Add window
          </Button>
        ) : null
      }
    >
      {governance.error ? (
        <ErrorState error={governance.error} subject="Maintenance windows" size="sm" />
      ) : null}
      {next ? (
        <p className="mb-3 flex flex-wrap items-center gap-2 text-ui text-fg" role="status">
          <Badge tone={next.active ? 'warn' : 'info'} dot>
            {next.active ? 'On now' : 'Next'}
          </Badge>
          {formatDateTime(next.start, timeZone)} to {formatDateTime(next.end, timeZone)}
          {next.description ? <span className="text-muted">· {next.description}</span> : null}
          <span className="text-caption text-muted">
            ({next.active ? `ends ${formatRelative(next.end)}` : `starts ${formatRelative(next.start)}`})
          </span>
        </p>
      ) : governance.data ? (
        <EmptyState size="sm" title="No maintenance planned" />
      ) : null}
      {order.length > 0 ? (
        <Table label="Maintenance windows">
          <thead>
            <tr>
              <Th>Starts</Th>
              <Th>Ends</Th>
              <Th>What&apos;s planned</Th>
              <Th>
                <span className="sr-only">State and actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {order.map(({ w, i }) => {
              const state = windowState(w, now);
              return (
                <Tr key={`${w.start}-${i}`}>
                  <Td mono muted={state === 'past'}>
                    {formatDateTime(w.start, timeZone)}
                  </Td>
                  <Td mono muted={state === 'past'}>
                    {formatDateTime(w.end, timeZone)}
                  </Td>
                  <Td muted>{w.description ?? '—'}</Td>
                  <Td className="text-right">
                    <span className="inline-flex items-center gap-2">
                      {state === 'now' ? <Badge tone="warn">On now</Badge> : null}
                      {state === 'past' ? <Badge>Ended</Badge> : null}
                      {canEdit ? (
                        <MenuButton
                          iconOnly
                          variant="ghost"
                          size="sm"
                          label={`Actions for the window starting ${formatDateTime(w.start, timeZone)}`}
                          items={[
                            { label: 'Edit window', icon: 'edit', onSelect: () => setEditing({ index: i }) },
                            {
                              label: 'Remove window',
                              icon: 'trash',
                              danger: true,
                              onSelect: () => setRemoving(i),
                            },
                          ]}
                        />
                      ) : null}
                    </span>
                  </Td>
                </Tr>
              );
            })}
          </tbody>
        </Table>
      ) : null}
      {editing ? (
        <WindowDialog
          open
          initial={editing.index === null ? null : (windows[editing.index] ?? null)}
          onClose={() => setEditing(null)}
          onSave={async (w) => {
            const list =
              editing.index === null ? [...windows, w] : windows.map((x, i) => (i === editing.index ? w : x));
            await save(list);
            notify(
              'success',
              editing.index === null ? 'Maintenance window added' : 'Maintenance window saved',
            );
          }}
        />
      ) : null}
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        title="Remove this maintenance window?"
        tone="primary"
        confirmLabel="Remove window"
        effect="Alert rules that stay quiet during maintenance notify as usual at that time."
        onConfirm={async () => {
          await save(windows.filter((_, i) => i !== removing));
          notify('success', 'Maintenance window removed');
        }}
      />
    </Card>
  );
}
