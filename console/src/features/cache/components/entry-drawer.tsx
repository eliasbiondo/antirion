'use client';

import { useEffect, useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Gate, usePermission } from '@/components/ui/gate';
import { DefinitionGrid } from '@/components/ui/layout';
import { CopyButton, CopyLinkButton } from '@/components/ui/misc';
import { Drawer } from '@/components/ui/overlay';
import { ErrorState, LoadingRows } from '@/components/ui/states';
import type { CacheEntry } from '@/lib/api/types';
import { formatBytes, formatCount, formatMoney, formatTokens, shortId } from '@/lib/format';
import { formatClock, formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { notify } from '@/stores/ui';

import { cacheApi } from '../api';
import { useCache } from '../store';

/** "in 14 min" for a time ahead, "expired" for one past. */
export function expiresIn(at: string, now: Date = new Date()): string {
  if (new Date(at).getTime() <= now.getTime()) return 'Expired';
  return `in ${formatRelative(at, now).replace(/ ago$/, '').replace(/^in /, '')}`;
}

/**
 * One cached response: what it answers and for whom, its cache key, the stored response for readers
 * with content access, and the actions on it (remove; for a similar match, report it as wrong).
 */
export function EntryDrawer({
  entry,
  onClose,
  startWith,
}: {
  entry: CacheEntry;
  onClose: () => void;
  /** Opens straight on the confirmation of an action (a row's "Report wrong match"). */
  startWith?: 'remove' | 'report' | undefined;
}) {
  const canContent = usePermission('logs:read_content');
  const timeZone = usePreferences((s) => s.timeZone);
  const deleteEntry = useCache((s) => s.deleteEntry);
  const report = useCache((s) => s.reportFalsePositive);
  const [detail, setDetail] = useState<CacheEntry | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [confirm, setConfirm] = useState<'remove' | 'report' | null>(startWith ?? null);

  useEffect(() => {
    let live = true;
    cacheApi.entry(entry.key).then(
      (d) => live && setDetail(d),
      (e: unknown) => live && setError(e),
    );
    return () => {
      live = false;
    };
  }, [entry.key]);

  const e = detail ?? entry;
  const semantic = e.mode === 'semantic';
  const response = detail?.response;
  return (
    <>
      <Drawer
        open
        onClose={onClose}
        width="lg"
        title="Cached response"
        subtitle={`${e.model} · ${shortId(e.key, 16)}`}
        headerActions={<CopyLinkButton />}
        footer={
          <>
            <Button variant="ghost" onClick={onClose}>
              Close
            </Button>
            {semantic ? (
              <Gate operation="reportCacheFalsePositive" mode="hide">
                <Button variant="secondary" icon="alert" onClick={() => setConfirm('report')}>
                  Report wrong match
                </Button>
              </Gate>
            ) : null}
            <Gate operation="deleteCacheEntry" mode="hide">
              <Button variant="danger" icon="trash" onClick={() => setConfirm('remove')}>
                Remove response
              </Button>
            </Gate>
          </>
        }
      >
        <div className="flex flex-col gap-5">
          <section aria-labelledby="entry-summary" className="flex flex-col gap-2">
            <h3 id="entry-summary" className="text-ui font-semibold text-fg">
              Summary
            </h3>
            <DefinitionGrid
              columns={3}
              items={[
                {
                  label: 'Match',
                  value: <Badge tone={semantic ? 'info' : 'neutral'}>{semantic ? 'Similar' : 'Exact'}</Badge>,
                },
                { label: 'Model', value: e.model },
                { label: 'Deployment', value: e.deployment ?? '—' },
                { label: 'Route', value: e.route ?? 'None' },
                { label: 'Shared per', value: (e.scope ?? '—').replace('_', ' ') },
                {
                  label: 'Size',
                  value: `${formatBytes(e.size_bytes)}${
                    e.input_tokens !== undefined
                      ? ` · ${formatTokens(e.input_tokens)} in, ${formatTokens(e.output_tokens ?? 0)} out`
                      : ''
                  }`,
                },
                {
                  label: 'Hits',
                  value: `${formatCount(e.hits)}${e.last_hit_at ? ` · last ${formatRelative(e.last_hit_at)}` : ''}`,
                },
                { label: 'Saved', value: e.saved_usd !== undefined ? formatMoney(e.saved_usd) : '—' },
                {
                  label: 'Similarity needed',
                  value: e.similarity_threshold != null ? e.similarity_threshold.toFixed(2) : '—',
                },
                { label: 'Cached', value: formatDateTime(e.created_at, timeZone) },
                {
                  label: 'Expires',
                  value: `${expiresIn(e.expires_at)}, at ${formatClock(e.expires_at, timeZone)}`,
                },
              ]}
            />
          </section>
          <section aria-labelledby="entry-key" className="flex flex-col gap-2">
            <div className="flex items-center gap-2">
              <h3 id="entry-key" className="text-ui font-semibold text-fg">
                Cache key
              </h3>
              <span className="ml-auto">
                <CopyButton value={e.key} label="Copy key" />
              </span>
            </div>
            <code className="mono block rounded-control border border-line bg-canvas px-3 py-2 text-caption break-all text-fg">
              {e.key}
            </code>
            <p className="text-caption text-muted">
              Apps receive this key with every response, so a support ticket can name the exact cached answer.
            </p>
          </section>
          <section aria-labelledby="entry-response" className="flex flex-col gap-2">
            <h3 id="entry-response" className="text-ui font-semibold text-fg">
              Response
            </h3>
            {error ? <ErrorState error={error} subject="This response" size="sm" /> : null}
            {!error && !detail ? <LoadingRows rows={3} /> : null}
            {detail && !canContent ? (
              <p className="text-ui text-muted">
                Only people who can read request content see the stored response.
              </p>
            ) : null}
            {detail && canContent && (response === null || response === undefined) ? (
              <p className="text-ui text-muted">The gateway didn’t return the stored response.</p>
            ) : null}
            {detail && canContent && response !== null && response !== undefined ? (
              <pre className="mono max-h-96 overflow-auto rounded-control border border-line bg-canvas px-3 py-2 text-caption whitespace-pre-wrap text-fg">
                {typeof response === 'string' ? response : JSON.stringify(response, null, 2)}
              </pre>
            ) : null}
          </section>
        </div>
      </Drawer>
      <ConfirmDialog
        open={confirm === 'remove'}
        onClose={() => setConfirm(null)}
        title="Remove this cached response?"
        objectName={shortId(e.key, 16)}
        confirmLabel="Remove response"
        effect="The next identical request goes to the provider and is cached again."
        onConfirm={async (reason) => {
          await deleteEntry(e.key, reason);
          notify('success', 'Response removed', e.model);
          onClose();
        }}
      />
      <ConfirmDialog
        open={confirm === 'report'}
        onClose={() => setConfirm(null)}
        withReason={false}
        title="Report this as a wrong match?"
        objectName={shortId(e.key, 16)}
        confirmLabel="Report wrong match"
        effect="The response is removed and counts toward this model’s false-positive rate, which tells you whether its similarity threshold is too loose."
        onConfirm={async () => {
          await report(e.key);
          notify('success', 'Wrong match reported', 'The response was removed.');
          onClose();
        }}
      />
    </>
  );
}
