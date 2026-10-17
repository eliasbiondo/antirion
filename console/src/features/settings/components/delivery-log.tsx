'use client';

import { useCallback, useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { permissionOf, useCan } from '@/components/ui/gate';
import { noPermissionReason } from '@/lib/permissions';
import { Drawer } from '@/components/ui/overlay';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { eventWords } from '@/features/alerts/events';
import { describeError } from '@/lib/api/client';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { type AlertDelivery, settingsApi } from '../api';

function resultTone(d: AlertDelivery): 'success' | 'warn' | 'error' {
  if (d.result === 'delivered') return 'success';
  if (d.result === 'timeout') return 'warn';
  return 'error';
}

function resultWords(d: AlertDelivery): string {
  if (d.result === 'delivered') return d.status_code ? `Delivered (${d.status_code})` : 'Delivered';
  if (d.result === 'timeout') return 'Timed out';
  return d.status_code ? `Failed (${d.status_code})` : 'Failed';
}

/**
 * Every delivery attempt to one webhook or channel over the last 7 days, newest first, with a
 * redelivery of an event that failed.
 */
export function DeliveryLog({
  target,
  kind,
  onClose,
}: {
  target: string;
  kind: 'webhook' | 'channel';
  onClose: () => void;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const canRedeliver = useCan('redeliverAlert');
  const [items, setItems] = useState<AlertDelivery[] | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const load = useCallback(
    (after?: string) => {
      settingsApi.deliveries(target, after).then((page) => {
        setItems((prev) => (after ? [...(prev ?? []), ...page.items] : page.items));
        setCursor(page.next_cursor ?? null);
        setError(null);
      }, setError);
    },
    [target],
  );
  useEffect(() => load(), [load]);
  return (
    <Drawer
      open
      onClose={onClose}
      width="lg"
      title={`Deliveries to ${target}`}
      subtitle={`Every attempt to this ${kind} in the last 7 days, newest first. A redelivery sends the same event again with its id.`}
      headerActions={
        <Button size="sm" variant="ghost" icon="refresh" onClick={() => load()}>
          Refresh
        </Button>
      }
    >
      {error ? <ErrorState error={error} subject="Deliveries" size="sm" onRetry={() => load()} /> : null}
      {!items && !error ? <LoadingRows rows={5} /> : null}
      {items?.length === 0 ? (
        <EmptyState
          size="sm"
          title="No deliveries in the last 7 days"
          description="Send a test to see one."
        />
      ) : null}
      {items && items.length > 0 ? (
        <div className="flex flex-col gap-3">
          <Table label="Deliveries">
            <thead>
              <tr>
                <Th>When</Th>
                <Th>Event</Th>
                <Th numeric>Attempt</Th>
                <Th>Result</Th>
                <Th>
                  <span className="sr-only">Actions</span>
                </Th>
              </tr>
            </thead>
            <tbody>
              {items.map((d) => (
                <Tr key={`${d.event_id}-${d.attempt}-${d.time}`}>
                  <Td muted title={formatDateTime(d.time, timeZone)}>
                    {formatRelative(d.time)}
                  </Td>
                  <Td>
                    {d.event_type ? eventWords(d.event_type) : 'Event'}
                    <span className="mono block text-caption text-dim">{d.event_id.slice(0, 18)}…</span>
                  </Td>
                  <Td numeric>{d.attempt}</Td>
                  <Td>
                    <Badge tone={resultTone(d)}>{resultWords(d)}</Badge>
                  </Td>
                  <Td>
                    {d.result === 'delivered' ? null : (
                      <Button
                        size="sm"
                        variant="ghost"
                        icon="send"
                        busy={busy === d.event_id}
                        disabled={!canRedeliver}
                        disabledReason={
                          canRedeliver ? undefined : noPermissionReason(permissionOf('redeliverAlert'))
                        }
                        onClick={() => {
                          setBusy(d.event_id);
                          settingsApi
                            .redeliver(d.event_id)
                            .then(
                              () => {
                                notify('success', 'Redelivery queued', d.event_id);
                                load();
                              },
                              (e: unknown) => notify('error', "Couldn't redeliver", describeError(e)),
                            )
                            .finally(() => setBusy(null));
                        }}
                      >
                        Redeliver
                      </Button>
                    )}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
          {cursor ? (
            <div>
              <Button size="sm" onClick={() => load(cursor)}>
                Show more
              </Button>
            </div>
          ) : null}
        </div>
      ) : null}
    </Drawer>
  );
}
