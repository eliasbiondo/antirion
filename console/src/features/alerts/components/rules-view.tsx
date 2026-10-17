'use client';

import { useEffect, useMemo, useState } from 'react';

import { useQueryParam } from '@/components/layout/url-sync';

import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/choice';
import { ConfirmDialog } from '@/components/ui/confirm';
import { Input } from '@/components/ui/form';
import { useCan } from '@/components/ui/gate';
import { Panel } from '@/components/ui/layout';
import { MenuButton } from '@/components/ui/menu';
import { Dialog } from '@/components/ui/overlay';
import { EmptyAction, EmptyState, ErrorState, LoadingRows } from '@/components/ui/states';
import { CountNote, ExportCsvButton, Table, Td, Th, Tr } from '@/components/ui/table';
import type { AlertRuleResource } from '@/lib/api/types';
import { formatCount, plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { formatRelative } from '@/lib/time';
import { notify } from '@/stores/ui';

import {
  categoryWord,
  conditionExpression,
  describeCondition,
  durationWords,
  scopeWords,
  severityWord,
} from '../describe';
import { useAlertEditors } from '../editors';
import { suggestName } from '../form';
import { builtInRules, ruleStats } from '../stats';
import { severityTone, useAlerts } from '../store';

function useRuleSwitch() {
  const setRuleEnabled = useAlerts((s) => s.setRuleEnabled);
  const canEdit = useCan('patchAlertRule');
  return {
    canEdit,
    reason: canEdit ? undefined : "You don't have permission to change alert rules.",
    toggle: (r: AlertRuleResource, next: boolean) =>
      void setRuleEnabled(r.name, next).then(
        () => notify('success', next ? 'Rule turned on' : 'Rule turned off', r.name),
        () => undefined,
      ),
  };
}

/** Fires over 30 days per rule, from the instances read for the hero. */
function useFires(): (name: string) => number | null {
  const history = useAlerts((s) => s.history.data);
  const now = useNow(60_000);
  const rules = useAlerts((s) => s.rules.data);
  const counts = useMemo(
    () =>
      history
        ? new Map((rules ?? []).map((r) => [r.name, ruleStats(history, r.name, now).fires] as const))
        : null,
    [history, rules, now],
  );
  return (name: string) => (counts ? (counts.get(name) ?? 0) : null);
}

/** Built-in rules from the inbox and 60 days of history, once the rule list is known. */
function useBuiltIn(list: AlertRuleResource[]) {
  const inbox = useAlerts((s) => s.alerts.data);
  const history = useAlerts((s) => s.history.data);
  const now = useNow(60_000);
  return useMemo(
    () =>
      builtInRules(
        [...(inbox ?? []), ...(history ?? [])],
        list.map((r) => r.name),
        now,
      ),
    [inbox, history, list, now],
  );
}

/** The rules: on/off, condition, scope, fires over 30 days, severity and channels; each row opens the editor. */
export function RulesView() {
  const rules = useAlerts((s) => s.rules);
  const show = useAlertEditors((s) => s.show);
  const canCreate = useCan('createAlertRule');
  const canDelete = useCan('deleteAlertRule');
  const deleteRule = useAlerts((s) => s.deleteRule);
  const { canEdit, reason, toggle } = useRuleSwitch();
  const fires = useFires();
  const [deleting, setDeleting] = useState<AlertRuleResource | null>(null);
  const list = rules.data ?? [];
  // A link to a rule (`rule=<name>`, from the palette) opens its editor once the rules are in.
  const [ruleParam, setRuleParam] = useQueryParam('rule');
  useEffect(() => {
    if (!ruleParam || !rules.data) return;
    if (rules.data.some((r) => r.name === ruleParam)) show({ kind: 'rule', name: ruleParam });
    setRuleParam(null);
  }, [ruleParam, rules.data, show, setRuleParam]);
  const builtIn = useBuiltIn(list);
  const on = list.filter((r) => r.spec.enabled !== false).length;
  const owners = list.some((r) => r.spec.owner_team);
  const taken = list.map((r) => r.name);
  const createDenied = canCreate ? undefined : "You don't have permission to create alert rules.";
  const newRule = (
    <Button
      variant="primary"
      size="sm"
      icon="plus"
      disabled={!canCreate}
      disabledReason={createDenied}
      onClick={() => show({ kind: 'rule', name: null })}
    >
      New rule
    </Button>
  );
  if (rules.error && !rules.data) return <ErrorState error={rules.error} subject="The rule list" />;
  if (!rules.data) return <LoadingRows label="Loading alert rules" />;
  if (list.length === 0)
    return (
      <>
        <div
          className={
            builtIn.length
              ? 'flex min-h-[200px] items-center justify-center border-b border-line'
              : 'flex min-h-[420px] flex-1 items-center justify-center'
          }
        >
          <EmptyState
            title="No rules of your own yet"
            description={
              builtIn.length
                ? 'The built-in checks below still raise alerts. Add a rule from a template, such as error rate or a budget forecast.'
                : 'Start from a template, such as error rate or a budget forecast.'
            }
            action={
              <EmptyAction
                disabled={!canCreate}
                disabledReason={createDenied}
                onClick={() => show({ kind: 'rule', name: null })}
              >
                New rule
              </EmptyAction>
            }
          />
        </div>
        <BuiltInRules rules={builtIn} />
      </>
    );
  return (
    <>
      <Panel
        title="Rules"
        subtitle={`${plural(list.length, 'rule')} · ${formatCount(on)} on`}
        bodyClassName="px-0 pb-0"
        actions={
          <>
            <ExportCsvButton
              filename="alert-rules"
              rows={list}
              columns={[
                { header: 'Rule', value: (r) => r.name },
                { header: 'On', value: (r) => r.spec.enabled !== false },
                { header: 'Category', value: (r) => r.spec.category },
                { header: 'Condition', value: (r) => describeCondition(r.spec) },
                { header: 'Expression', value: (r) => conditionExpression(r.spec) },
                { header: 'Scope', value: (r) => scopeWords(r.spec.scope) },
                { header: 'Severity', value: (r) => r.spec.severity ?? 'warning' },
                { header: 'Cooldown', value: (r) => r.spec.cooldown ?? '15m' },
                { header: 'Fires, 30 days', value: (r) => fires(r.name) },
                { header: 'Channels', value: (r) => (r.spec.channels ?? []).join(' ') },
                { header: 'Escalation', value: (r) => (r.spec.escalation ?? []).join(' ') },
                { header: 'Owner', value: (r) => r.spec.owner_team ?? '' },
                {
                  header: 'Silenced in maintenance',
                  value: (r) => r.spec.silence_during_maintenance ?? false,
                },
              ]}
            />
            <Button size="sm" icon="sliders" onClick={() => show({ kind: 'manage' })}>
              Manage
            </Button>
            {newRule}
          </>
        }
      >
        <Table label="Alert rules">
          <thead>
            <tr>
              <Th className="w-14 pl-5">On</Th>
              <Th>Rule</Th>
              <Th>Condition</Th>
              <Th>Scope</Th>
              <Th numeric>Fires, 30 days</Th>
              <Th>Severity</Th>
              <Th>Notifies</Th>
              {owners ? <Th>Owner</Th> : null}
              <Th className="w-12">
                <span className="sr-only">Actions</span>
              </Th>
            </tr>
          </thead>
          <tbody>
            {list.map((r) => {
              const enabled = r.spec.enabled !== false;
              const n = fires(r.name);
              return (
                <Tr
                  key={r.name}
                  interactive
                  className={enabled ? undefined : '[&>td]:text-muted'}
                  onClick={(e) => {
                    if ((e.target as HTMLElement).closest('button,a')) return;
                    show({ kind: 'rule', name: r.name });
                  }}
                >
                  <Td className="pl-5">
                    <Switch
                      label={`${enabled ? 'Turn off' : 'Turn on'} ${r.name}`}
                      hideLabel
                      checked={enabled}
                      disabled={!canEdit}
                      disabledReason={reason}
                      onChange={(next) => toggle(r, next)}
                    />
                  </Td>
                  <Td>
                    <button
                      type="button"
                      className="mono block text-left text-fg hover:underline"
                      onClick={() => show({ kind: 'rule', name: r.name })}
                    >
                      {r.name}
                    </button>
                    <span className="block text-caption text-muted">
                      {categoryWord(r.spec.category)}
                      {r.spec.silence_during_maintenance ? ' · silent in maintenance' : ''}
                    </span>
                  </Td>
                  <Td className="whitespace-normal" title={conditionExpression(r.spec)}>
                    {describeCondition(r.spec)}
                    {r.spec.cooldown ? (
                      <span className="block text-caption text-muted">
                        Quiet for {r.spec.cooldown === '0s' ? 'no time' : durationWords(r.spec.cooldown)}{' '}
                        after it fires
                      </span>
                    ) : null}
                  </Td>
                  <Td muted className="whitespace-normal">
                    {r.spec.scope && Object.keys(r.spec.scope).length ? (
                      <span className="flex flex-wrap gap-1">
                        {Object.entries(r.spec.scope).map(([k, v]) => (
                          <Badge key={k} mono>
                            {k}: {v}
                          </Badge>
                        ))}
                      </span>
                    ) : (
                      'Whole gateway'
                    )}
                  </Td>
                  <Td numeric mono muted={!n}>
                    {n === null ? '—' : formatCount(n)}
                  </Td>
                  <Td>
                    <span className="inline-flex items-center gap-1.5">
                      <Dot tone={severityTone(r.spec.severity ?? 'warning')} />
                      {severityWord(r.spec.severity ?? 'warning')}
                    </span>
                  </Td>
                  <Td mono muted>
                    {(r.spec.channels ?? []).join(', ') || '—'}
                  </Td>
                  {owners ? <Td muted>{r.spec.owner_team ?? '—'}</Td> : null}
                  <Td>
                    <MenuButton
                      label={`Actions for ${r.name}`}
                      iconOnly
                      size="sm"
                      variant="ghost"
                      items={[
                        { label: 'Edit', icon: 'edit', onSelect: () => show({ kind: 'rule', name: r.name }) },
                        {
                          label: 'Duplicate',
                          icon: 'copy',
                          disabled: !canCreate,
                          disabledReason: createDenied,
                          onSelect: () =>
                            show({
                              kind: 'rule',
                              name: null,
                              draft: { name: suggestName(r.name, taken), spec: r.spec },
                            }),
                        },
                        {
                          label: 'Delete',
                          icon: 'trash',
                          danger: true,
                          disabled: !canDelete,
                          disabledReason: canDelete
                            ? undefined
                            : "You don't have permission to delete rules.",
                          onSelect: () => setDeleting(r),
                        },
                      ]}
                    />
                  </Td>
                </Tr>
              );
            })}
          </tbody>
        </Table>
        <ConfirmDialog
          open={deleting !== null}
          onClose={() => setDeleting(null)}
          title={`Delete rule ${deleting?.name ?? ''}?`}
          confirmLabel="Delete rule"
          objectName={deleting?.name}
          typeToConfirm={deleting?.name}
          effect="The rule stops evaluating. Its past alerts stay in the inbox until they age out."
          onConfirm={async () => {
            if (!deleting) return;
            await deleteRule(deleting);
            notify('success', 'Rule deleted', deleting.name);
          }}
        />
      </Panel>
      <BuiltInRules rules={builtIn} />
    </>
  );
}

/** The gateway's own checks, seen through the alerts they raised; they have no editor. */
function BuiltInRules({ rules }: { rules: ReturnType<typeof builtInRules> }) {
  const now = useNow(60_000);
  if (rules.length === 0) return null;
  return (
    <Panel
      title="Built into the gateway"
      subtitle={`${plural(rules.length, 'check')} that raised alerts in the last 60 days. They can't be edited here.`}
      bodyClassName="px-0 pb-0"
    >
      <Table label="Built-in checks">
        <thead>
          <tr>
            <Th className="pl-5">Check</Th>
            <Th>Category</Th>
            <Th>Last severity</Th>
            <Th numeric>Fires, 30 days</Th>
            <Th>Last fired</Th>
          </tr>
        </thead>
        <tbody>
          {rules.map((r) => (
            <Tr key={r.name}>
              <Td mono className="pl-5">
                {r.name}
              </Td>
              <Td muted>{categoryWord(r.category)}</Td>
              <Td>
                <span className="inline-flex items-center gap-1.5">
                  <Dot tone={severityTone(r.severity)} />
                  {severityWord(r.severity)}
                </span>
              </Td>
              <Td numeric mono muted={!r.fires}>
                {formatCount(r.fires)}
              </Td>
              <Td muted>{formatRelative(r.lastFired, new Date(now))}</Td>
            </Tr>
          ))}
        </tbody>
      </Table>
    </Panel>
  );
}

/** Every rule in one searchable list with its switch; a row opens the editor over it. */
export function ManageRulesDialog() {
  const rules = useAlerts((s) => s.rules.data) ?? [];
  const close = useAlertEditors((s) => s.close);
  const show = useAlertEditors((s) => s.show);
  const canCreate = useCan('createAlertRule');
  const { canEdit, reason, toggle } = useRuleSwitch();
  const fires = useFires();
  const [q, setQ] = useState('');
  const needle = q.trim().toLowerCase();
  const visible = rules.filter(
    (r) =>
      !needle ||
      [
        r.name,
        r.spec.category,
        categoryWord(r.spec.category),
        describeCondition(r.spec),
        scopeWords(r.spec.scope),
        r.spec.description ?? '',
      ]
        .join(' ')
        .toLowerCase()
        .includes(needle),
  );
  const on = rules.filter((r) => r.spec.enabled !== false).length;
  return (
    <Dialog
      open
      onClose={close}
      size="lg"
      title="Manage alert rules"
      description={`${formatCount(on)} of ${plural(rules.length, 'rule')} on. Turn rules on or off here, or open one to change it.`}
      footer={
        <>
          <span className="mr-auto">
            <CountNote shown={visible.length} total={rules.length} noun="rules" />
          </span>
          <Button variant="ghost" onClick={close}>
            Done
          </Button>
          <Button
            variant="primary"
            icon="plus"
            disabled={!canCreate}
            disabledReason={canCreate ? undefined : "You don't have permission to create alert rules."}
            onClick={() => show({ kind: 'rule', name: null })}
          >
            New rule
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <Input
          icon="search"
          aria-label="Search rules"
          placeholder="Search by name, category, condition or scope"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          data-autofocus
        />
        {visible.length === 0 ? (
          <EmptyState size="sm" title={rules.length ? `No rule matches "${q}"` : 'No rules yet'} />
        ) : (
          <ul
            className="flex flex-col divide-y divide-line rounded-control border border-line"
            aria-label="Rules"
          >
            {visible.map((r) => {
              const enabled = r.spec.enabled !== false;
              const n = fires(r.name);
              return (
                <li key={r.name} className="flex items-center gap-3 px-3 py-2">
                  <Switch
                    label={`${enabled ? 'Turn off' : 'Turn on'} ${r.name}`}
                    hideLabel
                    checked={enabled}
                    disabled={!canEdit}
                    disabledReason={reason}
                    onChange={(next) => toggle(r, next)}
                  />
                  <button
                    type="button"
                    onClick={() => show({ kind: 'rule', name: r.name })}
                    className="min-w-0 flex-1 rounded-xs text-left"
                  >
                    <span
                      className={enabled ? 'mono block text-ui text-fg' : 'mono block text-ui text-muted'}
                    >
                      {r.name}
                    </span>
                    <span className="block truncate text-caption text-muted">
                      {describeCondition(r.spec)} · {scopeWords(r.spec.scope)}
                    </span>
                  </button>
                  <Badge>{categoryWord(r.spec.category)}</Badge>
                  <span className="inline-flex w-20 items-center gap-1.5 text-caption text-fg max-sm:hidden">
                    <Dot tone={severityTone(r.spec.severity ?? 'warning')} />
                    {severityWord(r.spec.severity ?? 'warning')}
                  </span>
                  <span className="mono w-16 text-right text-caption text-muted tabular-nums max-sm:hidden">
                    {n === null ? '—' : `${formatCount(n)} fired`}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </Dialog>
  );
}
