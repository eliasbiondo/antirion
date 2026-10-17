'use client';

import Link from 'next/link';
import { Fragment, type ReactNode, useEffect, useMemo, useState } from 'react';

import { useQueryParam, useScopedHref, useTabParam } from '@/components/layout/url-sync';
import { Button } from '@/components/ui/button';
import { Legend, Meter, TimeChart } from '@/components/ui/charts';
import { Segmented } from '@/components/ui/form';
import { useCan, usePermission } from '@/components/ui/gate';
import { Eyebrow, FilterBar, Hero, HeroCell, PageHeader, Stat, Tabs } from '@/components/ui/layout';
import { MenuButton, type MenuItem } from '@/components/ui/menu';
import { ModelMark } from '@/components/ui/marks';
import { UpdatedAt } from '@/components/ui/misc';
import { Banner, EmptyAction, EmptyState, ErrorState, LoadingRows, NoMatch } from '@/components/ui/states';
import { CountNote, Table, Th } from '@/components/ui/table';
import { downloadCsv } from '@/lib/csv';
import { useUrlFilters } from '@/lib/url-filters';
import { formatMoney, plural } from '@/lib/format';
import { usePageShortcut } from '@/lib/shortcuts';
import { formatDate } from '@/lib/time';
import { usePreferences } from '@/stores/preferences';
import { useScope } from '@/stores/scope';

import { budgetsApi } from '../api';
import type { Enforcement } from '../form';
import { useBudgetsData, useBudgetTree } from '../hooks';
import type { Person } from '../people';
import { useBudgets } from '../store';
import { type BudgetRow, flatten, isCounted, partitionLabel, STATE_LABEL, topIssue } from '../types';
import { ActivityTab, useWaivers } from './activity-tab';
import { BudgetDrawer } from './budget-drawer';
import { BudgetRows, matches, type StateFilter, useTreeFacts } from './budget-tree';
import { RecostDialog, ServiceTierDrawer } from './cost-tools';
import { OverageDialog } from './overage-dialog';
import { NotifyDialog, PeopleTab } from './people-tab';
import { type CountedOwner, counterGap, countedSeries } from '../sources';
import { TeamBurn, TopSpenders } from './team-burn';
import { LedgerTab } from '@/features/ledger/components/ledger-tab';
import { monthProjection } from '../projection';

const STATE_FILTERS: StateFilter[] = ['all', 'over', 'at_risk', 'on_pace'];

/** The budget list's state filter as a URL parameter, so a copied link keeps it. */
const BUDGET_FILTERS = { state: 'all' };

/** This month's request costs per day and adjustments per day of the counted budgets, 0-based. */
function useCountedDays(owners: CountedOwner[], days: number) {
  const scope = useScope((s) => s.scope);
  const organization = scope.kind === 'organization' ? scope.name : null;
  const key = owners.map((o) => `${o.level}/${o.owner}`).join(',');
  const [state, setState] = useState<{
    key: string;
    costs: number[];
    adjustments: number[];
    error: unknown;
  } | null>(null);
  useEffect(() => {
    if (!key) return;
    let cancelled = false;
    const now = new Date();
    const from = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1)).toISOString();
    const to = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1)).toISOString();
    const dayOf = (iso: string) => new Date(iso).getUTCDate() - 1;
    Promise.all([
      budgetsApi.countedDaily(from, to, owners, organization),
      budgetsApi.adjustmentsSince(from, owners),
    ])
      .then(([daily, adjusted]) => {
        if (cancelled) return;
        const costs = new Array<number>(days).fill(0);
        const adjustments = new Array<number>(days).fill(0);
        for (const d of daily) costs[dayOf(d.day)] = (costs[dayOf(d.day)] ?? 0) + d.cost;
        for (const a of adjusted) adjustments[dayOf(a.time)] = (adjustments[dayOf(a.time)] ?? 0) + a.amount;
        setState({ key, costs, adjustments, error: null });
      })
      .catch((error: unknown) => !cancelled && setState({ key, costs: [], adjustments: [], error }));
    return () => {
      cancelled = true;
    };
    // `owners` is keyed by `key`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, days, organization]);
  return state?.key === key ? state : null;
}

