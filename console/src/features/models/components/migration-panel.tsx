'use client';

import { useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { useCan } from '@/components/ui/gate';
import { DetailSection } from '@/components/ui/list-detail';
import { ExportCsvButton, Table, Td, Th, Tr } from '@/components/ui/table';
import type { UsageBucket } from '@/lib/api/types';
import { formatCount, plural } from '@/lib/format';
import { notify } from '@/stores/ui';

import { saveModelSpec, useModelInsight, useModels } from '../store';
import type { ModelView } from '../types';
import { withAlias, withoutAlias } from './alias-dialog';

const WEEK = 7 * 86_400_000;

export interface MigrationLine {
  kind: 'key' | 'alias';
  name: string;
  requests: number;
  lastWeek: number;
}

/** Who still calls a Model: each key and alias with its 30-day and 7-day requests, the busiest first. */
export function migrationLines(
  keys: UsageBucket[],
  requested: UsageBucket[],
  aliases: string[],
  now = Date.now(),
): MigrationLine[] {
  const sum = (rows: UsageBucket[], key: string, kind: MigrationLine['kind']) => {
    const map = new Map<string, MigrationLine>();
    for (const r of rows) {
      const name = r.group[key];
      if (!name) continue;
      const line = map.get(name) ?? { kind, name, requests: 0, lastWeek: 0 };
      line.requests += r.requests;
      if (new Date(r.start).getTime() >= now - WEEK) line.lastWeek += r.requests;
      map.set(name, line);
    }
    return [...map.values()];
  };
  const aliasLines = sum(requested, 'requested_model', 'alias').filter((l) => aliases.includes(l.name));
  for (const a of aliases)
    if (!aliasLines.some((l) => l.name === a))
      aliasLines.push({ kind: 'alias', name: a, requests: 0, lastWeek: 0 });
  return [...sum(keys, 'key', 'key'), ...aliasLines].sort(
    (a, b) => b.lastWeek - a.lastWeek || b.requests - a.requests,
  );
}

/** Moves an alias to another Model: off this one, onto that one, putting it back if the second write fails. */
async function moveAlias(from: string, to: string, alias: string): Promise<void> {
  const source = useModels.getState().byName(from);
  const target = useModels.getState().byName(to);
  if (!source || !target) throw new Error('The model is gone; reload the page');
  const entry = source.spec.aliases?.[alias] ?? {};
  await saveModelSpec(from, withoutAlias(source.spec, alias));
  try {
    await saveModelSpec(
      to,
      withAlias(target.spec, null, alias, { description: entry.description ?? '', owner: entry.owner ?? '' }),
    );
  } catch (error) {
    const again = useModels.getState().byName(from);
    if (again)
      await saveModelSpec(from, {
        ...again.spec,
        aliases: { ...(again.spec.aliases ?? {}), [alias]: entry },
      });
    throw error;
  }
}

/**
 * A deprecated Model's migration checklist: the keys and aliases that used it in the last 30 days,
 * marked done once they have not called it for 7 days; an alias can be moved to the replacement.
 */
export function MigrationPanel({ view }: { view: ModelView }) {
  const insight = useModelInsight((s) => s.slots[view.name]);
  const canEdit = useCan('replaceModel');
  const [moving, setMoving] = useState<string | null>(null);
  const replacement = view.model.spec.lifecycle?.replacement;
  if (view.lifecycle !== 'deprecated') return null;
  const lines = insight?.data ? migrationLines(insight.data.keys, insight.data.requested, view.aliases) : [];
  const done = lines.filter((l) => l.kind === 'key' && l.lastWeek === 0).length;
  return (
    <DetailSection
      title="Migration checklist"
      aside={
        <ExportCsvButton
          filename={`${view.name}-migration.csv`}
          rows={lines}
          columns={[
            { header: 'kind', value: (l) => l.kind },
            { header: 'name', value: (l) => l.name },
            { header: 'requests_30d', value: (l) => l.requests },
            { header: 'requests_7d', value: (l) => l.lastWeek },
          ]}
        />
      }
    >
      {!insight?.data ? (
        <p className="text-ui text-muted">
          {insight?.status === 'error' ? 'Usage couldn’t be read.' : 'Reading who calls it…'}
        </p>
      ) : lines.length === 0 ? (
        <p className="text-ui text-muted">
          No key or alias used it in the last 30 days: it can be turned off safely.
        </p>
      ) : (
        <>
          <p className="mb-3 text-ui text-muted">
            {plural(lines.length, 'key or alias', 'keys and aliases')} used it in the last 30 days; {done} of
            them
            {done === 1 ? ' has' : ' have'} moved off it.
            {replacement ? (
              <>
                {' '}
                Move them to <span className="mono text-fg">{replacement}</span>.
              </>
            ) : null}
          </p>
          <div className="overflow-x-auto">
            <Table label={`Migration of ${view.name}`}>
              <thead>
                <tr>
                  <Th>Key or alias</Th>
                  <Th numeric>Requests, 30 days</Th>
                  <Th numeric>Last 7 days</Th>
                  <Th>State</Th>
                  {canEdit && replacement ? (
                    <Th>
                      <span className="sr-only">Actions</span>
                    </Th>
                  ) : null}
                </tr>
              </thead>
              <tbody>
                {lines.map((l) => {
                  const isDone = l.kind === 'key' && l.lastWeek === 0;
                  return (
                    <Tr key={`${l.kind}/${l.name}`}>
                      <Td>
                        <span className="flex items-center gap-1.5">
                          <Badge>{l.kind === 'key' ? 'Key' : 'Alias'}</Badge>
                          <span className="mono">{l.kind === 'alias' ? `@${l.name}` : l.name}</span>
                        </span>
                      </Td>
                      <Td numeric>{formatCount(l.requests)}</Td>
                      <Td numeric>{formatCount(l.lastWeek)}</Td>
                      <Td>
                        {isDone ? (
                          <Badge tone="success">Moved</Badge>
                        ) : (
                          <Badge tone="warn">
                            {l.kind === 'key' ? 'Still calling' : 'Still points here'}
                          </Badge>
                        )}
                      </Td>
                      {canEdit && replacement ? (
                        <Td>
                          {l.kind === 'alias' ? (
                            <Button size="sm" variant="ghost" onClick={() => setMoving(l.name)}>
                              Move to {replacement}
                            </Button>
                          ) : null}
                        </Td>
                      ) : null}
                    </Tr>
                  );
                })}
              </tbody>
            </Table>
          </div>
        </>
      )}
      {moving && replacement ? (
        <ConfirmDialog
          open
          onClose={() => setMoving(null)}
          tone="primary"
          title={`Move @${moving} to ${replacement}?`}
          confirmLabel="Move alias"
          withReason={false}
          effect={`Apps sending @${moving} get ${replacement} from then on, with the alias’s description and owner.`}
          onConfirm={async () => {
            await moveAlias(view.name, replacement, moving);
            notify('success', 'Alias moved', `@${moving} → ${replacement}`);
          }}
        />
      ) : null}
    </DetailSection>
  );
}
