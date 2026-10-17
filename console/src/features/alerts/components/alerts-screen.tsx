'use client';

import { useEffect, useMemo } from 'react';

import { useQueryParam, useSetQueryParams, useTabParam } from '@/components/layout/url-sync';
import { Badge, Dot, type Tone } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ChipToggle, Segmented } from '@/components/ui/form';
import { FilterBar, PageHeader, Tabs } from '@/components/ui/layout';
import { SelectableList } from '@/components/ui/list-detail';
import { UpdatedAt } from '@/components/ui/misc';
import { Drawer } from '@/components/ui/overlay';
import {
  EmptyAction,
  EmptyState,
  ErrorState,
  isUnavailable,
  LoadingRows,
  NoMatch,
  UnavailableState,
} from '@/components/ui/states';
import { CountNote, ExportCsvButton } from '@/components/ui/table';
import type { Alert } from '@/lib/api/types';
import { useMediaQuery, useNow, usePolling } from '@/lib/hooks';
import { exportCsv } from '@/lib/csv';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';
import { Icon } from '@/components/ui/icon';
import { KeyboardHints } from '@/components/ui/keyboard-hints';

import { agoWords, scopeWords, severityWord, spanWords, STATUS_WORD } from '../describe';
import { useAlertEditors, type Editor } from '../editors';
import { SEVERITIES } from '../stats';
import { severityTone, useAlerts } from '../store';
import { AlertDetail, durationOf, snoozed } from './alert-detail';
import { ChannelDrawer } from './channel-drawer';
import { ChannelsView } from './channels-view';
import { AlertsHero } from './hero';
import { RuleDrawer } from './rule-drawer';
import { ManageRulesDialog, RulesView } from './rules-view';
import { WebhookDrawer } from './webhook-drawer';

type Status = 'all' | 'firing' | 'acknowledged' | 'resolved';
const STATUSES: Status[] = ['all', 'firing', 'acknowledged', 'resolved'];

/** The inbox's filters as URL parameters, so a copied link keeps them. */
const INBOX_FILTERS = { status: 'all', severity: [] as string[] };
type View = 'inbox' | 'rules' | 'channels';

const STATUS_TONE: Record<Alert['status'], Tone> = {
  firing: 'error',
  acknowledged: 'warn',
  resolved: 'success',
};

/** A tab count only when there is something to count. */
function positive(n: number | undefined): number | undefined {
  return n !== undefined && n > 0 ? n : undefined;
}

function StatusText({ alert }: { alert: Alert }) {
  return (
    <span className="inline-flex items-center gap-1.5 text-caption whitespace-nowrap">
      <Dot tone={STATUS_TONE[alert.status]} />
      <span className="text-fg">{STATUS_WORD[alert.status]}</span>
    </span>
  );
}

function AlertRow({ alert, now }: { alert: Alert; now: number }) {
  const lasting =
    alert.status === 'resolved'
      ? `resolved after ${spanWords(durationOf(alert, now))}`
      : `lasting ${spanWords(durationOf(alert, now))}`;
  return (
    <div className="flex items-start gap-3">
      <div className="min-w-0 flex-1">
        <p className="truncate text-body font-semibold text-fg">{alert.summary}</p>
        <p className="mt-0.5 text-caption text-muted">
          Fired {agoWords(alert.fired_at, now)} · {lasting}
        </p>
      </div>
      <div className="flex shrink-0 flex-col items-end gap-1">
        <StatusText alert={alert} />
        {snoozed(alert, now) ? (
          <span className="inline-flex items-center gap-1 text-caption text-muted">
            <Icon name="clock" size={12} />
            Snoozed
          </span>
        ) : alert.owner_team ? (
          <Badge>{alert.owner_team}</Badge>
        ) : null}
      </div>
    </div>
  );
}

/** The open editors, each over the one under it. */
function Editors() {
  const stack = useAlertEditors((s) => s.stack);
  const one = (e: Editor, i: number) => {
    const key = `${i}-${e.kind}-${'name' in e ? (e.name ?? 'new') : ''}`;
    switch (e.kind) {
      case 'manage':
        return <ManageRulesDialog key={key} />;
      case 'rule':
        return <RuleDrawer key={key} name={e.name} draft={e.draft} />;
      case 'channel':
        return <ChannelDrawer key={key} name={e.name} webhook={e.webhook} />;
      case 'webhook':
        return <WebhookDrawer key={key} name={e.name} events={e.events} />;
    }
  };
  return <>{stack.map(one)}</>;
}

