'use client';

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Switch } from '@/components/ui/form';
import { Gate, useCan } from '@/components/ui/gate';
import { MenuButton } from '@/components/ui/menu';
import { EmptyAction, EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import type { InvalidationRuleResource } from '@/lib/api/types';
import { formatCount, plural } from '@/lib/format';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';

import { describeRule } from '../rules';
import { useCache } from '../store';
import { RuleEditor } from './rule-editor';

/**
 * Clearing rules: each in words, with its switch, its firings in the last 24 hours and the last one,
 * Run now, edit and delete; "Create rule" opens the editor with its two presets.
 */
export function RulesTab() {
  const rules = useCache((s) => s.rules);
  const status = useCache((s) => s.ruleStatus);
  const load = useCache((s) => s.load);
  const runRule = useCache((s) => s.runRule);
  const setRuleEnabled = useCache((s) => s.setRuleEnabled);
  const deleteRule = useCache((s) => s.deleteRule);
  const canEdit = useCan('patchInvalidationRule');
  const canDelete = useCan('deleteInvalidationRule');
  const canReplace = useCan('replaceInvalidationRule');
  const [editing, setEditing] = useState<InvalidationRuleResource | 'new' | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const list = rules.data ?? [];
  const on = list.filter((r) => r.spec.enabled !== false).length;
  const fired = [...(status.data?.values() ?? [])].reduce((n, s) => n + s.fires_24h, 0);

  return (
    <section aria-labelledby="cache-rules-title">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-line bg-raised px-gutter pt-4 pb-3 max-sm:px-gutter-sm">
        <h2 id="cache-rules-title" className="text-title font-semibold text-fg">
          Clearing rules
        </h2>
        <span className="text-ui text-muted">
          {list.length > 0
            ? `${on} of ${list.length} on${status.data ? ` · ${plural(fired, 'firing')} in the last 24 h` : ''}`
            : 'Clear cached responses when something changes, or on a schedule.'}
        </span>
        <Gate operation="createInvalidationRule" mode="hide">
          <Button
            size="sm"
            variant="primary"
            icon="plus"
            className="ml-auto"
            onClick={() => setEditing('new')}
          >
            Create rule
          </Button>
        </Gate>
      </div>
      {rules.status === 'loading' && !rules.data ? <LoadingRows rows={3} /> : null}
      {rules.error ? (
        <ErrorState error={rules.error} subject="Clearing rules" onRetry={() => void load()} />
      ) : null}
      {rules.data?.length === 0 ? (
        <EmptyState
          title="No clearing rules"
          description="Rules clear stale answers when a model or route changes, or on a schedule."
          action={
            <Gate operation="createInvalidationRule" mode="hide">
              <EmptyAction onClick={() => setEditing('new')}>Create rule</EmptyAction>
            </Gate>
          }
        />
      ) : null}
      {list.length > 0 ? (
        <ul className="flex flex-col border-b border-line bg-raised" aria-label="Clearing rules">
          {list.map((r) => {
            const s = status.data?.get(r.name);
            const enabled = r.spec.enabled !== false;
            return (
              <li
                key={r.name}
                className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-line px-gutter py-3 last:border-0 max-sm:px-gutter-sm"
              >
                <Switch
                  label={`${enabled ? 'Turn off' : 'Turn on'} ${r.name}`}
                  hideLabel
                  checked={enabled}
                  disabled={!canEdit}
                  disabledReason="You don't have permission to change clearing rules."
                  onChange={(next) =>
                    void setRuleEnabled(r.name, next).then(
                      () => notify('success', next ? 'Rule turned on' : 'Rule turned off', r.name),
                      () => undefined,
                    )
                  }
                />
                <div className="min-w-0 flex-1">
                  <div className="mono text-ui text-fg">{r.name}</div>
                  <div className={enabled ? 'text-ui text-fg' : 'text-ui text-muted'}>
                    {describeRule(r.spec)}
                  </div>
                  {r.spec.description ? (
                    <div className="text-caption text-muted">{r.spec.description}</div>
                  ) : null}
                </div>
                <div className="w-36 text-right max-sm:text-left" aria-label={`Firings of ${r.name}`}>
                  {s ? (
                    <>
                      <div className="mono text-body text-fg tabular-nums">
                        {formatCount(s.fires_24h)}
                        <span className="ml-1 font-sans text-caption text-muted">in 24 h</span>
                      </div>
                      <div className="text-caption text-muted">
                        {s.last_fired_at
                          ? `Last ${formatRelative(s.last_fired_at)}${
                              s.last_dropped ? ` · ${plural(s.last_dropped, 'response')}` : ''
                            }`
                          : 'Never fired'}
                      </div>
                    </>
                  ) : (
                    <span className="text-caption text-muted">—</span>
                  )}
                </div>
                <div className="flex items-center gap-1">
                  <Gate operation="runInvalidationRule" mode="hide">
                    <Button
                      size="sm"
                      variant="ghost"
                      icon="play"
                      onClick={() =>
                        void runRule(r.name).then(
                          (dropped) =>
                            notify(
                              'success',
                              'Rule ran',
                              `${r.name} removed ${plural(dropped, 'response')}.`,
                            ),
                          () => undefined,
                        )
                      }
                    >
                      Run now
                    </Button>
                  </Gate>
                  {canReplace || canDelete ? (
                    <MenuButton
                      iconOnly
                      size="sm"
                      variant="ghost"
                      label={`More actions for ${r.name}`}
                      items={[
                        ...(canReplace
                          ? [{ label: 'Edit', icon: 'edit' as const, onSelect: () => setEditing(r) }]
                          : []),
                        ...(canDelete
                          ? [
                              {
                                label: 'Delete',
                                icon: 'trash' as const,
                                danger: true,
                                onSelect: () => setDeleting(r.name),
                              },
                            ]
                          : []),
                      ]}
                    />
                  ) : null}
                </div>
              </li>
            );
          })}
        </ul>
      ) : null}
      {editing ? (
        <RuleEditor rule={editing === 'new' ? null : editing} onClose={() => setEditing(null)} />
      ) : null}
      <ConfirmDialog
        open={deleting !== null}
        onClose={() => setDeleting(null)}
        title="Delete this clearing rule?"
        objectName={deleting ?? undefined}
        confirmLabel="Delete rule"
        effect="It stops clearing responses. Responses it already cleared stay cleared."
        onConfirm={async (reason) => {
          if (!deleting) return;
          await deleteRule(deleting, reason);
          notify('success', 'Clearing rule deleted', deleting);
        }}
      />
    </section>
  );
}
