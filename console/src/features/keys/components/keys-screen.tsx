'use client';

import { useMemo, useState } from 'react';

import { useQueryParam, useTabParam } from '@/components/layout/url-sync';
import { Badge, Dot } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Combobox, Input, Select } from '@/components/ui/form';
import { Gate } from '@/components/ui/gate';
import { StarredFilter, useFavouriteNames, useStarredFilter } from '@/components/ui/favourite';
import { Icon } from '@/components/ui/icon';
import { KeyboardHints } from '@/components/ui/keyboard-hints';
import { FilterBar, PageHeader, SummaryStrip, type SummaryItem } from '@/components/ui/layout';
import { MasterDetail, SelectableList } from '@/components/ui/list-detail';
import { UpdatedAt } from '@/components/ui/misc';
import { RevealOnceDialog } from '@/components/ui/reveal-once';
import { EmptyAction, EmptyState, ErrorState, LoadingRows, NoMatch } from '@/components/ui/states';
import { CountNote } from '@/components/ui/table';
import { downloadCsv } from '@/lib/csv';
import { formatCount, formatMoney, formatPercent, formatSigned, plural } from '@/lib/format';
import { useNow } from '@/lib/hooks';
import { usePageShortcut } from '@/lib/shortcuts';
import { useUrlFilters } from '@/lib/url-filters';
import { formatRelative } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useScope } from '@/stores/scope';
import type { BudgetCounter } from '@/lib/api/types';
import Link from 'next/link';
import { useScopedHref } from '@/components/layout/url-sync';
import { Sparkline } from '@/components/ui/charts';
import { Tabs } from '@/components/ui/layout';

import {
  budgetRatio,
  EMPTY_FILTERS,
  filterKeys,
  KEY_URL_FILTERS,
  keyFiltersOf,
  keyFiltersToUrl,
  NO_OWNER,
  tightestBudget,
  type KeyFilters,
  useKeysScreenData,
  useKeyViews,
  useTeamOfProject,
} from '../hooks';
import { type Hourly, useKeys, useKeysContext } from '../store';
import {
  KEY_STATUSES,
  KEY_TYPES,
  type KeyStatus,
  type KeyType,
  isPlaygroundKey,
  type KeyView,
  STATUS_LABEL,
  statusTone,
  TYPE_LABEL,
} from '../types';
import { KeyDetail } from './key-detail';
import { change, lastDay, splitDays } from './key-facts';
import { KeyActivity, KeysByTeam, RotationTab } from './keys-tabs';
import { KeyFormDrawer } from './key-form-drawer';
import { KeySnippets } from './key-snippets';
import { Tooltip } from '@/components/ui/tooltip';

const STALE_MS = 60 * 86_400_000;

type KeysView = 'keys' | 'teams' | 'rotation' | 'activity';

