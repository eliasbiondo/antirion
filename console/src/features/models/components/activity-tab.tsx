'use client';

import { useEffect, useState } from 'react';

import { Dot } from '@/components/ui/badge';
import { Segmented } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Icon, type IconName } from '@/components/ui/icon';
import { Section } from '@/components/ui/layout';
import { EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { Tooltip } from '@/components/ui/tooltip';
import type { Alert } from '@/lib/api/types';
import { formatDateTime, formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';

import { type ActivityItem, type ActivityKind, activityFeed } from '../activity';
import { modelsApi } from '../api';
import { useCatalog } from '../store';

const KIND_ICON: Record<ActivityKind, IconName> = { change: 'edit', access: 'lock', alert: 'alert' };

/** A feed of activity lines: what happened, who, and when (relative, the exact time on hover). */
export function ActivityList({
  items,
  onOpenModel,
}: {
  items: ActivityItem[];
  onOpenModel?: ((name: string) => void) | undefined;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  return (
    <ol className="flex flex-col">
      {items.map((item) => (
        <li key={item.id} className="flex items-start gap-3 border-b border-line py-2 text-ui last:border-0">
          <span className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center text-dim">
            <Icon name={KIND_ICON[item.kind]} size={14} />
          </span>
          <Dot tone={item.tone} className="mt-1.5" />
          <span className="min-w-0 flex-1">
            {onOpenModel && item.model ? (
              <button
                type="button"
                className="text-left text-fg hover:text-accent"
                onClick={() => {
                  if (item.model) onOpenModel(item.model);
                }}
              >
                {item.text}
              </button>
            ) : (
              <span className="text-fg">{item.text}</span>
            )}
            {item.actor ? <span className="text-muted"> · by {item.actor}</span> : null}
          </span>
          <Tooltip content={formatDateTime(item.time, timeZone)}>
            <span className="shrink-0 text-caption text-dim">{formatRelative(item.time)}</span>
          </Tooltip>
        </li>
      ))}
    </ol>
  );
}

/** Alert instances of the last 30 days that are scoped to a Model, or [] without access. */
function useModelAlerts(enabled: boolean): Alert[] | null {
  const [alerts, setAlerts] = useState<Alert[] | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    modelsApi.alerts(new Date(Date.now() - 30 * 86_400_000).toISOString()).then(
      (all) => live && setAlerts(all.filter((a) => Boolean(a.scope?.model))),
      () => live && setAlerts([]),
    );
    return () => {
      live = false;
    };
  }, [enabled]);
  return enabled ? alerts : [];
}

/** The catalog's recent activity: changes to Models, routes, deployments and defaults, access decisions, Model alerts. */
export function ActivityTab({ onOpenModel }: { onOpenModel?: (name: string) => void }) {
  const canAudit = useCan('listAudit');
  const canAlerts = useCan('listAlerts');
  const audit = useCatalog((s) => s.catalogAudit);
  const loadCatalogAudit = useCatalog((s) => s.loadCatalogAudit);
  const alerts = useModelAlerts(canAlerts);
  const [kind, setKind] = useState<ActivityKind | ''>('');
  useEffect(() => {
    if (canAudit) void loadCatalogAudit();
  }, [canAudit, loadCatalogAudit]);
  const items = activityFeed(audit.data ?? [], alerts ?? [], 80).filter((i) => !kind || i.kind === kind);
  return (
    <div className="flex flex-col">
      <Section
        title="Recent catalog activity"
        subtitle="changes to models, routes and defaults, access decisions and model alerts"
        actions={
          <Segmented<ActivityKind | ''>
            label="Show"
            value={kind}
            onChange={setKind}
            options={[
              { value: '', label: 'All' },
              { value: 'change', label: 'Changes' },
              { value: 'access', label: 'Access' },
              { value: 'alert', label: 'Alerts' },
            ]}
          />
        }
      >
        {!canAudit ? (
          <p className="text-ui text-muted">You need access to the audit log to see changes.</p>
        ) : null}
        {audit.status === 'loading' && !audit.data ? <LoadingRows rows={5} /> : null}
        {audit.error ? <ErrorState error={audit.error} subject="Catalog activity" size="sm" /> : null}
        {audit.data && items.length === 0 ? (
          <EmptyState size="sm" title="Nothing yet" description="Changes and access decisions show here." />
        ) : null}
        <ActivityList items={items} onOpenModel={onOpenModel} />
      </Section>
    </div>
  );
}