const CSV_COLUMNS = [
  { header: 'Alert', value: (a: Alert) => a.id },
  { header: 'Summary', value: (a: Alert) => a.summary },
  { header: 'Rule', value: (a: Alert) => a.rule },
  { header: 'Category', value: (a: Alert) => a.category },
  { header: 'Severity', value: (a: Alert) => a.severity },
  { header: 'Status', value: (a: Alert) => a.status },
  { header: 'Fired at', value: (a: Alert) => a.fired_at },
  { header: 'Acknowledged at', value: (a: Alert) => a.acknowledged_at ?? '' },
  { header: 'Acknowledged by', value: (a: Alert) => a.acknowledged_by ?? '' },
  { header: 'Resolved at', value: (a: Alert) => a.resolved_at ?? '' },
  { header: 'Resolved by', value: (a: Alert) => a.resolved_by ?? '' },
  {
    header: 'Duration (s)',
    value: (a: Alert) => Math.round(durationOf(a, Date.now())),
  },
  { header: 'Snoozed until', value: (a: Alert) => a.snoozed_until ?? '' },
  {
    header: 'Scope',
    value: (a: Alert) => (a.scope && Object.keys(a.scope).length ? scopeWords(a.scope) : ''),
  },
  { header: 'Value', value: (a: Alert) => a.value ?? '' },
  { header: 'Threshold', value: (a: Alert) => a.threshold ?? '' },
  { header: 'Owner', value: (a: Alert) => a.owner_team ?? '' },
  { header: 'Requests affected', value: (a: Alert) => a.impact?.requests ?? '' },
  { header: 'Errors', value: (a: Alert) => a.impact?.errors ?? '' },
  {
    header: 'Notifications',
    value: (a: Alert) => (a.timeline ?? []).filter((t) => t.kind === 'notified').length,
  },
  { header: 'Grouping key', value: (a: Alert) => a.fingerprint },
];