function KeyRow({
  view,
  team,
  starred,
  budget,
}: {
  view: KeyView;
  team: string | undefined;
  starred: boolean;
  budget: BudgetCounter | null;
}) {
  const usage = useKeysContext((s) => s.usage.data?.get(view.name));
  const anomalies = useKeysContext((s) => s.anomalies.data?.filter((a) => a.key === view.name).length ?? 0);
  const ratio = budget ? budgetRatio(budget) : 0;
  return (
    <div className="flex items-start gap-3">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-body font-semibold text-fg">{view.name}</span>
          {starred ? (
            <span className="text-star" aria-label="Starred">
              <Icon name="starFilled" size={12} />
            </span>
          ) : null}
          <Badge tone="accent">{isPlaygroundKey(view.key) ? 'Playground' : TYPE_LABEL[view.type]}</Badge>
        </div>
        <div className="mt-1 truncate text-caption text-muted">
          <span className="mono">{view.key.metadata.hint ?? view.key.metadata.kid}</span> ·{' '}
          {team ? `${team} / ` : ''}
          {view.project}
        </div>
        <div className="mt-0.5 text-caption text-dim">
          {isPlaygroundKey(view.key)
            ? `Made in the playground${view.owner ? ` for ${view.owner}` : ''}${view.key.spec.expires_at ? ` · expires ${formatRelative(view.key.spec.expires_at)}` : ''}`
            : view.key.metadata.last_used_at
              ? `Last used ${formatRelative(view.key.metadata.last_used_at)}`
              : 'Never used'}
        </div>
      </div>
      <div className="flex shrink-0 flex-col items-end gap-1">
        <Badge tone={statusTone(view.status)} dot>
          {STATUS_LABEL[view.status]}
        </Badge>
        {usage ? (
          <Tooltip content="Requests, last 24 h">
            <span className="mono text-caption text-muted">
              {formatCount(usage.requests, { compact: true })} / 24 h
            </span>
          </Tooltip>
        ) : null}
        {budget && Number(budget.amount) > 0 ? (
          <Badge
            tone={ratio >= 1 ? 'error' : ratio >= 0.8 ? 'warn' : 'neutral'}
            title={`Tightest budget: ${budget.level} ${budget.owner} · ${budget.name}, ${formatMoney(budget.spent)} of ${formatMoney(budget.amount)}`}
          >
            <Icon name="dollar" size={12} /> {formatPercent(ratio, 0)}
          </Badge>
        ) : null}
        {anomalies > 0 ? (
          <Badge tone="warn" title="Unusual activity">
            <Icon name="alert" size={12} /> {anomalies}
          </Badge>
        ) : null}
      </div>
    </div>
  );
}

/** The figures over the list, each a filter; hidden while there are no keys. */
function KeysSummary({
  views,
  onFilter,
  onRotation,
}: {
  views: KeyView[];
  onFilter: (filters: Partial<KeyFilters>) => void;
  onRotation: () => void;
}) {
  const usage = useKeysContext((s) => s.usage);
  const hourly = useKeysContext((s) => s.hourly.data);
  const auth = useKeysContext((s) => s.auth.data);
  const openFlags = useKeysContext((s) => s.anomalies.data?.length);
  const now = useNow(60_000);
  // Playground keys are short-lived and made on their own: they don't count toward the figures.
  const counted = views.filter((v) => !isPlaygroundKey(v.key));
  const fromPlayground = views.length - counted.length;
  const live = counted.filter((v) => v.status !== 'expired' && v.status !== 'disabled');
  const rotationDue = counted.filter((v) => v.key.metadata.rotation_due).length;
  const expiring = counted.filter((v) => v.status === 'expiring').length;
  const stale = counted.filter((v) => {
    const last = v.key.metadata.last_used_at
      ? new Date(v.key.metadata.last_used_at).getTime()
      : v.key.metadata.created_at
        ? new Date(v.key.metadata.created_at).getTime()
        : now;
    return now - last > STALE_MS;
  }).length;
  const totals = usage.data
    ? [...usage.data.values()].reduce(
        (a, u) => ({ requests: a.requests + u.requests, cost: a.cost + u.cost }),
        { requests: 0, cost: 0 },
      )
    : null;
  const items: SummaryItem[] = [
    {
      label: 'Active keys',
      value: live.length,
      ...(fromPlayground > 0 ? { sub: `${plural(fromPlayground, 'playground key')} not counted` } : {}),
      onClick: () => onFilter({ statuses: ['active'] }),
    },
    {
      label: 'Expiring within 7 days',
      value: expiring,
      tone: expiring > 0 ? 'warn' : 'neutral',
      onClick: () => onFilter({ statuses: ['expiring'] }),
    },
    {
      label: 'Need rotation',
      value: rotationDue,
      tone: rotationDue > 0 ? 'warn' : 'neutral',
      title: 'Older than your rotation policy allows',
      onClick: onRotation,
    },
    { label: 'Unused for 60+ days', value: stale },
  ];
  const day = hourly ? splitDays(hourly, now) : null;
  const sum = (list: Hourly[], f: (h: Hourly) => number) => list.reduce((a, h) => a + f(h), 0);
  const trend = (f: (h: Hourly) => number, label: string) => {
    if (!day || !hourly) return undefined;
    const c = change(sum(day.current, f), sum(day.previous, f));
    return (
      <span className="flex items-center gap-2">
        <Sparkline
          values={lastDay(hourly, now, f).values}
          label={`${label} per hour, last 24 hours`}
          className="w-20"
        />
        {c !== null ? (
          <Tooltip content="Against the 24 hours before">
            <span className="mono text-caption text-muted">{formatSigned(c, 0, { percent: true })}</span>
          </Tooltip>
        ) : null}
      </span>
    );
  };
  if (totals) {
    items.push({
      label: 'Requests, last 24 h',
      value: formatCount(totals.requests, { compact: true }),
      sub: trend((h) => h.requests, 'Requests'),
    });
    items.push({
      label: 'Spend, last 24 h',
      value: formatMoney(totals.cost, { compact: true }),
      sub: trend((h) => h.cost, 'Spend'),
    });
  }
  const detection = auth?.spec.anomaly_detection?.enabled;
  if (detection !== undefined)
    items.push({
      label: 'Unusual activity',
      value: detection ? (openFlags ?? 0) : 'Off',
      tone: detection && (openFlags ?? 0) > 0 ? 'warn' : 'neutral',
      title: detection ? 'Open flags; anomaly detection is on' : 'Anomaly detection is off in Settings',
      sub: detection ? 'Detection on' : 'Detection off',
    });
  return <SummaryStrip label="API keys summary" items={items} />;
}

