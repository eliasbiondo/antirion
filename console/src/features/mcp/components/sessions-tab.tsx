'use client';

import { Badge } from '@/components/ui/badge';
import { Panel } from '@/components/ui/layout';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Table, Td, Th, Tr } from '@/components/ui/table';
import { formatCount } from '@/lib/format';

import { useMcp } from '../store';
import { SESSION_MODE_LABEL } from './gateways-tab';

/**
 * The Sessions tab: per gateway, the sessions the answering node issued and refused and the size of
 * its revocation list. Sessions are encrypted tokens the gateway doesn't store, so there is no
 * list of them to show.
 */
export function SessionsTab() {
  const sessions = useMcp((s) => s.sessions);
  const settings = useMcp((s) => s.settings.data);
  const loadSessions = useMcp((s) => s.loadSessions);
  const lifetime = settings?.spec.session?.max_lifetime ?? '24h';
  const rows = sessions.data ?? [];
  return (
    <div className="flex flex-col">
      <Panel
        flush
        title="Sessions"
        subtitle={
          <>
            Counts of the node that answered, since it started. A session lasts up to{' '}
            <span className="mono text-fg">{lifetime}</span>; session keys are rotated in the configuration
            file.
          </>
        }
      >
        {sessions.status === 'loading' && !sessions.data ? <LoadingRows rows={3} /> : null}
        {sessions.error ? (
          <ErrorState
            error={sessions.error}
            subject="MCP sessions"
            onRetry={() => void loadSessions()}
            size="sm"
          />
        ) : null}
        {sessions.data && rows.length === 0 ? (
          <EmptyState size="sm" title="No gateways, so no sessions" />
        ) : null}
        {rows.length > 0 ? (
          <Table label="MCP sessions by gateway">
            <thead>
              <tr>
                <Th>Gateway</Th>
                <Th className="max-sm:hidden">Sessions</Th>
                <Th numeric>Issued</Th>
                <Th numeric>Refused</Th>
                <Th numeric className="max-sm:hidden">
                  Ended early
                </Th>
                <Th className="max-md:hidden">Session keys</Th>
              </tr>
            </thead>
            <tbody>
              {rows.map((s) => (
                <Tr key={s.gateway}>
                  <Td mono>{s.gateway}</Td>
                  <Td muted className="max-sm:hidden">
                    {SESSION_MODE_LABEL[s.session_mode]}
                  </Td>
                  <Td numeric>{formatCount(s.issued)}</Td>
                  <Td numeric className={s.rejected > 0 ? 'text-warn' : undefined}>
                    {formatCount(s.rejected)}
                  </Td>
                  <Td numeric className="max-sm:hidden">
                    {formatCount(s.revoked)}
                  </Td>
                  <Td className="max-md:hidden">
                    <span className="flex flex-wrap gap-1">
                      {s.key_ids.map((k, i) => (
                        <Badge key={k} mono tone={i === 0 ? 'accent' : 'neutral'}>
                          {i === 0 ? `${k} · signs new` : k}
                        </Badge>
                      ))}
                    </span>
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        ) : null}
      </Panel>
    </div>
  );
}