/**
 * The counted budgets' spend this month, as the hero counts it: the request costs charged to them
 * and the spend adjustments made on them, as a running total, with the projection to the counters'
 * own month-end figure.
 */
function DailySpend({
  total,
  counted,
  projectedEnd,
  owners,
}: {
  total: number | null;
  counted: number;
  projectedEnd: number;
  owners: CountedOwner[];
}) {
  const now = new Date();
  const days = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 0)).getUTCDate();
  const today = now.getUTCDate();
  const data = useCountedDays(owners, days);
  const labels = Array.from(
    { length: days },
    (_, i) => `${now.toLocaleString('en-US', { month: 'short', timeZone: 'UTC' })} ${i + 1}`,
  );
  if (data?.error) return <ErrorState error={data.error} subject="Daily spend" size="sm" />;
  const series = countedSeries(data?.costs ?? [], data?.adjustments ?? [], days, today, projectedEnd);
  const gap = data ? counterGap(counted, series.spent) : null;
  const adjusted = data?.adjustments.some((a) => a !== 0) ?? false;
  return (
    <>
      <TimeChart
        kind="line"
        gaps
        title="Spend on the counted budgets this month, with the projection"
        labels={labels}
        height={170}
        formatValue={(v) => formatMoney(v, { compact: true })}
        series={[
          { name: 'Actual', values: series.actual, tone: 'accent' },
          { name: 'Projected', values: series.projected, tone: 'warn', dashed: true },
        ]}
        reference={
          total ? { value: total, label: `Budget ${formatMoney(total, { compact: true })}` } : undefined
        }
        marker={{ index: today - 1, label: 'Today' }}
      />
      <Legend
        items={[
          { label: 'Actual', tone: 'accent' },
          { label: 'Projected', tone: 'warn', dashed: true },
        ]}
      />
      {adjusted ? (
        <p className="mt-2 text-caption text-muted">
          Steps are spend adjustments made on the budgets, counted on the day they were made.
        </p>
      ) : null}
      {gap !== null ? (
        <p className="mt-2 text-caption text-muted">
          The budget counters hold{' '}
          <span className="mono text-fg">{formatMoney(Math.abs(gap), { compact: true })}</span>{' '}
          {gap > 0 ? 'more' : 'less'} than this chart shows for them.
        </p>
      ) : null}
    </>
  );
}

/**
 * The top issue's details: its burn rate and run-out day, its largest Model this month, and the
 * requests budgets refused for its owner in the last hour; with approve, raise, downgrade and the
 * matching requests.
 */