/** Open anomaly flags, as a notice over the list; nothing when there are none or none can be read. */
function AnomalyNotice() {
  const anomalies = useKeysContext((s) => s.anomalies.data);
  const dismiss = useKeysContext((s) => s.dismissAnomaly);
  const shown = usePreferences((s) => s.keyAnomalyNotice);
  const setPreference = usePreferences((s) => s.set);
  const logHref = useScopedHref('/request-log/');
  const [everyFlag, setEveryFlag] = useState(false);
  if (!anomalies || anomalies.length === 0) return null;
  const keyCount = new Set(anomalies.map((a) => a.key)).size;
  const listed = everyFlag ? anomalies : anomalies.slice(0, 3);
  if (!shown)
    return (
      <div className="flex items-center gap-2 border-b border-line bg-raised px-gutter py-1.5 text-caption text-muted max-sm:px-gutter-sm">
        <Icon name="alert" size={12} />
        {plural(anomalies.length, 'unusual activity flag')} hidden
        <Button size="sm" variant="ghost" onClick={() => setPreference({ keyAnomalyNotice: true })}>
          Show
        </Button>
      </div>
    );
  return (
    <section
      aria-label="Unusual activity"
      className="border-b border-line bg-raised px-gutter py-3 max-sm:px-gutter-sm"
    >
      <div className="mb-2 flex items-center gap-2 text-ui font-semibold text-fg">
        <span className="text-warn">
          <Icon name="alert" size={16} />
        </span>
        Unusual activity, last 7 days
        <span className="font-normal text-muted">
          ·{' '}
          {keyCount === anomalies.length
            ? plural(keyCount, 'key')
            : `${plural(anomalies.length, 'flag')} on ${plural(keyCount, 'key')}`}
        </span>
        <Button
          size="sm"
          variant="ghost"
          className="ml-auto"
          onClick={() => setPreference({ keyAnomalyNotice: false })}
        >
          Hide
        </Button>
      </div>
      <ul className="flex flex-col gap-1.5">
        {listed.map((a) => (
          <li key={a.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-ui">
            <Dot tone={a.severity === 'critical' ? 'error' : a.severity === 'warning' ? 'warn' : 'info'} />
            <span className="text-fg">
              <span className="font-semibold">{a.key}</span>{' '}
              {ANOMALY_TEXT[a.kind] ?? a.kind.replace(/_/g, ' ')}
            </span>
            <span className="text-caption text-muted">
              {plural(a.count, 'request')} · {formatRelative(a.last_seen)}
              {a.owner ? ` · owner ${a.owner}` : ''}
            </span>
            <Link
              href={`${logHref}${logHref.includes('?') ? '&' : '?'}key=${encodeURIComponent(a.key)}`}
              className="ml-auto text-caption text-accent hover:underline"
            >
              See requests
            </Link>
            <Gate operation="dismissAnomaly" mode="hide">
              <Button size="sm" variant="ghost" onClick={() => void dismiss(a.id)}>
                Dismiss
              </Button>
            </Gate>
          </li>
        ))}
      </ul>
      {anomalies.length > 3 ? (
        <Button size="sm" variant="ghost" className="mt-1.5 -ml-2" onClick={() => setEveryFlag(!everyFlag)}>
          {everyFlag ? 'Show fewer' : `Show ${plural(anomalies.length - 3, 'more flag')}`}
        </Button>
      ) : null}
    </section>
  );
}

const ANOMALY_TEXT: Record<string, string> = {
  country: 'used from a new country',
  spike: 'saw a sudden spike in requests',
  user_agent: 'used by an unfamiliar client',
  network: 'used from a new network',
  exposed: 'may have been exposed publicly',
};

/** API keys: a summary, the filters, the key list and the selected key. */
export function KeysScreen() {
  useKeysScreenData();
  const status = useKeys((s) => s.status);
  const error = useKeys((s) => s.error);
  const loadedAt = useKeys((s) => s.loadedAt);
  const fetch = useKeys((s) => s.fetch);
  const views = useKeyViews();
  const teamOf = useTeamOfProject();
  const teams = useKeysContext((s) => s.teams.data) ?? [];
  const scope = useScope((s) => s.scope);
  const url = useUrlFilters(KEY_URL_FILTERS);
  const filters = useMemo(() => keyFiltersOf(url.filters), [url.filters]);
  const setFilters = (next: KeyFilters) => url.set(keyFiltersToUrl(next));
  const [selected, setSelected] = useQueryParam('key');
  const [form, setForm] = useState<'new' | 'edit' | null>(null);
  const [secret, setSecret] = useState<{ title: string; value: string; created: boolean } | null>(null);
  const [tab, setTab] = useTabParam();
  const view: KeysView = tab === 'teams' || tab === 'rotation' || tab === 'activity' ? tab : 'keys';
  const budgets = useKeysContext((s) => s.budgets.data);
  const now = useNow(60_000);
  const scopeOrg = scope.kind === 'organization' ? scope.name : undefined;
  const owners = useMemo(
    () => [...new Set(views.map((v) => v.owner).filter((o): o is string => o !== null))].sort(),
    [views],
  );
  const openKey = (name: string) => {
    setTab(null);
    setSelected(name);
  };

  const starred = useStarredFilter('key');
  const starredNames = useFavouriteNames('key');
  const visible = starred.apply(
    useMemo(() => filterKeys(views, filters, teamOf), [views, filters, teamOf]),
    (v) => v.name,
  );
  const linked = views.find((v) => v.name === selected);
  // A link to a key that is not there (revoked, mistyped) says so instead of opening another key;
  // a key the filters hide does not stay open beside "No results".
  const missing = selected !== null && loadedAt !== null && status !== 'loading' && !linked;
  const current = missing
    ? undefined
    : ((linked && (!url.active || visible.includes(linked)) ? linked : undefined) ?? visible[0]);
  const typeCount = (t: KeyType) => views.filter((v) => v.type === t).length;
  const statusCount = (s: KeyStatus) => views.filter((v) => v.status === s).length;
  const filtered = url.active;
  const clearAll = () => url.clear();

  usePageShortcut('n', () => setForm('new'));
  usePageShortcut('e', () => exportKeys(visible, teamOf));

  return (
    <>
      <PageHeader
        title="API keys"
        description="Credentials your apps use to send requests through the gateway"
        actions={
          <>
            <UpdatedAt at={loadedAt} />
            {views.length > 0 ? (
              <Button icon="download" variant="ghost" onClick={() => exportKeys(visible, teamOf)}>
                Export CSV
              </Button>
            ) : null}
            <Gate operation="createKey" mode="hide">
              <Button icon="plus" variant="primary" onClick={() => setForm('new')}>
                Create key
              </Button>
            </Gate>
          </>
        }
      />
      {views.length > 0 ? (
        <KeysSummary
          views={views}
          onFilter={(f) =>
            url.setWith(
              { ...keyFiltersToUrl({ ...EMPTY_FILTERS, ...f }), starred: false },
              { tab: null, view: null },
            )
          }
          onRotation={() => setTab('rotation')}
        />
      ) : null}
      <AnomalyNotice />
      {views.length > 0 ? (
        <Tabs<KeysView>
          label="Key views"
          value={view}
          onChange={(v) => setTab(v === 'keys' ? null : v)}
          items={[
            { value: 'keys', label: 'Keys', count: views.length, icon: 'key' },
            { value: 'teams', label: 'By team', icon: 'users' },
            { value: 'rotation', label: 'Rotation', icon: 'rotate' },
            { value: 'activity', label: 'Activity', icon: 'clock' },
          ]}
        />
      ) : null}
      {view === 'teams' ? <KeysByTeam views={views} teamOf={teamOf} onOpen={openKey} /> : null}
      {view === 'rotation' ? <RotationTab views={views} onOpen={openKey} now={now} /> : null}
      {view === 'activity' ? <KeyActivity onOpen={openKey} /> : null}
      {view === 'keys' && views.length > 0 ? (
        <FilterBar>
          <div className="w-64 max-sm:w-full">
            <Input
              data-page-search
              aria-label="Search keys"
              icon="search"
              value={filters.query}
              onChange={(e) => setFilters({ ...filters, query: e.target.value })}
              placeholder="Search keys"
            />
          </div>
          <div className="w-44 max-sm:min-w-[40%] max-sm:flex-1">
            <Select
              aria-label="Type"
              value={filters.type === 'all' ? '' : filters.type}
              onChange={(v) => setFilters({ ...filters, type: (v || 'all') as KeyType | 'all' })}
              emptyOption={`All types (${views.length})`}
              options={KEY_TYPES.map((t) => ({ value: t, label: `${TYPE_LABEL[t]} (${typeCount(t)})` }))}
            />
          </div>
          <div className="w-44 max-sm:min-w-[40%] max-sm:flex-1">
            <Select
              aria-label="Status"
              value={filters.statuses[0] ?? ''}
              onChange={(v) => setFilters({ ...filters, statuses: v ? [v as KeyStatus] : [] })}
              emptyOption="All statuses"
              options={KEY_STATUSES.map((s) => ({
                value: s,
                label: `${STATUS_LABEL[s]} (${statusCount(s)})`,
              }))}
            />
          </div>
          {teams.length > 1 ? (
            <div className="w-44 max-sm:min-w-[40%] max-sm:flex-1">
              <Combobox
                aria-label="Team"
                value={filters.team}
                onChange={(v) => setFilters({ ...filters, team: v })}
                emptyOption="All teams"
                options={teams.map((t) => ({ value: t.name, label: t.spec.display_name ?? t.name }))}
              />
            </div>
          ) : null}
          {views.length > 0 ? (
            <div className="w-44 max-sm:min-w-[40%] max-sm:flex-1">
              <Combobox
                aria-label="Owner"
                value={filters.owner}
                onChange={(v) => setFilters({ ...filters, owner: v })}
                emptyOption="All owners"
                options={[
                  { value: NO_OWNER, label: 'No owner' },
                  ...owners.map((o) => ({ value: o, label: o })),
                ]}
              />
            </div>
          ) : null}
          <StarredFilter kind="key" pressed={starred.on} onChange={starred.set} />
          {filtered ? (
            <Button size="sm" variant="ghost" onClick={clearAll}>
              Clear filters
            </Button>
          ) : null}
          <span className="ml-auto">
            <CountNote shown={visible.length} total={views.length} noun="keys" />
          </span>
        </FilterBar>
      ) : null}
      {view === 'keys' ? (
        <>
          {status === 'loading' && views.length === 0 ? <LoadingRows /> : null}
          {status === 'error' ? (
            <ErrorState error={error} subject="API keys" onRetry={() => void fetch()} />
          ) : null}
          {status === 'ready' && views.length === 0 ? (
            <EmptyState
              title="No API keys yet"
              description="Apps send requests with a key. You'll see its secret once."
              action={
                <Gate operation="createKey" mode="hide">
                  <EmptyAction onClick={() => setForm('new')}>Create key</EmptyAction>
                </Gate>
              }
            />
          ) : null}
          {views.length > 0 ? (
            <MasterDetail
              sticky
              narrow={{
                open: Boolean(selected && current),
                onClose: () => setSelected(null),
                title: current?.name ?? '',
              }}
              list={
                visible.length === 0 ? (
                  filtered ? (
                    <NoMatch onClear={clearAll} />
                  ) : null
                ) : (
                  <SelectableList
                    label="API keys"
                    items={visible}
                    keyOf={(v) => v.name}
                    selectedKey={current?.name ?? null}
                    onSelect={(name) => setSelected(name)}
                    toneOf={(v) => statusTone(v.status)}
                    renderItem={(v) => (
                      <KeyRow
                        view={v}
                        team={teamOf(v.project)}
                        starred={starredNames.has(v.name)}
                        budget={
                          budgets
                            ? tightestBudget(
                                v,
                                budgets,
                                teamOf(v.project),
                                v.key.metadata.organization ?? scopeOrg,
                              )
                            : null
                        }
                      />
                    )}
                  />
                )
              }
              detail={
                current ? (
                  <KeyDetail
                    key={current.name}
                    view={current}
                    team={teamOf(current.project)}
                    organization={scope.kind === 'organization' ? scope.name : undefined}
                    onEdit={() => setForm('edit')}
                    onSecret={(title, value) => setSecret({ title, value, created: false })}
                  />
                ) : missing ? (
                  <EmptyState
                    title="We couldn't find this key"
                    description="It may have been revoked, or the link has a mistake in it."
                  />
                ) : (
                  <EmptyState title="Select a key to see its details" />
                )
              }
            />
          ) : null}
          {views.length > 0 ? <KeyboardHints keys={['search', 'move', 'open', 'export', 'new']} /> : null}
        </>
      ) : null}
      {form ? (
        <KeyFormDrawer
          key={form === 'edit' ? current?.name : 'new'}
          open
          onClose={() => setForm(null)}
          editing={form === 'edit' ? current?.key : undefined}
          onCreated={(name, value) => {
            setSelected(name);
            setSecret({ title: 'Your new API key', value, created: true });
          }}
        />
      ) : null}
      <RevealOnceDialog
        secret={secret?.value ?? null}
        onClose={() => setSecret(null)}
        title={secret?.title ?? ''}
        label="API key"
        description={
          secret?.created
            ? "Copy it into your app's configuration now."
            : 'Update your apps with the new secret; the old one keeps working until the grace period ends.'
        }
      >
        <KeySnippets />
      </RevealOnceDialog>
    </>
  );
}

function exportKeys(views: KeyView[], teamOf: (project: string) => string | undefined) {
  downloadCsv(
    'api-keys.csv',
    [
      'name',
      'type',
      'status',
      'kid',
      'hint',
      'project',
      'team',
      'owner',
      'created_at',
      'expires_at',
      'last_used_at',
    ],
    views.map((v) => [
      v.name,
      v.type,
      v.status,
      v.key.metadata.kid,
      v.key.metadata.hint ?? '',
      v.project,
      teamOf(v.project) ?? '',
      v.owner ?? '',
      v.key.metadata.created_at,
      v.key.spec.expires_at ?? '',
      v.key.metadata.last_used_at ?? '',
    ]),
  );
}