/** Alerts: 30-day figures, the inbox with its detail, the rules and the channels, with their editors. */
export function AlertsScreen() {
  const alerts = useAlerts((s) => s.alerts);
  const history = useAlerts((s) => s.history.data);
  const rules = useAlerts((s) => s.rules);
  const channels = useAlerts((s) => s.channels);
  const load = useAlerts((s) => s.load);
  const show = useAlertEditors((s) => s.show);
  const now = useNow(30_000);
  const wide = !useMediaQuery('(max-width: 1023px)');
  const [viewParam, setView] = useTabParam();
  const [selected, setSelected] = useQueryParam('alert');
  const setParams = useSetQueryParams();
  const url = useUrlFilters(INBOX_FILTERS);
  const status: Status = STATUSES.includes(url.filters.status as Status)
    ? (url.filters.status as Status)
    : 'all';
  const severities = url.filters.severity;
  const setStatus = (next: Status) => url.set({ status: next });
  useEffect(() => {
    void load();
  }, [load]);
  usePolling(() => void load({ quiet: true }));
  // Editors belong to this visit of the page.
  useEffect(() => () => useAlertEditors.setState({ stack: [] }), []);

  const view: View = viewParam === 'rules' || viewParam === 'channels' ? viewParam : 'inbox';
  const list = useMemo(() => alerts.data ?? [], [alerts.data]);
  const visible = list.filter(
    (a) =>
      (status === 'all' || a.status === status) &&
      (severities.length === 0 || severities.includes(a.severity)),
  );
  usePageShortcut('e', () => {
    if (view === 'inbox' && visible.length > 0) exportCsv('alerts', CSV_COLUMNS, visible);
  });
  // An alert the filters hide does not stay open beside "No results": the detail follows the list.
  const filtered = status !== 'all' || severities.length > 0;
  const linked = list.find((a) => a.id === selected) ?? history?.find((a) => a.id === selected);
  // A link to an alert this gateway does not have says so, rather than opening another one.
  const missing = selected !== null && alerts.data !== null && !linked;
  const current = missing
    ? undefined
    : ((linked && (!filtered || visible.includes(linked)) ? linked : undefined) ??
      (wide ? visible[0] : undefined));
  // The status counts follow the severity filter, and the reverse.
  const bySeverity = list.filter((a) => severities.length === 0 || severities.includes(a.severity));
  const firing = bySeverity.filter((a) => a.status === 'firing');
  const acknowledged = bySeverity.filter((a) => a.status === 'acknowledged');

  const unavailable =
    isUnavailable(alerts.error) &&
    isUnavailable(rules.error) &&
    isUnavailable(channels.error) &&
    !alerts.data;

  const header = (
    <PageHeader
      title="Alerts"
      description="What needs attention on the gateway, and the rules that raise it"
      actions={
        <>
          <UpdatedAt at={alerts.at} />
          {unavailable ? null : (
            <Button icon="sliders" onClick={() => show({ kind: 'manage' })} disabled={!rules.data}>
              Manage rules
            </Button>
          )}
        </>
      }
    />
  );

  if (unavailable) {
    return (
      <>
        {header}
        <div className="flex min-h-[calc(100vh-48px-57px)] items-center justify-center bg-raised">
          <UnavailableState
            subject="Alerting"
            description="Alerts tell you when latency, errors, spend or guardrails cross a line you set, and who was told. This version of the gateway doesn't include them; upgrade the gateway to use them."
            operation={(alerts.error as { operation?: string } | null)?.operation}
          />
        </div>
      </>
    );
  }

  return (
    <>
      {header}
      {alerts.data && (list.length > 0 || (history?.length ?? 0) > 0) ? (
        <AlertsHero
          inbox={list}
          now={now}
          onFiring={() => url.setWith({ status: 'firing' }, { tab: null, view: null })}
        />
      ) : null}
      <Tabs<View>
        label="Alerts views"
        value={view}
        onChange={(v) =>
          // An alert selected in the inbox does not follow to the rules or channels.
          setParams({ tab: v === 'inbox' ? null : v, view: null, ...(v !== 'inbox' ? { alert: null } : {}) })
        }
        items={[
          {
            value: 'inbox',
            label: 'Inbox',
            count: positive(firing.length + acknowledged.length),
            icon: 'bell',
          },
          { value: 'rules', label: 'Rules', count: positive(rules.data?.length), icon: 'sliders' },
          { value: 'channels', label: 'Channels', count: positive(channels.data?.length), icon: 'send' },
        ]}
      />
      {view === 'rules' ? (
        <div className="flex grow flex-col bg-raised">
          <RulesView />
        </div>
      ) : null}
      {view === 'channels' ? (
        <div className="flex grow flex-col bg-raised">
          <ChannelsView />
        </div>
      ) : null}
      {view === 'inbox' ? (
        <>
          {list.length > 0 ? (
            <FilterBar>
              <Segmented<Status>
                label="Alert status"
                value={status}
                onChange={setStatus}
                options={[
                  { value: 'all', label: 'All', count: bySeverity.length },
                  { value: 'firing', label: 'Firing', count: firing.length },
                  { value: 'acknowledged', label: 'Acknowledged', count: acknowledged.length },
                  {
                    value: 'resolved',
                    label: 'Resolved',
                    count: bySeverity.filter((a) => a.status === 'resolved').length,
                  },
                ]}
              />
              <div className="flex gap-1.5" role="group" aria-label="Severity">
                {SEVERITIES.map((sv) => (
                  <ChipToggle
                    key={sv}
                    tone={severityTone(sv)}
                    pressed={severities.includes(sv)}
                    count={
                      list.filter((a) => (status === 'all' || a.status === status) && a.severity === sv)
                        .length
                    }
                    onChange={(on) =>
                      url.set({ severity: on ? [...severities, sv] : severities.filter((x) => x !== sv) })
                    }
                  >
                    {severityWord(sv)}
                  </ChipToggle>
                ))}
              </div>
              <span className="ml-auto flex items-center gap-3">
                <CountNote shown={visible.length} total={list.length} noun="alerts" />
                <ExportCsvButton filename="alerts" rows={visible} columns={CSV_COLUMNS} />
              </span>
            </FilterBar>
          ) : null}
          {alerts.status === 'loading' && !alerts.data ? <LoadingRows label="Loading alerts" /> : null}
          {alerts.error && !alerts.data ? (
            <ErrorState error={alerts.error} subject="The alert inbox" onRetry={() => void load()} />
          ) : null}
          {alerts.data?.length === 0 ? (
            <div className="flex min-h-[420px] flex-1 items-center justify-center bg-raised">
              <EmptyState
                title="No alerts"
                action={
                  rules.data?.length ? (
                    <EmptyAction onClick={() => setView('rules')}>View rules</EmptyAction>
                  ) : (
                    <EmptyAction onClick={() => show({ kind: 'rule', name: null })}>Create rule</EmptyAction>
                  )
                }
              />
            </div>
          ) : null}
          {list.length > 0 ? (
            <div className="grid min-h-[520px] flex-auto lg:grid-cols-[minmax(340px,440px)_minmax(0,1fr)]">
              <div className="min-w-0 bg-canvas lg:border-r lg:border-line">
                {visible.length === 0 ? (
                  <NoMatch onClear={() => url.clear()} />
                ) : (
                  <SelectableList
                    label="Alerts"
                    items={visible}
                    keyOf={(a) => a.id}
                    selectedKey={current?.id ?? null}
                    onSelect={setSelected}
                    toneOf={(a) => severityTone(a.severity)}
                    renderItem={(a) => <AlertRow alert={a} now={now} />}
                  />
                )}
              </div>
              {wide ? (
                <div className="min-w-0 bg-raised lg:sticky lg:top-0 lg:max-h-(--pane-h) lg:self-start lg:overflow-y-auto">
                  {current ? (
                    <AlertDetail key={current.id} alert={current} onOpen={setSelected} />
                  ) : missing ? (
                    <EmptyState
                      title="We couldn't find this alert"
                      description="It may have been resolved and cleared, or the link has a mistake in it."
                    />
                  ) : (
                    <EmptyState title="Select an alert to see its details" />
                  )}
                </div>
              ) : null}
            </div>
          ) : null}
          {!wide ? (
            <Drawer
              open={current !== undefined}
              onClose={() => setSelected(null)}
              width="lg"
              title={current ? severityWord(current.severity) + ' alert' : 'Alert'}
            >
              <div className="-mx-5 -my-4">
                {current ? <AlertDetail key={current.id} alert={current} onOpen={setSelected} /> : null}
              </div>
            </Drawer>
          ) : null}
          {list.length > 0 ? <KeyboardHints keys={['move', 'open', 'close', 'export']} /> : null}
        </>
      ) : null}
      <Editors />
    </>
  );
}