function IssueBanner({
  issue,
  onEdit,
  onApprove,
}: {
  issue: BudgetRow;
  onEdit: (row: BudgetRow, enforcement?: Enforcement) => void;
  onApprove: (row: BudgetRow) => void;
}) {
  const timeZone = usePreferences((s) => s.timeZone);
  const modelsUsed = useBudgets((s) => s.modelsUsed.data);
  const canGrant = useCan('grantBudgetOverage');
  const subject = `${issue.level}/${issue.owner}`;
  const [refusals, setRefusals] = useState<{ subject: string; count: number | null } | null>(null);
  useEffect(() => {
    let cancelled = false;
    budgetsApi
      .refusedLastHour(issue.level, issue.owner)
      .then((count) => !cancelled && setRefusals({ subject: `${issue.level}/${issue.owner}`, count }))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [issue.level, issue.owner]);
  const refused = refusals?.subject === subject ? refusals.count : null;
  const largest = [...(modelsUsed?.get(`${issue.level}/${issue.owner}`)?.entries() ?? [])].sort(
    (a, b) => b[1].cost - a[1].cost,
  )[0];
  const logsQuery: Record<string, string> =
    issue.level === 'team' || issue.level === 'key' ? { [issue.level]: issue.owner } : {};
  if (issue.partition?.attr === 'user' && issue.partition.value) logsQuery.q = issue.partition.value;
  const logsHref = useScopedHref('/request-log/', logsQuery);
  const name = issue.partition ? `${partitionLabel(issue.partition)} (${issue.owner})` : issue.owner;
  const details: ReactNode[] = [];
  if (issue.ratePerDay !== null) details.push(`burning ${formatMoney(issue.ratePerDay)} a day`);
  if (issue.runsOutAt) details.push(`runs out ${formatDate(issue.runsOutAt, timeZone)}`);
  // Model costs come from requests; a budget filled by adjustments has none worth naming.
  if (largest && largest[1].cost > 0)
    details.push(
      <>
        largest Model{' '}
        <span className="inline-flex items-center gap-1 align-middle">
          <ModelMark model={largest[0]} size="xs" />
          <span className="mono text-fg">{largest[0]}</span>
        </span>{' '}
        ({formatMoney(largest[1].cost)} this month)
      </>,
    );
  if (refused !== null) details.push(`${plural(refused, 'request')} refused in the last hour`);
  const approvable = canGrant && (issue.state === 'blocked' || issue.counter.mode === 'approval');
  return (
    <Banner
      tone={issue.state === 'at_risk' ? 'warn' : 'error'}
      title={
        issue.state === 'blocked'
          ? `${name} is blocked`
          : issue.state === 'over'
            ? `${name} is over budget`
            : issue.rolling
              ? `${name} is near its limit`
              : `${name} may run out before the period ends`
      }
      actions={
        <span className="flex flex-wrap gap-2">
          {approvable ? (
            <Button variant="secondary" onClick={() => onApprove(issue)}>
              Approve overage
            </Button>
          ) : null}
          <Button variant="secondary" onClick={() => onEdit(issue)}>
            Raise amount
          </Button>
          {issue.kind === 'budget' ? (
            <MenuButton
              label="More"
              items={[
                {
                  label: 'Downgrade to a cheaper Model',
                  icon: 'arrowDown',
                  onSelect: () => onEdit(issue, 'downgrade'),
                },
              ]}
            />
          ) : null}
        </span>
      }
    >
      <span className="mono">{issue.name}</span>: {formatMoney(issue.spent)} of {formatMoney(issue.amount)}
      {issue.rolling ? ' in its window. ' : `, projected ${formatMoney(issue.projected)}. `}
      {issue.state === 'blocked'
        ? issue.rolling
          ? 'New requests are refused until older spend leaves the window, the amount goes up or going over is approved.'
          : 'New requests are refused until the period resets, the amount goes up or going over is approved.'
        : issue.state === 'over'
          ? 'This budget only alerts, so requests still go through.'
          : issue.rolling
            ? 'Spend in the window has reached its alert threshold.'
            : 'On the current pace it runs out early.'}
      {details.length > 0 ? (
        <span className="mt-1 block text-caption text-muted">
          {details.map((d, i) => (
            <Fragment key={i}>{d} · </Fragment>
          ))}
          <Link href={logsHref} className="text-accent hover:underline">
            View requests
          </Link>
        </span>
      ) : null}
    </Banner>
  );
}

type View = 'budgets' | 'people' | 'activity' | 'ledger';

/** Budgets: the scope's month, the tree of budgets with forecasts, the top issue, people and the activity. */
export function BudgetsScreen() {
  useBudgetsData();
  const counters = useBudgets((s) => s.counters);
  const monthScope = useScope((s) => s.scope);
  const events = useBudgets((s) => s.events);
  const overage = useBudgets((s) => s.overage);
  const load = useBudgets((s) => s.load);
  const tree = useBudgetTree();
  const facts = useTreeFacts(tree);
  const waivers = useWaivers();
  const canRecost = useCan('recostSpend');
  const canSeeBudgets = usePermission('policies:read');
  // A budget is written on the policies of a team, a project or the gateway.
  const canWritePolicies = usePermission('policies:write');
  const canWriteTeams = usePermission('teams:update');
  const canWriteProjects = usePermission('projects:update');
  const canCreate = canWritePolicies || canWriteTeams || canWriteProjects;
  const canReadPrices = useCan('getPricingSettings');
  const [tab, setTab] = useTabParam();
  const url = useUrlFilters(BUDGET_FILTERS);
  const filter: StateFilter = STATE_FILTERS.includes(url.filters.state as StateFilter)
    ? (url.filters.state as StateFilter)
    : 'all';
  const setFilter = (state: StateFilter) => url.set({ state });
  const [open, setOpen] = useState(new Set<string>());
  const [opened, setOpened] = useState<{ row: BudgetRow | null; enforcement?: Enforcement } | null>(null);
  // An open budget is in the URL (`budget`), so its link can be copied and reopens it.
  const [budgetParam, setBudgetParam] = useQueryParam('budget');
  const [approving, setApproving] = useState<BudgetRow | null>(null);
  const [notifying, setNotifying] = useState<{ person: Person; row: BudgetRow } | null>(null);
  const [tool, setTool] = useState<'recost' | 'tiers' | null>(null);
  const all = useMemo(() => flatten(tree).filter(isCounted), [tree]);
  const linked = budgetParam ? (facts.rows.get(budgetParam) ?? null) : null;
  const editing = opened ?? (linked ? { row: linked } : null);
  const setEditing = (next: { row: BudgetRow | null; enforcement?: Enforcement } | null) => {
    setOpened(next);
    const id = next?.row?.id ?? null;
    if (id !== budgetParam) setBudgetParam(id);
  };
  const count = (f: StateFilter) => all.filter((r) => matches(r, f)).length;
  const issue = topIssue(tree);
  const top = tree.find((r) => r.level === 'organization' && r.kind === 'budget') ?? null;
  const roots = tree.filter(isCounted);
  const total = top ? top.amount : roots.reduce((a, r) => a + r.amount, 0) || null;
  const now = new Date();
  // The one month-end rule Overview and Analytics use: the counted budgets' own forecasts.
  const figures = monthProjection(counters.data, monthScope, null, now.getTime());
  const spent = figures?.spent ?? (top ? top.spent : roots.reduce((a, r) => a + r.spent, 0));
  const projected = figures?.projected ?? (top ? top.projected : roots.reduce((a, r) => a + r.projected, 0));
  const day = now.getUTCDate();
  const days = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 0)).getUTCDate();
  const month = now.toLocaleString('en-US', { month: 'long', timeZone: 'UTC' });
  const exhaustedToday = events.data
    ? events.data.filter(
        (e) => e.kind === 'exhausted' && new Date(e.time).toDateString() === now.toDateString(),
      ).length
    : null;
  const downgraded = all.reduce((a, r) => a + (r.counter.downgraded_requests ?? 0), 0);
  const waiting = (overage.data?.length ?? 0) + waivers.length;
  const canReadLedger = useCan('listLedgerLines');
  const view: View =
    tab === 'activity' || tab === 'people' || (tab === 'ledger' && canReadLedger) ? tab : 'budgets';

  const toggle = (id: string) =>
    setOpen((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const exportTree = () =>
    downloadCsv(
      'budgets.csv',
      [
        'level',
        'owner',
        'budget',
        'partition',
        'mode',
        'spent_usd',
        'amount_usd',
        'projected_usd',
        'state',
        'budget_owner',
      ],
      all.map((r) => [
        r.level,
        r.owner,
        r.name,
        r.counter.partition ?? '',
        r.counter.mode,
        r.spent.toFixed(2),
        r.amount.toFixed(2),
        r.projected.toFixed(2),
        STATE_LABEL[r.state],
        r.budgetOwner ?? '',
      ]),
    );
  usePageShortcut('e', exportTree);
  usePageShortcut('n', () => setEditing({ row: null }));

  const tools: MenuItem[] = [];
  if (canRecost) tools.push({ label: 'Re-cost spend', icon: 'refresh', onSelect: () => setTool('recost') });
  if (canReadPrices)
    tools.push({ label: 'Prices by service tier', icon: 'coins', onSelect: () => setTool('tiers') });
  const actions = {
    onEdit: (row: BudgetRow) => setEditing({ row }),
    onApprove: setApproving,
    onNotify: (person: Person, row: BudgetRow) => setNotifying({ person, row }),
  };
  const editingParent = editing?.row?.parentId ? facts.rows.get(editing.row.parentId) : undefined;

  return (
    <>
      <PageHeader
        title="Budgets"
        description="Spending limits, and how this period is tracking"
        context={`Day ${day} of ${days} in ${month}`}
        actions={
          <>
            <UpdatedAt at={counters.at} />
            {all.length > 0 ? (
              <Button icon="download" variant="ghost" onClick={exportTree}>
                Export CSV
              </Button>
            ) : null}
            {tools.length > 0 ? <MenuButton items={tools} label="Cost tools" variant="ghost" /> : null}
            {canCreate ? (
              <Button icon="plus" variant="primary" onClick={() => setEditing({ row: null })}>
                Create budget
              </Button>
            ) : null}
          </>
        }
      />
      {all.length > 0 ? (
        <Hero className="grid-cols-[minmax(0,1fr)_minmax(0,1.6fr)] max-lg:grid-cols-1">
          <HeroCell className="flex flex-col gap-4 bg-[linear-gradient(135deg,color-mix(in_oklab,var(--ant-warn)_8%,transparent),transparent_60%)] max-sm:px-gutter-sm">
            <Eyebrow tone={total && projected > total ? 'error' : 'success'}>
              Projected spend at period end{top ? ` · ${top.owner}` : ''}
            </Eyebrow>
            <div className="flex flex-wrap items-baseline gap-2">
              <span className="mono text-hero leading-none font-semibold tracking-tighter text-fg max-sm:text-[44px]">
                {formatMoney(projected, { compact: true })}
              </span>
              {total ? (
                <span className="mono text-body text-muted">of {formatMoney(total, { compact: true })}</span>
              ) : null}
            </div>
            <p className="text-body text-muted">
              <span className="mono font-semibold text-fg">{formatMoney(spent, { compact: true })}</span>{' '}
              spent so far on the budget counters
              {total ? (
                <>
                  {' · '}
                  <span className={projected > total ? 'text-error' : 'text-success'}>
                    {projected > total
                      ? `${formatMoney(projected - total, { compact: true })} over on the current pace`
                      : `${formatMoney(total - projected, { compact: true })} to spare`}
                  </span>
                </>
              ) : null}
            </p>
            {total ? (
              <Meter
                value={spent}
                max={total}
                projected={projected}
                tick={day / days}
                tone={projected > total ? 'error' : 'accent'}
                label="Spent and projected against the budget"
                className="h-2.5"
              />
            ) : null}
            <div className="grid grid-cols-3 gap-4 border-t border-line pt-4">
              <Stat
                label="Over budget"
                value={count('over')}
                tone={count('over') > 0 ? 'error' : 'neutral'}
                sub={`${count('at_risk')} at risk · ${count('on_pace')} on track`}
              />
              {exhaustedToday !== null ? (
                <Stat
                  label="Limits reached today"
                  value={exhaustedToday}
                  sub={
                    downgraded > 0
                      ? `${plural(downgraded, 'request')} switched to a cheaper Model`
                      : 'budgets that hit their limit'
                  }
                />
              ) : null}
              <Stat label="Days left" value={days - day} sub={`until ${month} ends`} />
            </div>
          </HeroCell>
          <HeroCell className="max-sm:px-gutter-sm">
            <div className="mb-2 flex items-baseline gap-2">
              <h2 className="text-body font-semibold text-fg">Spend this month</h2>
              <span className="text-caption text-muted">
                What the budget counters count, running total, with the projection to month end
              </span>
            </div>
            <DailySpend
              total={total}
              counted={spent}
              projectedEnd={projected}
              owners={
                top
                  ? [{ level: top.level, owner: top.owner }]
                  : roots.map((r) => ({ level: r.level, owner: r.owner }))
              }
            />
          </HeroCell>
        </Hero>
      ) : null}
      {issue ? (
        <IssueBanner
          issue={issue}
          onEdit={(row, enforcement) => setEditing({ row, enforcement })}
          onApprove={setApproving}
        />
      ) : null}
      <Tabs<View>
        label="Budget views"
        value={view}
        onChange={(v) => setTab(v === 'budgets' ? null : v)}
        items={[
          {
            value: 'budgets',
            label: 'Budgets',
            count: counters.data ? all.length : undefined,
            icon: 'dollar',
          },
          { value: 'people', label: 'People', icon: 'users' },
          { value: 'activity', label: 'Activity', count: waiting > 0 ? waiting : undefined, icon: 'clock' },
          ...(canReadLedger ? [{ value: 'ledger' as const, label: 'Ledger', icon: 'receipt' as const }] : []),
        ]}
      />
      {view === 'ledger' ? <LedgerTab /> : null}
      {view === 'activity' ? <ActivityTab /> : null}
      {view === 'people' ? <PeopleTab /> : null}
      {view === 'budgets' ? (
        <>
          {all.length > 0 ? (
            <FilterBar>
              <Segmented<StateFilter>
                label="Budget status"
                value={filter}
                onChange={setFilter}
                className="max-w-full overflow-x-auto"
                options={[
                  { value: 'all', label: 'All', count: all.length },
                  { value: 'over', label: 'Over budget', count: count('over') },
                  { value: 'at_risk', label: 'At risk', count: count('at_risk') },
                  { value: 'on_pace', label: 'On track', count: count('on_pace') },
                ]}
              />
              <span className="ml-auto">
                <CountNote shown={count(filter)} total={all.length} noun="budgets" />
              </span>
            </FilterBar>
          ) : null}
          {counters.status === 'loading' && !counters.data ? <LoadingRows /> : null}
          {counters.error ? (
            <ErrorState error={counters.error} subject="Budgets" onRetry={() => void load()} />
          ) : null}
          {counters.data && all.length === 0 ? (
            canSeeBudgets ? (
              <EmptyState
                title="No budgets yet"
                description="Set a spending limit for a team, project or key."
                action={
                  canCreate ? (
                    <EmptyAction onClick={() => setEditing({ row: null })}>Create budget</EmptyAction>
                  ) : undefined
                }
              />
            ) : (
              <EmptyState
                title="Your role doesn't include budgets"
                description="The ledger still lists what your requests cost. Ask a gateway admin for access."
              />
            )
          ) : null}
          {all.length > 0 && count(filter) === 0 ? <NoMatch onClear={() => setFilter('all')} /> : null}
          {all.length > 0 && count(filter) > 0 ? (
            <Table label="Budgets" className="border-b border-line bg-raised">
              <thead>
                <tr>
                  <Th>Budget</Th>
                  <Th>Status</Th>
                  <Th numeric>Spent</Th>
                  <Th>Forecast to period end</Th>
                  <Th numeric>Projected</Th>
                  <Th>
                    <span className="sr-only">Actions</span>
                  </Th>
                </tr>
              </thead>
              <tbody>
                <BudgetRows
                  rows={tree}
                  depth={0}
                  filter={filter}
                  open={open}
                  toggle={toggle}
                  facts={facts}
                  actions={actions}
                />
              </tbody>
            </Table>
          ) : null}
          {all.length > 0 ? (
            <div className="grid grid-cols-1 gap-px border-b border-line bg-line xl:grid-cols-[minmax(0,1.6fr)_minmax(0,1fr)]">
              <TeamBurn tree={tree} />
              <TopSpenders onOpen={() => setTab('people')} />
            </div>
          ) : null}
        </>
      ) : null}
      {editing ? (
        <BudgetDrawer
          key={editing.row ? `${editing.row.id}/${editing.enforcement ?? ''}` : 'new'}
          row={editing.row}
          parent={editingParent}
          enforcement={editing.enforcement}
          onClose={() => setEditing(null)}
        />
      ) : null}
      {approving ? (
        <OverageDialog
          counter={approving.counter}
          label={`${approving.partition ? `${partitionLabel(approving.partition)} · ` : ''}${approving.owner} · ${approving.name}`}
          open
          onClose={() => setApproving(null)}
        />
      ) : null}
      <NotifyDialog
        person={notifying?.person ?? null}
        row={notifying?.row ?? null}
        onClose={() => setNotifying(null)}
      />
      <RecostDialog open={tool === 'recost'} onClose={() => setTool(null)} />
      {tool === 'tiers' ? <ServiceTierDrawer open onClose={() => setTool(null)} /> : null}
    </>
  );
}
