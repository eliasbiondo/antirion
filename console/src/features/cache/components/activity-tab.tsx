'use client';

import { useMemo } from 'react';

import { Badge } from '@/components/ui/badge';
import { Icon, type IconName } from '@/components/ui/icon';
import { Tooltip } from '@/components/ui/tooltip';
import { EmptyState, ErrorState, isUnavailable, LoadingRows, UnavailableState } from '@/components/ui/states';
import { cn } from '@/lib/cn';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { toActivity, type ActivityKind } from '../activity';
import { useCache } from '../store';

const ICONS: Record<ActivityKind, { icon: IconName; tone: string }> = {
  clear: { icon: 'trash', tone: 'text-warn' },
  remove: { icon: 'x', tone: 'text-muted' },
  report: { icon: 'alert', tone: 'text-info' },
  rule: { icon: 'refresh', tone: 'text-accent' },
  config: { icon: 'settings', tone: 'text-muted' },
};

/** Recent cache activity: clears, removals, wrong-match reports, rule firings and settings changes. */
export function ActivityTab() {
  const activity = useCache((s) => s.activity);
  const timeZone = usePreferences((s) => s.timeZone);
  const items = useMemo(() => toActivity(activity.data ?? []).slice(0, 100), [activity.data]);

  if (activity.status === 'loading' && !activity.data) return <LoadingRows rows={6} />;
  if (activity.error)
    return isUnavailable(activity.error) ? (
      <UnavailableState subject="Cache activity" />
    ) : (
      <ErrorState error={activity.error} subject="Cache activity" />
    );
  return (
    <section aria-labelledby="cache-activity-title">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-line bg-raised px-gutter pt-4 pb-3 max-sm:px-gutter-sm">
        <h2 id="cache-activity-title" className="text-title font-semibold text-fg">
          Activity
        </h2>
        <span className="text-ui text-muted">Clears, rule firings and settings changes, newest first.</span>
      </div>
      {items.length === 0 ? (
        <EmptyState
          title="No cache activity yet"
          description="Clears, rules firing and setting changes show here."
        />
      ) : (
        <ol className="flex flex-col border-b border-line bg-raised" aria-label="Cache activity">
          {items.map((a) => {
            const look = ICONS[a.kind];
            return (
              <li
                key={a.id}
                className="flex items-start gap-3 border-b border-line px-gutter py-2.5 last:border-0 max-sm:px-gutter-sm"
              >
                <span className={cn('mt-0.5 shrink-0', look.tone)}>
                  <Icon name={look.icon} size={16} />
                </span>
                <div className="min-w-0 flex-1">
                  <div className="text-ui text-fg">
                    {a.text}
                    {a.failed ? (
                      <Badge tone="error" className="ml-2">
                        Failed
                      </Badge>
                    ) : null}
                  </div>
                  <div className="text-caption text-muted">{a.actor}</div>
                </div>
                <Tooltip content={formatDateTime(a.time, timeZone)}>
                  <time dateTime={a.time} className="shrink-0 text-caption text-muted">
                    {formatRelative(a.time)}
                  </time>
                </Tooltip>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}
